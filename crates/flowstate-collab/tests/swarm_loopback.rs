#![cfg(test)]

use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use async_channel::Receiver;
use flowstate_collab::{
  DIRECT_ALPN, SessionAdmission, SessionId,
  net::{
    NetEvent, PublishPayload, auth,
    direct::{self, DirectProto, DirectServeRequest, DirectServeState, DirectSessionHandler},
    swarm::SwarmHandle,
  },
  proto_direct::DirectRequest,
  proto_gossip::{GOSSIP_INLINE_LIMIT, GossipMsg},
  ticket::SessionTicket,
};
use iroh::{Endpoint, EndpointAddr, Watcher as _, address_lookup::memory::MemoryLookup, endpoint::presets, protocol::Router};
use iroh_gossip::net::Gossip;
use tokio::time::timeout;

struct Peer {
  endpoint: Endpoint,
  gossip: Gossip,
  direct_state: DirectServeState,
  router: Router,
  events: Receiver<NetEvent>,
  swarm: Option<SwarmHandle>,
}

impl Peer {
  async fn spawn(lookup: MemoryLookup) -> Result<Self> {
    let endpoint = Endpoint::builder(presets::Minimal)
      .address_lookup(lookup)
      .clear_ip_transports()
      .bind_addr("127.0.0.1:0")?
      .bind()
      .await
      .context("binding loopback endpoint failed")?;
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let direct_state = DirectServeState::default();
    let router = Router::builder(endpoint.clone())
      .accept(iroh_gossip::ALPN, gossip.clone())
      .accept(DIRECT_ALPN, DirectProto::new(direct_state.clone()))
      .spawn();
    let (_, events) = async_channel::unbounded();

    Ok(Self {
      endpoint,
      gossip,
      direct_state,
      router,
      events,
      swarm: None,
    })
  }

  fn start_swarm(&mut self, session: SessionId, bootstrap: Vec<EndpointAddr>) -> Result<()> {
    let (evt_tx, evt_rx) = async_channel::unbounded();
    self.events = evt_rx;
    self.swarm = Some(SwarmHandle::spawn(
      self.endpoint.clone(),
      self.gossip.clone(),
      self.direct_state.clone(),
      session,
      bootstrap,
      evt_tx,
    )?);
    Ok(())
  }

  async fn publish(&self, payload: PublishPayload) -> Result<()> {
    let Some(swarm) = &self.swarm else {
      bail!("peer swarm is not running");
    };
    swarm.publish(payload).await
  }

