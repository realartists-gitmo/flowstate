use std::{
  collections::HashMap,
  sync::{Mutex, OnceLock},
  thread,
  time::Duration,
};

use anyhow::{Context as _, Result};
use iroh::{Endpoint, EndpointAddr, Watcher as _, endpoint::presets};
use iroh_gossip::net::Gossip;
use tokio::time::timeout;

use crate::{
  SessionId,
  admission::SessionAdmission,
  proto_direct::{AssetBytes, DirectRequest, DiscoveryAdmissionGrant},
  ticket::SessionTicket,
};

use super::{
  MintedTicket, NetCommand, NetEvent, TicketSeed, auth,
  direct::{self, DirectProto, DirectServeState},
  swarm::SwarmHandle,
};

pub type CommandSender = async_channel::Sender<NetCommand>;
pub type EventReceiver = async_channel::Receiver<NetEvent>;

const DIRECT_PULL_TIMEOUT: Duration = Duration::from_secs(10);
const ENDPOINT_ADDR_READY_TIMEOUT: Duration = Duration::from_secs(2);
/// FS-074: the command and event channels are bounded so a stalled network
/// thread or UI event pump applies backpressure instead of growing without
/// limit. Command producers use `try_send` and tolerate a full channel; event
/// producers coalesce or backpressure per `swarm::forward_event`.
const RUNTIME_COMMAND_CAPACITY: usize = 1024;
const RUNTIME_EVENT_CAPACITY: usize = 1024;
static RUNTIME: OnceLock<Mutex<Option<RuntimeBridge>>> = OnceLock::new();

fn unix_now() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or(Duration::ZERO)
    .as_secs()
}

#[derive(Clone)]
struct RuntimeBridge {
  commands: CommandSender,
  events: EventReceiver,
}

impl RuntimeBridge {
  fn is_alive(&self) -> bool {
    !self.commands.is_closed()
  }

  fn handles(&self) -> (CommandSender, EventReceiver) {
    (self.commands.clone(), self.events.clone())
  }
}

pub fn start() -> Result<(CommandSender, EventReceiver)> {
  let runtime = RUNTIME.get_or_init(|| Mutex::new(None));
  let mut runtime = runtime.lock().expect("collaboration runtime lock poisoned");
  if let Some(runtime) = runtime.as_ref().filter(|runtime| runtime.is_alive()) {
    tracing::debug!("reusing existing collaboration network runtime");
    return Ok(runtime.handles());
  }

  tracing::info!("starting collaboration network runtime");
  let next_runtime = spawn_runtime()?;
  let handles = next_runtime.handles();
  *runtime = Some(next_runtime);
  drop(runtime);
  Ok(handles)
}

fn spawn_runtime() -> Result<RuntimeBridge> {
  let (cmd_tx, cmd_rx) = async_channel::bounded(RUNTIME_COMMAND_CAPACITY);
  let (evt_tx, evt_rx) = async_channel::bounded(RUNTIME_EVENT_CAPACITY);
  let thread_evt_tx = evt_tx.clone();

  thread::Builder::new()
    .name("flowstate-collab-net".into())
    .spawn(move || {
      tracing::debug!("collaboration network thread started");
      let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
      {
        Ok(runtime) => runtime,
        Err(error) => {
          let _ = thread_evt_tx.try_send(NetEvent::EndpointOnline(false));
          tracing::error!(error = %format_args!("{error:#}"), "collaboration tokio runtime failed");
          return;
        },
      };

      if let Err(error) = runtime.block_on(net_main(cmd_rx, evt_tx)) {
        let _ = thread_evt_tx.try_send(NetEvent::EndpointOnline(false));
        tracing::error!(error = %format_args!("{error:#}"), "collaboration networking stopped unexpectedly");
      }
      tracing::debug!("collaboration network thread stopped");
    })
    .context("spawning collaboration networking thread failed")?;

  Ok(RuntimeBridge {
    commands: cmd_tx,
    events: evt_rx,
  })
}

