use std::{collections::HashMap, sync::OnceLock, thread, time::Duration};

use anyhow::{Context as _, Result};
use iroh::{Endpoint, EndpointAddr, endpoint::presets};
use iroh_gossip::net::Gossip;

use crate::proto_direct::{AssetBytes, DirectRequest};

use super::{
  NetCommand, NetEvent, TicketSeed,
  direct::{self, DirectProto, DirectServeState},
  swarm::SwarmHandle,
};

pub type CommandSender = async_channel::Sender<NetCommand>;
pub type EventReceiver = async_channel::Receiver<NetEvent>;

const DIRECT_PULL_TIMEOUT: Duration = Duration::from_secs(10);
static RUNTIME: OnceLock<RuntimeBridge> = OnceLock::new();

#[derive(Clone)]
struct RuntimeBridge {
  commands: CommandSender,
  events: EventReceiver,
}

pub fn start() -> Result<(CommandSender, EventReceiver)> {
  if let Some(runtime) = RUNTIME.get() {
    tracing::debug!("reusing existing collaboration network runtime");
    return Ok((runtime.commands.clone(), runtime.events.clone()));
  }

  tracing::info!("starting collaboration network runtime");
  let runtime = spawn_runtime()?;
  let set_failed = RUNTIME.set(runtime.clone()).is_err();
  if set_failed && let Some(runtime) = RUNTIME.get() {
    tracing::debug!("another caller initialized collaboration network runtime first");
    return Ok((runtime.commands.clone(), runtime.events.clone()));
  }
  Ok((runtime.commands, runtime.events))
}

fn spawn_runtime() -> Result<RuntimeBridge> {
  let (cmd_tx, cmd_rx) = async_channel::unbounded();
  let (evt_tx, evt_rx) = async_channel::unbounded();
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
      NetCommand::CreateSession { session, reply } => {
        tracing::info!(%session, "creating collaboration network session");
        direct_state.attach_session(session).await;
        replace_swarm(
          &mut swarms,
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
        tracing::debug!(%session, peer = %bound_addr.id, "collaboration network session created");
        let _ = reply
          .send(Ok(TicketSeed {
            inviter: bound_addr.clone(),
          }))
          .await;
      },
      NetCommand::JoinSession { session, bootstrap } => {
        let bootstrap_count = bootstrap.len();
        tracing::info!(%session, bootstrap_count, "joining collaboration network session");
        direct_state.attach_session(session).await;
        replace_swarm(
          &mut swarms,
          SwarmHandle::spawn(endpoint.clone(), gossip.clone(), direct_state.clone(), session, bootstrap, evt_tx.clone())?,
        )
        .await;
        tracing::debug!(%session, bootstrap_count, "collaboration network join subscribed");
      },
      NetCommand::LeaveSession { session } => {
        tracing::info!(%session, "leaving collaboration network session");
        if let Some(handle) = swarms.remove(&session) {
          handle.stop().await;
        } else {
          tracing::debug!(%session, "leave requested for missing collaboration swarm");
        }
        direct_state.detach_session(session).await;
      },
      NetCommand::Publish { session, payload } => {
        let payload_kind = payload.kind();
        let payload_bytes = payload.byte_len();
        if let Some(handle) = swarms.get(&session) {
          tracing::trace!(%session, payload_kind, payload_bytes, "publishing collaboration payload");
          handle.publish(payload).await?;
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
      NetCommand::PullSnapshot { session, candidates, reply } => {
        let candidate_count = candidates.len();
        tracing::debug!(%session, candidate_count, "pulling collaboration snapshot");
        let result = direct::pull_with_fallback(DirectRequest::Snapshot { session }, candidates, DIRECT_PULL_TIMEOUT).await;
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
      NetCommand::MintTicketAddr { reply } => {
        tracing::debug!(peer = %bound_addr.id, "minting collaboration ticket address");
        let _ = reply.send(bound_addr.clone()).await;
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
