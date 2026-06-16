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
    tracing::info!(%session, bootstrap_count = bootstrap.len(), "spawning collaboration gossip swarm");
    register_bootstrap_addrs(&endpoint, &bootstrap)?;
    let bootstrap_ids = bootstrap.iter().map(endpoint_id).collect::<Vec<_>>();
    let (tx, rx) = async_channel::unbounded();
    tokio::spawn(async move {
      if let Err(error) = run_session(gossip, direct_state, session, bootstrap_ids, rx, evt_tx.clone()).await {
        tracing::error!(%session, error = %format_args!("{error:#}"), "collaboration gossip swarm failed");
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
    tracing::trace!(session = %self.session, payload_kind = payload.kind(), payload_bytes = payload.byte_len(), "queueing collaboration gossip publish");
    self
      .tx
      .send(SwarmCommand::Publish(payload))
      .await
      .context("collaboration swarm task stopped")
  }

  pub async fn stop(&self) {
    tracing::debug!(session = %self.session, "stopping collaboration gossip swarm");
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
  tracing::info!(%session, bootstrap_count = bootstrap.len(), "subscribing to collaboration gossip topic");
  let topic = gossip
    .subscribe(topic_id(session), bootstrap)
    .await
    .context("subscribing to collaboration gossip topic failed")?;
  tracing::info!(%session, "subscribed to collaboration gossip topic");
  let (sender, mut receiver) = topic.split();

  loop {
    tokio::select! {
      command = rx.recv() => {
        match command {
          Ok(SwarmCommand::Publish(payload)) => publish(&sender, &direct_state, session, payload).await?,
          Ok(SwarmCommand::Stop) => {
            tracing::debug!(%session, "collaboration gossip swarm stop received");
            break;
          },
          Err(error) => {
            tracing::debug!(%session, error = %error, "collaboration gossip swarm command channel closed");
            break;
          },
        }
      },
      event = receiver.next() => {
        let event = event.context("collaboration gossip receiver ended")??;
        handle_event(event, session, &evt_tx).await?;
      },
    }
  }

  tracing::info!(%session, "collaboration gossip swarm stopped");
  Ok(())
}

async fn publish(sender: &GossipSender, direct_state: &DirectServeState, session: SessionId, payload: PublishPayload) -> Result<()> {
  let payload_kind = payload.kind();
  let payload_bytes = payload.byte_len();
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
  let frame_bytes = frame.len();
  tracing::trace!(
    %session,
    payload_kind,
    payload_bytes,
    gossip_kind = message.kind(),
    gossip_payload_bytes = message.payload_len(),
    frame_bytes,
    neighbors_only,
    "broadcasting collaboration gossip frame",
  );

  if neighbors_only {
    sender.broadcast_neighbors(frame.into()).await?;
  } else {
    sender.broadcast(frame.into()).await?;
  }
  tracing::trace!(%session, payload_kind, frame_bytes, neighbors_only, "collaboration gossip frame broadcast complete");
  Ok(())
}

async fn update_message(direct_state: &DirectServeState, session: SessionId, bytes: Vec<u8>) -> Result<GossipMsg> {
  let update_bytes = bytes.len();
  let inline = GossipMsg::Update(bytes.clone());
  if proto_gossip::encoded_len(&inline)? <= GOSSIP_INLINE_LIMIT {
    tracing::trace!(%session, update_bytes, "collaboration update will be sent inline over gossip");
    return Ok(inline);
  }

  let len = bytes.len() as u64;
  let blob = direct_state.insert_blob(session, bytes).await;
  tracing::debug!(%session, ?blob, update_bytes, "collaboration update exceeded gossip limit; publishing blob announcement");
  Ok(GossipMsg::UpdateAvailable { blob, len })
}

async fn handle_event(event: Event, session: SessionId, evt_tx: &Sender<NetEvent>) -> Result<()> {
  match event {
    Event::Received(message) => match proto_gossip::decode(&message.content) {
      Ok(msg) => {
        tracing::trace!(
          %session,
          from = %message.delivered_from,
          gossip_kind = msg.kind(),
          gossip_payload_bytes = msg.payload_len(),
          frame_bytes = message.content.len(),
          "received collaboration gossip frame",
        );
        let _ = evt_tx
          .send(NetEvent::Gossip {
            session,
            from: message.delivered_from,
            msg,
          })
          .await;
      },
      Err(error) if proto_gossip::is_protocol_version_mismatch(&error) => {
        let _ = evt_tx
          .send(NetEvent::IncompatibleVersion {
            session,
            peer: message.delivered_from,
          })
          .await;
      },
      Err(error) => tracing::warn!(
        %session,
        from = %message.delivered_from,
        frame_bytes = message.content.len(),
        error = %format_args!("{error:#}"),
        "ignored malformed collaboration gossip frame",
      ),
    },
    Event::NeighborUp(peer) => {
      tracing::info!(%session, peer = %peer, "collaboration gossip neighbor up");
      let _ = evt_tx.send(NetEvent::NeighborUp { session, peer }).await;
    },
    Event::NeighborDown(peer) => {
      tracing::info!(%session, peer = %peer, "collaboration gossip neighbor down");
      let _ = evt_tx.send(NetEvent::NeighborDown { session, peer }).await;
    },
    Event::Lagged => {
      tracing::warn!(%session, "collaboration gossip receiver lagged");
      let _ = evt_tx.send(NetEvent::GossipLagged { session }).await;
    },
  }
  Ok(())
}

fn register_bootstrap_addrs(endpoint: &Endpoint, bootstrap: &[EndpointAddr]) -> Result<()> {
  if bootstrap.is_empty() {
    return Ok(());
  }
  tracing::debug!(bootstrap_count = bootstrap.len(), "registering collaboration bootstrap addresses");
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

#[cfg(test)]
mod tests {
  use iroh::SecretKey;
  use iroh_gossip::{
    api::{Event, Message},
    proto::DeliveryScope,
  };

  use crate::{
    net::NetEvent,
    proto_gossip::{self, GossipMsg},
    SessionId,
  };

  use super::handle_event;

  #[tokio::test]
  async fn version_mismatch_emits_incompatible_version_once() {
    let session = SessionId::new();
    let peer = SecretKey::generate().public();
    let mut frame = proto_gossip::encode(&GossipMsg::Presence(Vec::new())).expect("valid gossip frame");
    frame[0] = frame[0].wrapping_add(1);

    let (evt_tx, evt_rx) = async_channel::unbounded();
    handle_event(
      Event::Received(Message {
        content: frame.into(),
        scope: DeliveryScope::Neighbors,
        delivered_from: peer,
      }),
      session,
      &evt_tx,
    )
    .await
    .expect("gossip event handling should ignore mismatches");

    match evt_rx.recv().await.expect("incompatible-version event") {
      NetEvent::IncompatibleVersion { session: event_session, peer: event_peer } => {
        assert_eq!(event_session, session);
        assert_eq!(event_peer, peer);
      },
      event => panic!("unexpected event: {event:?}"),
    }
    assert!(evt_rx.try_recv().is_err());
  }
}
