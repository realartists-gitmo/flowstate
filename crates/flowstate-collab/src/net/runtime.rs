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
    return Ok((runtime.commands.clone(), runtime.events.clone()));
  }

  let runtime = spawn_runtime()?;
  let set_failed = RUNTIME.set(runtime.clone()).is_err();
  if set_failed
    && let Some(runtime) = RUNTIME.get()
  {
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
      let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
      {
        Ok(runtime) => runtime,
        Err(error) => {
          let _ = thread_evt_tx.try_send(NetEvent::EndpointOnline(false));
          eprintln!("flowstate collaboration tokio runtime failed: {error:#}");
          return;
        },
      };

      if let Err(error) = runtime.block_on(net_main(cmd_rx, evt_tx)) {
        let _ = thread_evt_tx.try_send(NetEvent::EndpointOnline(false));
        eprintln!("flowstate collaboration networking stopped: {error:#}");
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
        direct_state.attach_session(session).await;
        replace_swarm(
          &mut swarms,
          SwarmHandle::spawn(endpoint.clone(), gossip.clone(), direct_state.clone(), session, Vec::new(), evt_tx.clone())?,
        )
        .await;
        let _ = reply
          .send(Ok(TicketSeed {
            inviter: endpoint_addr(&endpoint),
          }))
          .await;
      },
      NetCommand::JoinSession { session, bootstrap } => {
        direct_state.attach_session(session).await;
        replace_swarm(
          &mut swarms,
          SwarmHandle::spawn(endpoint.clone(), gossip.clone(), direct_state.clone(), session, bootstrap, evt_tx.clone())?,
        )
        .await;
      },
      NetCommand::LeaveSession { session } => {
        if let Some(handle) = swarms.remove(&session) {
          handle.stop().await;
        }
        direct_state.detach_session(session).await;
      },
      NetCommand::Publish { session, payload } => {
        if let Some(handle) = swarms.get(&session) {
          handle.publish(payload).await?;
        }
      },
      NetCommand::PullUpdates {
        session,
        from,
        our_vv,
        reply,
      } => {
        let result = direct::pull_with_fallback(DirectRequest::Updates { session, have_vv: our_vv }, vec![from], DIRECT_PULL_TIMEOUT).await;
        let _ = reply.send(result).await;
      },
      NetCommand::PullSnapshot { session, from, reply } => {
        let result = direct::pull_with_fallback(DirectRequest::Snapshot { session }, vec![from], DIRECT_PULL_TIMEOUT).await;
        let _ = reply.send(result).await;
      },
      NetCommand::PullBlob { session, from, blob, reply } => {
        let result = direct::pull_with_fallback(DirectRequest::Blob { session, blob }, vec![from], DIRECT_PULL_TIMEOUT).await;
        let _ = reply.send(result).await;
      },
      NetCommand::PullAsset { session, from, asset, reply } => {
        let result = direct::pull_with_fallback(DirectRequest::Asset { session, asset }, vec![from], DIRECT_PULL_TIMEOUT)
          .await
          .map(|bytes| AssetBytes { bytes });
        let _ = reply.send(result).await;
      },
      NetCommand::MintTicketAddr { reply } => {
        let _ = reply.send(endpoint_addr(&endpoint)).await;
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

async fn replace_swarm(swarms: &mut HashMap<crate::SessionId, SwarmHandle>, handle: SwarmHandle) {
  let session = handle.session;
  if let Some(previous) = swarms.insert(session, handle) {
    previous.stop().await;
  }
}

fn endpoint_addr(endpoint: &Endpoint) -> EndpointAddr {
  endpoint.addr()
}
