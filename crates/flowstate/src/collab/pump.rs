use flowstate_collab::{
  SessionId,
  net::{NetEvent, runtime},
};
use gpui::{App, Context};

use super::{
  manager::CollabManager,
  session::{CollabSession, DetachReason},
};

impl CollabManager {
  pub(super) fn start_event_pump<T>(&mut self, events: runtime::EventReceiver, cx: &mut Context<T>)
  where
    T: 'static,
  {
    if self.event_pump_started {
      tracing::trace!("collaboration network event pump already started");
      return;
    }
    self.event_pump_started = true;
    tracing::debug!("starting collaboration network event pump");
    cx.spawn(async move |_, cx| {
      while let Ok(event) = events.recv().await {
        if cx
          .update_global::<CollabManager, _>(|manager, cx| manager.handle_net_event(event, cx))
          .is_err()
        {
          tracing::warn!("collaboration network event pump could not update manager; stopping");
          break;
        }
      }
      tracing::debug!("collaboration network event pump stopped");
    })
    .detach();
  }

  fn handle_net_event(&mut self, event: NetEvent, cx: &mut App) {
    match event {
      NetEvent::EndpointOnline(online) => {
        tracing::info!(online, "collaboration endpoint status changed");
        self.endpoint_online = online;
        for session in self.sessions_by_id.values() {
          session.update(cx, |session, cx| session.set_endpoint_online(online, cx));
        }
      },
      NetEvent::Gossip { session, from, msg } => {
        tracing::trace!(%session, from = %from, gossip_kind = msg.kind(), gossip_payload_bytes = msg.payload_len(), "routing collaboration gossip event");
        self.update_session(session, cx, |session, cx| session.handle_gossip(from, msg, cx));
      },
      NetEvent::IncompatibleVersion { session, peer } => {
        tracing::warn!(%session, peer = %peer, "routing collaboration incompatible-version event");
        self.update_session(session, cx, |session, cx| session.handle_incompatible_version(peer, cx));
      },
      NetEvent::AdmissionRefused { session, identity } => {
        tracing::warn!(%session, %identity, "routing collaboration admission-refused event");
        self.update_session(session, cx, |session, cx| session.handle_admission_refused(identity, cx));
      },
      NetEvent::NeighborUp { session, peer } => {
        tracing::debug!(%session, peer = %peer, "routing collaboration neighbor-up event");
        self.update_session(session, cx, |session, cx| session.neighbor_up(peer, cx));
      },
      NetEvent::NeighborDown { session, peer } => {
        tracing::debug!(%session, peer = %peer, "routing collaboration neighbor-down event");
        self.update_session(session, cx, |session, cx| session.neighbor_down(peer, cx));
      },
      NetEvent::GossipLagged { session } => {
        tracing::warn!(%session, "routing collaboration gossip-lagged event");
        self.update_session(session, cx, |session, cx| session.handle_gossip_lagged(cx));
      },
      NetEvent::SubscribeFailed { session, error } => {
        tracing::error!(%session, error = %error, "collaboration gossip subscription failed");
        let detached = self.update_session(session, cx, |session, cx| session.detach(DetachReason::Fatal(error), cx));
        if detached.unwrap_or(false) {
          self.unregister_session(session);
        }
      },
    }
  }

  fn update_session<R>(
    &mut self,
    session: SessionId,
    cx: &mut App,
    update: impl FnOnce(&mut CollabSession, &mut Context<CollabSession>) -> R,
  ) -> Option<R> {
    self
      .sessions_by_id
      .get(&session)
      .cloned()
      .map(|session| session.update(cx, update))
      .or_else(|| {
        tracing::warn!(%session, "collaboration event targeted missing session");
        None
      })
  }
}
