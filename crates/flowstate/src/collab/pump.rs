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
      return;
    }
    self.event_pump_started = true;
    cx.spawn(async move |_, cx| {
      while let Ok(event) = events.recv().await {
        if cx
          .update_global::<CollabManager, _>(|manager, cx| manager.handle_net_event(event, cx))
          .is_err()
        {
          break;
        }
      }
    })
    .detach();
  }

  fn handle_net_event(&mut self, event: NetEvent, cx: &mut App) {
    match event {
      NetEvent::EndpointOnline(online) => {
        self.endpoint_online = online;
        for session in self.sessions_by_id.values() {
          session.update(cx, |session, cx| session.set_endpoint_online(online, cx));
        }
      },
      NetEvent::Gossip { session, from, msg } => {
        self.update_session(session, cx, |session, cx| session.handle_gossip(from, msg, cx));
      },
      NetEvent::IncompatibleVersion { session, peer } => {
        self.update_session(session, cx, |session, cx| session.handle_incompatible_version(peer, cx));
      },
      NetEvent::NeighborUp { session, peer } => {
        self.update_session(session, cx, |session, cx| session.neighbor_up(peer, cx));
      },
      NetEvent::NeighborDown { session, peer } => {
        self.update_session(session, cx, |session, cx| session.neighbor_down(peer, cx));
      },
      NetEvent::GossipLagged { session } => {
        self.update_session(session, cx, |session, cx| session.handle_gossip_lagged(cx));
      },
      NetEvent::SubscribeFailed { session, error } => {
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
  }
}
