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
use tracing::warn;

use crate::{
  SessionId,
  proto_direct::{AssetBytes, DirectRequest},
};

use super::{
  NetCommand, NetEvent, TicketSeed,
  direct::{self, DirectProto, DirectServeState},
  swarm::SwarmHandle,
};

pub type CommandSender = async_channel::Sender<NetCommand>;
pub type EventReceiver = async_channel::Receiver<NetEvent>;

const DIRECT_PULL_TIMEOUT: Duration = Duration::from_secs(10);
const ENDPOINT_ADDR_READY_TIMEOUT: Duration = Duration::from_secs(2);
static RUNTIME: OnceLock<Mutex<Option<RuntimeBridge>>> = OnceLock::new();

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
    return Ok(runtime.handles());
  }

  let next_runtime = spawn_runtime()?;
  let handles = next_runtime.handles();
  *runtime = Some(next_runtime);
  drop(runtime);
  Ok(handles)
}

fn spawn_runtime() -> Result<RuntimeBridge> {
  let (cmd_tx, cmd_rx) = async_channel::unbounded();
  let (evt_tx, evt_rx) = async_channel::unbounded();
  let thread_evt_tx = evt_tx.clone();

  thread::Builder::new()
    .name("flowstate-collab-net".into())
    .spawn(move || {
      let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
      {
        Ok(runtime) => runtime,
        Err(error) => {
          let _ = thread_evt_tx.try_send(NetEvent::EndpointOnline(false));
          tracing::warn!("flowstate collaboration tokio runtime failed: {error:#}");
          return;
        },
      };

      if let Err(error) = runtime.block_on(net_main(cmd_rx, evt_tx)) {
        let _ = thread_evt_tx.try_send(NetEvent::EndpointOnline(false));
        tracing::warn!("flowstate collaboration networking stopped: {error:#}");
      }
    })
    .context("spawning collaboration networking thread failed")?;

  Ok(RuntimeBridge {
    commands: cmd_tx,
    events: evt_rx,
  })
}

async fn net_main(cmd_rx: async_channel::Receiver<NetCommand>, evt_tx: async_channel::Sender<NetEvent>) -> Result<()> {
  let endpoint = Endpoint::builder(presets::N0)
    .bind()
    .await
    .context("binding collaboration endpoint failed")?;
  direct::install_endpoint(endpoint.clone());

  let gossip = Gossip::builder().spawn(endpoint.clone());
  let direct_state = DirectServeState::default();
  let _router = iroh::protocol::Router::builder(endpoint.clone())
    .accept(iroh_gossip::ALPN, gossip.clone())
    .accept(crate::DIRECT_ALPN, DirectProto::new(direct_state.clone()))
    .spawn();

  let _ = evt_tx.send(NetEvent::EndpointOnline(true)).await;
  let mut swarms = HashMap::new();

  while let Ok(command) = cmd_rx.recv().await {
    match command {
      NetCommand::EnsureUp => {
        let _ = evt_tx.send(NetEvent::EndpointOnline(true)).await;
      },
      NetCommand::RegisterDirectHandler { session, handler } => {
        direct_state.register_handler(session, handler).await;
      },
      NetCommand::CreateSession { session, reply } => {
        let result = create_session(&mut swarms, &endpoint, &gossip, &direct_state, &evt_tx, session).await;
        if let Err(error) = &result {
          warn!("flowstate collaboration create session failed: {error:#}");
        }
        let _ = reply.send(result).await;
      },
      NetCommand::JoinSession { session, bootstrap } => {
        if let Err(error) = join_session(&mut swarms, &endpoint, &gossip, &direct_state, &evt_tx, session, bootstrap).await {
          warn!("flowstate collaboration join session failed: {error:#}");
          let _ = evt_tx
            .send(NetEvent::SubscribeFailed {
              session,
              error: format!("{error:#}"),
            })
            .await;
        }
      },
      NetCommand::LeaveSession { session } => {
        if let Some(handle) = swarms.remove(&session) {
          handle.stop().await;
        }
        direct_state.detach_session(session).await;
      },
      NetCommand::Publish { session, payload } => {
        if let Some(handle) = swarms.get(&session)
          && let Err(error) = handle.publish(payload).await
        {
          warn!("flowstate collaboration publish failed: {error:#}");
        }
      },
      NetCommand::PullUpdates {
        session,
        candidates,
        our_vv,
        reply,
      } => {
        let result = direct::pull_with_fallback(DirectRequest::Updates { session, have_vv: our_vv }, candidates, DIRECT_PULL_TIMEOUT).await;
        let _ = reply.send(result).await;
      },
      NetCommand::PullSnapshot {
        session,
        candidates,
        progress,
        reply,
      } => {
        let result = direct::pull_with_fallback_progress(DirectRequest::Snapshot { session }, candidates, DIRECT_PULL_TIMEOUT, progress).await;
        let _ = reply.send(result).await;
      },
      NetCommand::PullBlob {
        session,
        candidates,
        blob,
        reply,
      } => {
        let result = direct::pull_with_fallback(DirectRequest::Blob { session, blob }, candidates, DIRECT_PULL_TIMEOUT).await;
        let _ = reply.send(result).await;
      },
      NetCommand::PullAsset {
        session,
        candidates,
        asset,
        reply,
      } => {
        let result = direct::pull_with_fallback(DirectRequest::Asset { session, asset }, candidates, DIRECT_PULL_TIMEOUT)
          .await
          .map(|bytes| AssetBytes { bytes });
        let _ = reply.send(result).await;
      },
      NetCommand::MintTicketAddr { reply } => {
        let _ = reply.send(reachable_endpoint_addr(&endpoint).await).await;
      },
      NetCommand::Shutdown => break,
    }
  }

  for (_, handle) in swarms {
    handle.stop().await;
  }
  endpoint.close().await;
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
  direct_state.attach_session(session).await;
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
  Ok(TicketSeed {
    inviter: reachable_endpoint_addr(endpoint).await,
  })
}

async fn join_session(
  swarms: &mut HashMap<SessionId, SwarmHandle>,
  endpoint: &Endpoint,
  gossip: &Gossip,
  direct_state: &DirectServeState,
  evt_tx: &async_channel::Sender<NetEvent>,
  session: SessionId,
  bootstrap: Vec<EndpointAddr>,
) -> Result<()> {
  direct_state.attach_session(session).await;
  replace_swarm(
    swarms,
    SwarmHandle::spawn(endpoint.clone(), gossip.clone(), direct_state.clone(), session, bootstrap, evt_tx.clone())?,
  )
  .await;
  Ok(())
}

async fn replace_swarm(swarms: &mut HashMap<crate::SessionId, SwarmHandle>, handle: SwarmHandle) {
  let session = handle.session;
  if let Some(previous) = swarms.insert(session, handle) {
    previous.stop().await;
  }
}

fn endpoint_addr(endpoint: &Endpoint) -> EndpointAddr {
  endpoint.addr()
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