async fn net_main(cmd_rx: async_channel::Receiver<NetCommand>, evt_tx: async_channel::Sender<NetEvent>) -> Result<()> {
  tracing::info!("binding collaboration endpoint");
  let endpoint = Endpoint::builder(presets::N0)
    .bind()
    .await
    .context("binding collaboration endpoint failed")?;
  let bound_addr = endpoint_addr(&endpoint);
  tracing::info!(peer = %bound_addr.id, "collaboration endpoint bound");
  direct::install_endpoint(endpoint.clone());

  let gossip = Gossip::builder().spawn(endpoint.clone());
  let direct_state = DirectServeState::default();
  let _router = iroh::protocol::Router::builder(endpoint.clone())
    .accept(iroh_gossip::ALPN, gossip.clone())
    .accept(crate::DIRECT_ALPN, DirectProto::new(direct_state.clone()))
    .spawn();
  tracing::info!(peer = %bound_addr.id, "collaboration direct and gossip protocols installed");

  let _ = evt_tx.send(NetEvent::EndpointOnline(true)).await;
  let mut swarms = HashMap::new();

  while let Ok(command) = cmd_rx.recv().await {
    tracing::trace!(command = command.kind(), "collaboration network command received");
    match command {
      NetCommand::EnsureUp => {
        tracing::debug!("collaboration endpoint ensure-up requested");
        let _ = evt_tx.send(NetEvent::EndpointOnline(true)).await;
      },
      NetCommand::RegisterDirectHandler { session, handler } => {
        tracing::debug!(%session, "registering collaboration direct handler");
        direct_state.register_handler(session, handler).await;
      },
      NetCommand::ConfigureStandingAccess {
        document,
        session,
        document_fingerprint,
        title,
        identities,
      } => {
        direct_state
          .configure_standing_access(session, document_fingerprint, title, document, identities.into_iter().collect())
          .await;
      },
      NetCommand::RequestDiscoveredTicket {
        advertisement,
        request,
        reply,
      } => {
        let result = async {
          advertisement.verify(unix_now())?;
          anyhow::ensure!(
            request.session == advertisement.session,
            "discovery request session does not match advertisement"
          );
          anyhow::ensure!(
            request.document_fingerprint == advertisement.document_fingerprint,
            "discovery request document does not match advertisement"
          );
          let bytes = direct::pull_with_fallback(
            DirectRequest::RequestAdmission { request: *request },
            vec![advertisement.endpoint.id],
            DIRECT_PULL_TIMEOUT,
          )
          .await?;
          let grant: DiscoveryAdmissionGrant = postcard::from_bytes(&bytes).context("decoding discovery admission grant")?;
          Ok(SessionTicket::new(
            advertisement.session,
            vec![advertisement.endpoint],
            grant.title,
            grant.admission,
            grant.document,
          ))
        }
        .await;
        let _ = reply.send(result).await;
      },
      NetCommand::CreateSession { session, reply } => {
        let result = create_session(&mut swarms, &endpoint, &gossip, &direct_state, &evt_tx, session).await;
        if let Err(error) = &result {
          tracing::warn!(%session, error = %format_args!("{error:#}"), "collaboration create session failed");
        }
        let _ = reply.send(result).await;
      },
      NetCommand::JoinSession {
        session,
        bootstrap,
        admission,
      } => {
        if let Err(error) = join_session(&mut swarms, &endpoint, &gossip, &direct_state, &evt_tx, session, bootstrap, admission).await {
          tracing::warn!(%session, error = %format_args!("{error:#}"), "collaboration join session failed");
          let _ = evt_tx
            .send(NetEvent::SubscribeFailed {
              session,
              error: format!("{error:#}"),
            })
            .await;
        }
      },
      NetCommand::LeaveSession { session } => {
        tracing::info!(%session, "leaving collaboration network session");
        if let Some(handle) = swarms.remove(&session) {
          handle.stop().await;
        } else {
          tracing::debug!(%session, "leave requested for missing collaboration swarm");
        }
        direct_state.detach_session(session).await;
        // Forget the bearer admission so a later rejoin must install it again.
        direct_state.auth().remove_session(session);
        auth::clear_local_admission(session);
      },
      NetCommand::Publish { session, payload } => {
        let payload_kind = payload.kind();
        let payload_bytes = payload.byte_len();
        if let Some(handle) = swarms.get(&session) {
          tracing::trace!(%session, payload_kind, payload_bytes, "publishing collaboration payload");
          if let Err(error) = handle.publish(payload).await {
            tracing::warn!(%session, payload_kind, payload_bytes, error = %format_args!("{error:#}"), "collaboration publish failed");
          }
        } else {
          tracing::warn!(%session, payload_kind, payload_bytes, "dropped collaboration publish for missing swarm");
        }
      },
      NetCommand::PullUpdates {
        session,
        candidates,
        our_vv,
        reply,
      } => {
        let candidate_count = candidates.len();
        let vv_bytes = our_vv.len();
        tracing::debug!(%session, candidate_count, vv_bytes, "pulling collaboration updates");
        let result = direct::pull_with_fallback(DirectRequest::Updates { session, have_vv: our_vv }, candidates, DIRECT_PULL_TIMEOUT).await;
        log_direct_pull_result("updates", session, candidate_count, &result);
        let _ = reply.send(result).await;
      },
      NetCommand::PullSnapshot {
        session,
        candidates,
        progress,
        reply,
      } => {
        let candidate_count = candidates.len();
        tracing::debug!(%session, candidate_count, "pulling collaboration snapshot");
        let result = direct::pull_with_fallback_progress(DirectRequest::Snapshot { session }, candidates, DIRECT_PULL_TIMEOUT, progress).await;
        log_direct_pull_result("snapshot", session, candidate_count, &result);
        let _ = reply.send(result).await;
      },
      NetCommand::PullBlob {
        session,
        candidates,
        blob,
        reply,
      } => {
        let candidate_count = candidates.len();
        tracing::debug!(%session, ?blob, candidate_count, "pulling collaboration blob");
        let result = direct::pull_with_fallback(DirectRequest::Blob { session, blob }, candidates, DIRECT_PULL_TIMEOUT).await;
        log_direct_pull_result("blob", session, candidate_count, &result);
        let _ = reply.send(result).await;
      },
      NetCommand::PullAsset {
        session,
        candidates,
        asset,
        reply,
      } => {
        let candidate_count = candidates.len();
        tracing::debug!(%session, asset, candidate_count, "pulling collaboration asset");
        let result = direct::pull_with_fallback(DirectRequest::Asset { session, asset }, candidates, DIRECT_PULL_TIMEOUT)
          .await
          .map(|bytes| AssetBytes { bytes });
        match &result {
          Ok(bytes) => tracing::debug!(%session, asset, candidate_count, bytes = bytes.bytes.len(), "collaboration asset pull succeeded"),
          Err(error) => tracing::warn!(%session, asset, candidate_count, error = %format_args!("{error:#}"), "collaboration asset pull failed"),
        }
        let _ = reply.send(result).await;
      },
      NetCommand::MintTicket { session, reply } => {
        // Every authenticated editor carries the same ephemeral admission
        // secret, so any live participant can mint an equivalent invite.
        let inviter = reachable_endpoint_addr(&endpoint).await;
        let result = direct_state
          .auth()
          .admission(session)
          .map(|admission| MintedTicket { inviter, admission })
          .ok_or_else(|| anyhow::anyhow!("collaboration session admission is unavailable"));
        let _ = reply.send(result).await;
      },
      NetCommand::MintTicketAddr { reply } => {
        let addr = reachable_endpoint_addr(&endpoint).await;
        tracing::debug!(peer = %addr.id, "minting collaboration ticket address");
        let _ = reply.send(addr).await;
      },
      NetCommand::Shutdown => {
        tracing::info!(open_sessions = swarms.len(), "shutting down collaboration network runtime");
        break;
      },
    }
  }

  for (_, handle) in swarms {
    tracing::debug!(session = %handle.session, "stopping collaboration swarm during shutdown");
    handle.stop().await;
  }
  endpoint.close().await;
  tracing::info!(peer = %bound_addr.id, "collaboration endpoint closed");
  Ok(())
}

