use std::time::Duration;

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
  capability::unix_now,
  ids::SessionId,
  net::auth::SessionAuthRegistry,
  proto_gossip::{self, GOSSIP_INLINE_LIMIT, GossipMsg},
};

use super::{NetEvent, PublishPayload, direct::DirectServeState};

/// Generous bound for queued publish commands; senders backpressure via
/// `send().await` when the swarm loop falls behind (FS-074).
const COMMAND_CHANNEL_CAPACITY: usize = 1024;
/// Presence refreshes recur on a keepalive timer, so a lost frame heals
/// itself; still, transient broadcast failures get a small bounded retry
/// before the frame is dropped (FS-072).
const PRESENCE_PUBLISH_ATTEMPTS: u32 = 3;
const PRESENCE_PUBLISH_BACKOFF: Duration = Duration::from_millis(100);

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
    let local_peer = endpoint_id(&endpoint.addr());
    let (tx, rx) = async_channel::bounded(COMMAND_CHANNEL_CAPACITY);
    tokio::spawn(async move {
      if let Err(error) = run_session(gossip, direct_state, session, local_peer, bootstrap_ids, rx, evt_tx.clone()).await {
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
  local_peer: EndpointId,
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
  let mut dropped_coalescable: u64 = 0;

  loop {
    tokio::select! {
      command = rx.recv() => {
        match command {
          // FS-071/FS-072: publish failures are classified inside
          // `handle_publish` and never abort the swarm loop.
          Ok(SwarmCommand::Publish(payload)) => handle_publish(&sender, &direct_state, session, payload).await,
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
        // Only unrecoverable subscription loss exits the loop (FS-071):
        // a `None`/`Err` here means the gossip topic itself is gone.
        let event = event.context("collaboration gossip receiver ended")??;
        handle_event(event, session, local_peer, direct_state.auth(), &evt_tx, &mut dropped_coalescable).await;
      },
    }
  }

  tracing::info!(%session, dropped_coalescable, "collaboration gossip swarm stopped");
  Ok(())
}

/// Minimal broadcast abstraction so publish-failure handling is unit-testable
/// without a live gossip topic.
trait FrameSink {
  async fn send_frame(&self, frame: Vec<u8>, neighbors_only: bool) -> Result<()>;
}

impl FrameSink for GossipSender {
  async fn send_frame(&self, frame: Vec<u8>, neighbors_only: bool) -> Result<()> {
    if neighbors_only {
      self.broadcast_neighbors(frame.into()).await?;
    } else {
      self.broadcast(frame.into()).await?;
    }
    Ok(())
  }
}

/// Publish one payload, classifying failures (FS-071/FS-072/FS-074):
/// - frame build/encode/blob-insert failures: log and drop the frame;
/// - transient broadcast failures: presence gets a bounded retry with
///   backoff, everything else is logged and dropped (updates recover via
///   anti-entropy digests, digests recur on a timer).
///
/// This function never returns an error, so a publish failure cannot kill
/// the swarm loop.
async fn handle_publish<S: FrameSink>(sink: &S, direct_state: &DirectServeState, session: SessionId, payload: PublishPayload) {
  let payload_kind = payload.kind();
  let payload_bytes = payload.byte_len();
  let retriable = matches!(payload, PublishPayload::Presence(_));
  let (message, neighbors_only) = match payload {
    PublishPayload::Update(bytes) => match update_message(direct_state, session, bytes).await {
      Ok(message) => (message, false),
      Err(error) => {
        tracing::warn!(%session, payload_kind, payload_bytes, error = %format_args!("{error:#}"), "building collaboration gossip update failed; dropping frame");
        return;
      },
    },
    PublishPayload::Presence(bytes) => (GossipMsg::Presence(bytes), false),
    PublishPayload::Digest { vv } => (GossipMsg::Digest { session, vv }, true),
    PublishPayload::CapabilityEpoch { epoch, signature } => (GossipMsg::CapabilityEpoch { epoch, signature }, false),
  };

  let frame = match if neighbors_only {
    proto_gossip::encode(&message)
  } else {
    proto_gossip::encode_inline(&message)
  } {
    Ok(frame) => frame,
    Err(error) => {
      tracing::warn!(%session, payload_kind, payload_bytes, error = %format_args!("{error:#}"), "encoding collaboration gossip frame failed; dropping frame");
      return;
    },
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

  let attempts = if retriable { PRESENCE_PUBLISH_ATTEMPTS } else { 1 };
  for attempt in 1..=attempts {
    match sink.send_frame(frame.clone(), neighbors_only).await {
      Ok(()) => {
        tracing::trace!(%session, payload_kind, frame_bytes, neighbors_only, attempt, "collaboration gossip frame broadcast complete");
        return;
      },
      Err(error) if attempt < attempts => {
        tracing::debug!(%session, payload_kind, attempt, error = %format_args!("{error:#}"), "collaboration gossip broadcast failed; retrying");
        tokio::time::sleep(PRESENCE_PUBLISH_BACKOFF * attempt).await;
      },
      Err(error) => {
        tracing::warn!(%session, payload_kind, payload_bytes, attempt, error = %format_args!("{error:#}"), "collaboration gossip broadcast failed; dropping frame");
      },
    }
  }
}

async fn update_message(direct_state: &DirectServeState, session: SessionId, bytes: Vec<u8>) -> Result<GossipMsg> {
  let update_bytes = bytes.len();
  let inline = GossipMsg::Update(bytes.clone());
  if proto_gossip::encoded_len(&inline)? <= GOSSIP_INLINE_LIMIT {
    tracing::trace!(%session, update_bytes, "collaboration update will be sent inline over gossip");
    return Ok(inline);
  }

  let blob = direct_state.insert_blob(session, bytes).await?;
  tracing::debug!(%session, ?blob, update_bytes, "collaboration update exceeded gossip limit; publishing blob announcement");
  Ok(GossipMsg::UpdateAvailable {
    blob,
    len: update_bytes as u64,
  })
}

async fn handle_event(
  event: Event,
  session: SessionId,
  local_peer: EndpointId,
  auth: &SessionAuthRegistry,
  evt_tx: &Sender<NetEvent>,
  dropped_coalescable: &mut u64,
) {
  match event {
    Event::Received(message) if message.delivered_from == local_peer => {
      tracing::trace!(
        %session,
        from = %message.delivered_from,
        frame_bytes = message.content.len(),
        "ignored self-delivered collaboration gossip frame",
      );
    },
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
        // FS-080: signed revocation control frames are consumed at the
        // transport layer; they only mutate the shared auth registry.
        if let GossipMsg::CapabilityEpoch { epoch, signature } = &msg {
          let applied = auth.apply_epoch_bump(session, *epoch, signature);
          tracing::info!(%session, from = %message.delivered_from, epoch, applied, "collaboration capability epoch frame received");
          return;
        }
        // FS-080 role enforcement at the gossip layer: drop document updates
        // delivered from endpoints that authenticated as viewers. Updates are
        // authorless CRDT bytes and `delivered_from` is the delivering
        // neighbor, so this catches direct frames from known viewers only;
        // relayed or never-authenticated senders pass (see net::auth docs).
        if matches!(msg, GossipMsg::Update(_) | GossipMsg::UpdateAvailable { .. })
          && auth.should_drop_update_from(session, message.delivered_from, unix_now())
        {
          tracing::warn!(
            %session,
            from = %message.delivered_from,
            gossip_kind = msg.kind(),
            "dropped collaboration document update delivered from a view-only peer",
          );
          return;
        }
        forward_event(
          evt_tx,
          NetEvent::Gossip {
            session,
            from: message.delivered_from,
            msg,
          },
          session,
          dropped_coalescable,
        )
        .await;
      },
      Err(error) if proto_gossip::is_protocol_version_mismatch(&error) => {
        forward_event(
          evt_tx,
          NetEvent::IncompatibleVersion {
            session,
            peer: message.delivered_from,
          },
          session,
          dropped_coalescable,
        )
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
      forward_event(evt_tx, NetEvent::NeighborUp { session, peer }, session, dropped_coalescable).await;
    },
    Event::NeighborDown(peer) => {
      tracing::info!(%session, peer = %peer, "collaboration gossip neighbor down");
      forward_event(evt_tx, NetEvent::NeighborDown { session, peer }, session, dropped_coalescable).await;
    },
    Event::Lagged => {
      tracing::warn!(%session, "collaboration gossip receiver lagged");
      forward_event(evt_tx, NetEvent::GossipLagged { session }, session, dropped_coalescable).await;
    },
  }
}

/// Forward an event to the UI-facing channel without blocking the network
/// task on coalescable traffic (FS-074): presence/digest frames recur, so
/// when the channel is full they are dropped and counted instead of stalling
/// gossip processing. Non-coalescable events (document updates, membership
/// changes) apply backpressure via `send().await`.
async fn forward_event(evt_tx: &Sender<NetEvent>, event: NetEvent, session: SessionId, dropped_coalescable: &mut u64) {
  if is_coalescable(&event) {
    match evt_tx.try_send(event) {
      Ok(()) => {},
      Err(async_channel::TrySendError::Full(_)) => {
        *dropped_coalescable += 1;
        tracing::warn!(
          %session,
          dropped_total = *dropped_coalescable,
          "collaboration event channel is full; dropped a coalescable frame",
        );
      },
      Err(async_channel::TrySendError::Closed(_)) => {
        tracing::debug!(%session, "collaboration event channel closed; dropping event");
      },
    }
  } else {
    let _ = evt_tx.send(event).await;
  }
}

fn is_coalescable(event: &NetEvent) -> bool {
  matches!(
    event,
    NetEvent::Gossip {
      msg: GossipMsg::Presence(_) | GossipMsg::Digest { .. },
      ..
    } | NetEvent::GossipLagged { .. }
  )
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
  use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
  };

  use anyhow::{Result, anyhow};
  use iroh::SecretKey;
  use iroh_gossip::{
    api::{Event, Message},
    proto::DeliveryScope,
  };

  use crate::{
    SessionId,
    capability::{CapabilityRole, SessionCapability, sign_epoch_bump},
    net::{NetEvent, PublishPayload, auth::SessionAuthRegistry, direct::DirectServeState},
    proto_gossip::{self, GossipMsg},
  };

  use super::{FrameSink, PRESENCE_PUBLISH_ATTEMPTS, handle_event, handle_publish};

  struct FailingSink {
    attempts: Arc<AtomicU32>,
  }

  impl FrameSink for FailingSink {
    #[allow(clippy::unused_async_trait_impl, reason = "test mock must match the async FrameSink trait signature")]
    async fn send_frame(&self, _frame: Vec<u8>, _neighbors_only: bool) -> Result<()> {
      self.attempts.fetch_add(1, Ordering::SeqCst);
      Err(anyhow!("simulated gossip publish failure"))
    }
  }

  fn received(frame: Vec<u8>, from: iroh::EndpointId) -> Event {
    Event::Received(Message {
      content: frame.into(),
      scope: DeliveryScope::Neighbors,
      delivered_from: from,
    })
  }

  #[tokio::test]
  async fn version_mismatch_emits_incompatible_version_once() {
    let session = SessionId::new();
    let local_peer = SecretKey::generate().public();
    let peer = SecretKey::generate().public();
    let auth = SessionAuthRegistry::default();
    let mut frame = proto_gossip::encode(&GossipMsg::Presence(Vec::new())).expect("valid gossip frame");
    frame[0] = frame[0].wrapping_add(1);

    let (evt_tx, evt_rx) = async_channel::unbounded();
    let mut dropped = 0;
    handle_event(received(frame, peer), session, local_peer, &auth, &evt_tx, &mut dropped).await;

    match evt_rx.recv().await.expect("incompatible-version event") {
      NetEvent::IncompatibleVersion {
        session: event_session,
        peer: event_peer,
      } => {
        assert_eq!(event_session, session);
        assert_eq!(event_peer, peer);
      },
      event => panic!("unexpected event: {event:?}"),
    }
    assert!(evt_rx.try_recv().is_err());
  }

  #[tokio::test]
  async fn self_delivered_gossip_frame_is_ignored() {
    let session = SessionId::new();
    let local_peer = SecretKey::generate().public();
    let auth = SessionAuthRegistry::default();
    let frame = proto_gossip::encode(&GossipMsg::Update(vec![1, 2, 3])).expect("valid gossip frame");

    let (evt_tx, evt_rx) = async_channel::unbounded();
    let mut dropped = 0;
    handle_event(received(frame, local_peer), session, local_peer, &auth, &evt_tx, &mut dropped).await;

    assert!(evt_rx.try_recv().is_err());
  }

  /// FS-071/FS-072: a failing broadcast must not surface as an error (which
  /// previously killed the swarm loop). Presence frames get a bounded retry;
  /// other payloads are dropped after one attempt.
  #[tokio::test]
  async fn publish_failure_is_swallowed_with_bounded_presence_retry() {
    let session = SessionId::new();
    let direct_state = DirectServeState::default();

    let attempts = Arc::new(AtomicU32::new(0));
    let sink = FailingSink { attempts: attempts.clone() };
    handle_publish(&sink, &direct_state, session, PublishPayload::Presence(vec![1, 2, 3])).await;
    assert_eq!(attempts.load(Ordering::SeqCst), PRESENCE_PUBLISH_ATTEMPTS);

    attempts.store(0, Ordering::SeqCst);
    handle_publish(&sink, &direct_state, session, PublishPayload::Update(vec![4, 5, 6])).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    attempts.store(0, Ordering::SeqCst);
    handle_publish(&sink, &direct_state, session, PublishPayload::Digest { vv: vec![7] }).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
  }

  /// FS-080: document updates delivered from an endpoint that authenticated
  /// as a viewer are dropped before reaching the session, while presence from
  /// the same endpoint still flows.
  #[tokio::test]
  async fn updates_delivered_from_an_authenticated_viewer_are_dropped() {
    let session = SessionId::new();
    let local_peer = SecretKey::generate().public();
    let owner = SecretKey::generate();
    let viewer = SecretKey::generate().public();
    let auth = SessionAuthRegistry::default();
    auth.configure_session(session, owner.public(), 0);
    let capability = SessionCapability::issue(&owner, session, CapabilityRole::Viewer, u64::MAX, 0);
    auth
      .authenticate_peer(session, viewer, &capability, 10)
      .expect("viewer capability should authenticate");

    let (evt_tx, evt_rx) = async_channel::unbounded();
    let mut dropped = 0;
    let update = proto_gossip::encode(&GossipMsg::Update(vec![1])).expect("valid gossip frame");
    handle_event(received(update, viewer), session, local_peer, &auth, &evt_tx, &mut dropped).await;
    assert!(evt_rx.try_recv().is_err(), "viewer update must be dropped");

    let presence = proto_gossip::encode(&GossipMsg::Presence(vec![2])).expect("valid gossip frame");
    handle_event(received(presence, viewer), session, local_peer, &auth, &evt_tx, &mut dropped).await;
    assert!(
      matches!(
        evt_rx.try_recv(),
        Ok(NetEvent::Gossip {
          msg: GossipMsg::Presence(_),
          ..
        })
      ),
      "viewer presence must still flow"
    );
  }

  /// FS-080: signed epoch bumps are consumed at the transport layer and raise
  /// the shared registry epoch; forged bumps are ignored.
  #[tokio::test]
  async fn capability_epoch_frames_update_the_auth_registry() {
    let session = SessionId::new();
    let local_peer = SecretKey::generate().public();
    let owner = SecretKey::generate();
    let relay = SecretKey::generate().public();
    let auth = SessionAuthRegistry::default();
    auth.configure_session(session, owner.public(), 0);

    let (evt_tx, evt_rx) = async_channel::unbounded();
    let mut dropped = 0;

    let forged = proto_gossip::encode(&GossipMsg::CapabilityEpoch {
      epoch: 5,
      signature: sign_epoch_bump(&SecretKey::generate(), session, 5),
    })
    .expect("valid gossip frame");
    handle_event(received(forged, relay), session, local_peer, &auth, &evt_tx, &mut dropped).await;
    assert_eq!(auth.current_epoch(session), Some(0));

    let genuine = proto_gossip::encode(&GossipMsg::CapabilityEpoch {
      epoch: 5,
      signature: sign_epoch_bump(&owner, session, 5),
    })
    .expect("valid gossip frame");
    handle_event(received(genuine, relay), session, local_peer, &auth, &evt_tx, &mut dropped).await;
    assert_eq!(auth.current_epoch(session), Some(5));
    assert!(evt_rx.try_recv().is_err(), "epoch frames are not forwarded to sessions");
  }

  /// FS-074: when the event channel is full, coalescable frames (presence,
  /// digest) are dropped and counted instead of blocking the network task;
  /// the queued event is preserved.
  #[tokio::test]
  async fn coalescable_frames_are_dropped_when_the_event_channel_is_full() {
    let session = SessionId::new();
    let local_peer = SecretKey::generate().public();
    let peer = SecretKey::generate().public();
    let auth = SessionAuthRegistry::default();

    let (evt_tx, evt_rx) = async_channel::bounded(1);
    evt_tx
      .try_send(NetEvent::GossipLagged { session })
      .expect("prefill event channel");

    let mut dropped = 0;
    let presence = proto_gossip::encode(&GossipMsg::Presence(vec![1])).expect("valid gossip frame");
    handle_event(received(presence, peer), session, local_peer, &auth, &evt_tx, &mut dropped).await;
    assert_eq!(dropped, 1);
    assert!(matches!(evt_rx.try_recv(), Ok(NetEvent::GossipLagged { .. })));
    assert!(evt_rx.try_recv().is_err());
  }
}