  async fn stop_swarm(&mut self) {
    if let Some(swarm) = self.swarm.take() {
      swarm.stop().await;
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local UDP loopback and real iroh gossip timing"]
async fn swarm_loopback_net_subset() -> Result<()> {
  let session = SessionId::new();
  let lookup = MemoryLookup::new();
  let mut a = Peer::spawn(lookup.clone()).await?;
  let mut b = Peer::spawn(lookup.clone()).await?;
  let mut c = Peer::spawn(lookup.clone()).await?;
  let admission = SessionAdmission::generate();
  for peer in [&a, &b, &c] {
    peer
      .direct_state
      .auth()
      .configure_session(session, admission.clone());
  }
  auth::install_local_admission(session, admission.clone());

  let a_addr = wait_addr(&a.endpoint).await;
  let b_addr = wait_addr(&b.endpoint).await;
  let c_addr = wait_addr(&c.endpoint).await;
  ensure!(!a_addr.is_empty(), "A endpoint did not expose loopback addresses");
  ensure!(!b_addr.is_empty(), "B endpoint did not expose loopback addresses");
  ensure!(!c_addr.is_empty(), "C endpoint did not expose loopback addresses");
  let addrs = [a_addr.clone(), b_addr.clone(), c_addr.clone()];
  for addr in addrs.iter().cloned() {
    lookup.add_endpoint_info(addr);
  }
  for endpoint in [&a.endpoint, &b.endpoint, &c.endpoint] {
    endpoint
      .address_lookup()?
      .add(MemoryLookup::from_endpoint_info(addrs.iter().cloned()));
  }

  install_snapshot_handler(&a, session, b"snapshot-from-a".to_vec()).await;
  let snapshot = direct::pull_with_endpoint(
    &b.endpoint,
    DirectRequest::Snapshot { session },
    vec![a.endpoint.id()],
    Duration::from_secs(5),
  )
  .await?;
  assert_eq!(snapshot, b"snapshot-from-a".to_vec());

  // B received the same session bearer secret as A, so its invite is not an
  // owner proxy: C can authenticate directly to B and bootstrap from B even
  // if A is unavailable.
  install_snapshot_handler(&b, session, b"snapshot-from-b".to_vec()).await;
  let b_invite = SessionTicket::new(session, vec![b_addr.clone()], "Shared brief".into(), admission.clone());
  let b_invite = SessionTicket::decode_text(&b_invite.encode_text())?;
  let snapshot = direct::pull_with_endpoint(
    &c.endpoint,
    DirectRequest::Snapshot { session },
    vec![b_invite.bootstrap[0].id],
    Duration::from_secs(5),
  )
  .await?;
  assert_eq!(snapshot, b"snapshot-from-b".to_vec());

  a.start_swarm(session, Vec::new())?;
  b.start_swarm(session, vec![a_addr.clone()])?;
  c.start_swarm(session, vec![b_addr.clone()])?;
  recv_neighbor(&a.events).await?;
  recv_neighbor(&b.events).await?;
  recv_neighbor(&c.events).await?;

  let small_update = b"a-to-b-to-c".to_vec();
  a.publish(PublishPayload::Update(small_update.clone()))
    .await?;
  recv_update(&b.events, &small_update).await?;
  recv_update(&c.events, &small_update).await?;

  a.publish(PublishPayload::Presence(b"presence-a".to_vec()))
    .await?;
  b.publish(PublishPayload::Presence(b"presence-b".to_vec()))
    .await?;
  c.publish(PublishPayload::Presence(b"presence-c".to_vec()))
    .await?;
  recv_presence(&a.events).await?;
  recv_presence(&b.events).await?;
  recv_presence(&c.events).await?;

  let large_update = vec![7; GOSSIP_INLINE_LIMIT + 1];
  a.publish(PublishPayload::Update(large_update.clone()))
    .await?;
  let blob = recv_update_available(&b.events, large_update.len() as u64).await?;
  let blob_bytes = direct::pull_with_endpoint(
    &b.endpoint,
    DirectRequest::Blob { session, blob },
    vec![a.endpoint.id()],
    Duration::from_secs(5),
  )
  .await?;
  assert_eq!(blob_bytes, large_update);

  a.stop_swarm().await;
  let b_update = b"b-after-a-left".to_vec();
  b.publish(PublishPayload::Update(b_update.clone())).await?;
  recv_update(&c.events, &b_update).await?;
  let c_update = b"c-after-a-left".to_vec();
  c.publish(PublishPayload::Update(c_update.clone())).await?;
  recv_update(&b.events, &c_update).await?;

  c.stop_swarm().await;
  c.start_swarm(session, vec![b_addr])?;
  b.publish(PublishPayload::Digest {
    vv: b"digest-after-resubscribe".to_vec(),
  })
  .await?;
  recv_digest(&c.events).await?;

  a.router
    .shutdown()
    .await
    .context("shutting down A router failed")?;
  b.router
    .shutdown()
    .await
    .context("shutting down B router failed")?;
  c.router
    .shutdown()
    .await
    .context("shutting down C router failed")?;
  auth::clear_local_admission(session);
  Ok(())
}

async fn install_snapshot_handler(peer: &Peer, session: SessionId, snapshot: Vec<u8>) {
  let (request_tx, request_rx) = async_channel::unbounded();
  peer
    .direct_state
    .register_handler(session, DirectSessionHandler::new(request_tx, None))
    .await;
  tokio::spawn(async move {
    while let Ok(request) = request_rx.recv().await {
      if let DirectServeRequest::Snapshot { reply } = request {
        let _ = reply.send(Ok(snapshot.clone())).await;
      }
    }
  });
}

async fn wait_addr(endpoint: &Endpoint) -> EndpointAddr {
  let mut watcher = endpoint.watch_addr();
  let wait = async {
    loop {
      let addr = watcher.get();
      if !addr.is_empty() {
        return addr;
      }
      if watcher.updated().await.is_err() {
        return endpoint.addr();
      }
    }
  };
  timeout(Duration::from_secs(3), wait)
    .await
    .unwrap_or_else(|_| endpoint.addr())
}

async fn recv_update(events: &Receiver<NetEvent>, expected: &[u8]) -> Result<()> {
  recv_matching(
    events,
    "update",
    |event| matches!(event, NetEvent::Gossip { msg: GossipMsg::Update(bytes), .. } if bytes == expected),
  )
  .await
  .map(|_| ())
}

async fn recv_presence(events: &Receiver<NetEvent>) -> Result<()> {
  recv_matching(events, "presence", |event| {
    matches!(
      event,
      NetEvent::Gossip {
        msg: GossipMsg::Presence(_),
        ..
      }
    )
  })
  .await
  .map(|_| ())
}

async fn recv_neighbor(events: &Receiver<NetEvent>) -> Result<()> {
  recv_matching(events, "neighbor", |event| matches!(event, NetEvent::NeighborUp { .. }))
    .await
    .map(|_| ())
}

async fn recv_update_available(events: &Receiver<NetEvent>, expected_len: u64) -> Result<flowstate_collab::BlobId> {
  let event = recv_matching(
    events,
    "update-available",
    |event| matches!(event, NetEvent::Gossip { msg: GossipMsg::UpdateAvailable { len, .. }, .. } if *len == expected_len),
  )
  .await?;
  let NetEvent::Gossip {
    msg: GossipMsg::UpdateAvailable { blob, .. },
    ..
  } = event
  else {
    bail!("matched event was not update-available");
  };
  Ok(blob)
}

async fn recv_digest(events: &Receiver<NetEvent>) -> Result<()> {
  recv_matching(events, "digest", |event| {
    matches!(
      event,
      NetEvent::Gossip {
        msg: GossipMsg::Digest { .. },
        ..
      }
    )
  })
  .await
  .map(|_| ())
}

async fn recv_matching<F>(events: &Receiver<NetEvent>, label: &str, matches: F) -> Result<NetEvent>
where
  F: Fn(&NetEvent) -> bool,
{
  timeout(Duration::from_secs(10), async {
    loop {
      let event = events.recv().await.context("event channel closed")?;
      if matches(&event) {
        return Ok(event);
      }
    }
  })
  .await
  .with_context(|| format!("timed out waiting for {label}"))?
}