async fn create_session(
  swarms: &mut HashMap<SessionId, SwarmHandle>,
  endpoint: &Endpoint,
  gossip: &Gossip,
  direct_state: &DirectServeState,
  evt_tx: &async_channel::Sender<NetEvent>,
  session: SessionId,
) -> Result<TicketSeed> {
  tracing::info!(%session, "creating collaboration network session");
  direct_state.attach_session(session).await;
  let admission = SessionAdmission::generate();
  direct_state
    .auth()
    .configure_session(session, admission.clone());
  auth::install_local_admission(session, admission.clone());
  replace_swarm(
    swarms,
    SwarmHandle::spawn(
      endpoint.clone(),
      gossip.clone(),
      direct_state.clone(),
      session,
      Vec::new(),
      evt_tx.clone(),
    )?,
  )
  .await;
  let inviter = reachable_endpoint_addr(endpoint).await;
  tracing::debug!(%session, peer = %inviter.id, "collaboration network session created");
  Ok(TicketSeed { inviter, admission })
}

async fn join_session(
  swarms: &mut HashMap<SessionId, SwarmHandle>,
  endpoint: &Endpoint,
  gossip: &Gossip,
  direct_state: &DirectServeState,
  evt_tx: &async_channel::Sender<NetEvent>,
  session: SessionId,
  bootstrap: Vec<EndpointAddr>,
  admission: SessionAdmission,
) -> Result<()> {
  let bootstrap_count = bootstrap.len();
  tracing::info!(%session, bootstrap_count, "joining collaboration network session");
  direct_state.attach_session(session).await;
  direct_state
    .auth()
    .configure_session(session, admission.clone());
  auth::install_local_admission(session, admission);
  replace_swarm(
    swarms,
    SwarmHandle::spawn(endpoint.clone(), gossip.clone(), direct_state.clone(), session, bootstrap, evt_tx.clone())?,
  )
  .await;
  tracing::debug!(%session, bootstrap_count, "collaboration network join subscribed");
  Ok(())
}

