use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};
use iroh::{Endpoint, EndpointAddr, EndpointId, address_lookup::memory::MemoryLookup};
use iroh_gossip::{
  api::{Event, GossipSender},
  net::Gossip,
  proto::TopicId,
};
use n0_future::StreamExt as _;

use crate::{
  ids::SessionId,
  proto_gossip::{self, GOSSIP_INLINE_LIMIT, GossipMsg},
};

use super::{NetEvent, PublishPayload, direct::DirectServeState};

#[derive(Debug)]
enum SwarmCommand {
  Publish(PublishPayload),
  Stop,
}

#[derive(Clone, Debug)]
pub struct SwarmHandle {
  pub session: SessionId,
  tx: Sender<SwarmCommand>,
}

impl SwarmHandle {
  pub fn spawn(
    endpoint: Endpoint,
    gossip: Gossip,
    direct_state: DirectServeState,
    session: SessionId,
    bootstrap: Vec<EndpointAddr>,
    evt_tx: Sender<NetEvent>,
  ) -> Result<Self> {
    register_bootstrap_addrs(&endpoint, &bootstrap)?;
    let bootstrap_ids = bootstrap.iter().map(endpoint_id).collect::<Vec<_>>();
    let (tx, rx) = async_channel::unbounded();
    tokio::spawn(async move {
      if let Err(error) = run_session(gossip, direct_state, session, bootstrap_ids, rx, evt_tx.clone()).await {
        let _ = evt_tx
          .send(NetEvent::SubscribeFailed {
            session,
            error: format!("{error:#}"),
          })
          .await;
      }
    });
    Ok(Self { session, tx })
  }

  pub async fn publish(&self, payload: PublishPayload) -> Result<()> {
    self
      .tx
      .send(SwarmCommand::Publish(payload))
      .await
      .context("collaboration swarm task stopped")
  }

  pub async fn stop(&self) {
    let _ = self.tx.send(SwarmCommand::Stop).await;
  }
}

async fn run_session(
  gossip: Gossip,
  direct_state: DirectServeState,
  session: SessionId,
  bootstrap: Vec<EndpointId>,
  rx: Receiver<SwarmCommand>,
  evt_tx: Sender<NetEvent>,
) -> Result<()> {
  let topic = gossip
    .subscribe(topic_id(session), bootstrap)
    .await
    .context("subscribing to collaboration gossip topic failed")?;
  let (sender, mut receiver) = topic.split();

  loop {
    tokio::select! {
      command = rx.recv() => {
        match command {
          Ok(SwarmCommand::Publish(payload)) => publish(&sender, &direct_state, session, payload).await?,
          Ok(SwarmCommand::Stop) | Err(_) => break,
        }
      },
      event = receiver.next() => {
        let event = event.context("collaboration gossip receiver ended")??;
        handle_event(event, session, &evt_tx).await?;
      },
    }
  }

  Ok(())
}

async fn publish(sender: &GossipSender, direct_state: &DirectServeState, session: SessionId, payload: PublishPayload) -> Result<()> {
  let (message, neighbors_only) = match payload {
    PublishPayload::Update(bytes) => (update_message(direct_state, session, bytes).await?, false),
    PublishPayload::Presence(bytes) => (GossipMsg::Presence(bytes), false),
    PublishPayload::Digest { vv } => (GossipMsg::Digest { session, vv }, true),
  };

  let frame = if neighbors_only {
    proto_gossip::encode(&message)?
  } else {
    proto_gossip::encode_inline(&message)?
  };

  if neighbors_only {
    sender.broadcast_neighbors(frame.into()).await?;
  } else {
    sender.broadcast(frame.into()).await?;
  }
  Ok(())
}

async fn update_message(direct_state: &DirectServeState, session: SessionId, bytes: Vec<u8>) -> Result<GossipMsg> {
  let inline = GossipMsg::Update(bytes.clone());
  if proto_gossip::encoded_len(&inline)? <= GOSSIP_INLINE_LIMIT {
    return Ok(inline);
  }

  let len = bytes.len() as u64;
  let blob = direct_state.insert_blob(session, bytes).await;
  Ok(GossipMsg::UpdateAvailable { blob, len })
}

async fn handle_event(event: Event, session: SessionId, evt_tx: &Sender<NetEvent>) -> Result<()> {
  match event {
    Event::Received(message) => match proto_gossip::decode(&message.content) {
      Ok(msg) => {
        let _ = evt_tx
          .send(NetEvent::Gossip {
            session,
            from: message.delivered_from,
            msg,
          })
          .await;
      },
      Err(error) => eprintln!("flowstate collab ignored gossip frame: {error:#}"),
    },
    Event::NeighborUp(peer) => {
      let _ = evt_tx.send(NetEvent::NeighborUp { session, peer }).await;
    },
    Event::NeighborDown(peer) => {
      let _ = evt_tx.send(NetEvent::NeighborDown { session, peer }).await;
    },
    Event::Lagged => {
      let _ = evt_tx.send(NetEvent::GossipLagged { session }).await;
    },
  }
  Ok(())
}

fn register_bootstrap_addrs(endpoint: &Endpoint, bootstrap: &[EndpointAddr]) -> Result<()> {
  if bootstrap.is_empty() {
    return Ok(());
  }
  endpoint
    .address_lookup()?
    .add(MemoryLookup::from_endpoint_info(bootstrap.iter().cloned()));
  Ok(())
}

fn endpoint_id(addr: &EndpointAddr) -> EndpointId {
  addr.id
}

fn topic_id(session: SessionId) -> TopicId {
  TopicId::from_bytes(*session.as_bytes())
}