async fn replace_swarm(swarms: &mut HashMap<crate::SessionId, SwarmHandle>, handle: SwarmHandle) {
  let session = handle.session;
  if let Some(previous) = swarms.insert(session, handle) {
    tracing::debug!(%session, "replacing existing collaboration swarm");
    previous.stop().await;
  }
}

fn endpoint_addr(endpoint: &Endpoint) -> EndpointAddr {
  endpoint.addr()
}

fn log_direct_pull_result(kind: &'static str, session: crate::SessionId, candidate_count: usize, result: &Result<Vec<u8>>) {
  match result {
    Ok(bytes) => tracing::debug!(%session, kind, candidate_count, bytes = bytes.len(), "collaboration direct pull succeeded"),
    Err(error) => tracing::warn!(%session, kind, candidate_count, error = %format_args!("{error:#}"), "collaboration direct pull failed"),
  }
}

async fn reachable_endpoint_addr(endpoint: &Endpoint) -> EndpointAddr {
  let mut addr_watcher = endpoint.watch_addr();
  let wait = async {
    loop {
      if !addr_watcher.get().is_empty() {
        return;
      }
      tokio::select! {
        () = endpoint.online() => return,
        updated = addr_watcher.updated() => {
          if updated.is_err() {
            return;
          }
        },
      }
    }
  };
  let _ = timeout(ENDPOINT_ADDR_READY_TIMEOUT, wait).await;
  endpoint_addr(endpoint)
}
