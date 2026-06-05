use super::*;

#[cfg_attr(not(test), allow(dead_code, reason = "staged state-machine helper for the live collaboration refactor"))] // staged state-machine helper for the live collaboration refactor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HeartbeatState {
  interval_millis: u64,
  last_sent_millis: Option<u64>,
  last_received_millis: Option<u64>,
}

#[cfg_attr(not(test), allow(dead_code, reason = "staged state-machine helper for the live collaboration refactor"))] // staged state-machine helper for the live collaboration refactor.
impl HeartbeatState {
  #[must_use]
  pub(crate) const fn new(interval_millis: u64) -> Self {
    Self {
      interval_millis,
      last_sent_millis: None,
      last_received_millis: None,
    }
  }

  pub(crate) fn record_sent(&mut self, now_millis: u64) {
    self.last_sent_millis = Some(now_millis);
  }

  pub(crate) fn record_received(&mut self, now_millis: u64) {
    self.last_received_millis = Some(now_millis);
  }

  #[must_use]
  pub(crate) fn should_send(&self, now_millis: u64) -> bool {
    self
      .last_sent_millis
      .is_none_or(|sent| now_millis.saturating_sub(sent) >= self.interval_millis)
  }

  #[must_use]
  pub(crate) fn is_expired(&self, now_millis: u64, timeout_millis: u64) -> bool {
    self
      .last_received_millis
      .is_some_and(|received| now_millis.saturating_sub(received) >= timeout_millis)
  }
}

#[cfg_attr(not(test), allow(dead_code, reason = "staged state-machine helper for the live collaboration refactor"))] // staged state-machine helper for the live collaboration refactor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AckTracker {
  last_ack_frontier: Option<Vec<u8>>,
  last_ack_hash: Option<[u8; 32]>,
}

#[cfg_attr(not(test), allow(dead_code, reason = "staged state-machine helper for the live collaboration refactor"))] // staged state-machine helper for the live collaboration refactor.
impl AckTracker {
  #[must_use]
  pub(crate) fn should_ack(&self, frontier: &[u8], hash: [u8; 32]) -> bool {
    self.last_ack_frontier.as_deref() != Some(frontier) || self.last_ack_hash != Some(hash)
  }

  pub(crate) fn record_ack(&mut self, frontier: Vec<u8>, hash: [u8; 32]) {
    self.last_ack_frontier = Some(frontier);
    self.last_ack_hash = Some(hash);
  }
}

#[cfg_attr(not(test), allow(dead_code, reason = "staged state-machine helper for the live collaboration refactor"))] // staged state-machine helper for the live collaboration refactor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct IdempotencyTracker {
  last_session_id: Option<SessionId>,
  last_update_hash: Option<[u8; 32]>,
}

#[cfg_attr(not(test), allow(dead_code, reason = "staged state-machine helper for the live collaboration refactor"))] // staged state-machine helper for the live collaboration refactor.
impl IdempotencyTracker {
  #[must_use]
  pub(crate) fn should_process(&self, session_id: SessionId, update_hash: [u8; 32]) -> bool {
    self.last_session_id != Some(session_id) || self.last_update_hash != Some(update_hash)
  }

  pub(crate) fn record(&mut self, session_id: SessionId, update_hash: [u8; 32]) {
    self.last_session_id = Some(session_id);
    self.last_update_hash = Some(update_hash);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn heartbeat_state_tracks_send_and_timeout() {
    let mut state = HeartbeatState::new(100);
    assert!(state.should_send(0));
    state.record_sent(10);
    assert!(!state.should_send(50));
    assert!(state.should_send(110));
    assert!(!state.is_expired(50, 100));
    state.record_received(25);
    assert!(!state.is_expired(120, 100));
    assert!(state.is_expired(125, 100));
  }

  #[test]
  fn ack_tracker_is_deduplicated_by_frontier_and_hash() {
    let mut tracker = AckTracker::default();
    let frontier = vec![1, 2, 3];
    let hash = [7; 32];
    assert!(tracker.should_ack(&frontier, hash));
    tracker.record_ack(frontier.clone(), hash);
    assert!(!tracker.should_ack(&frontier, hash));
    assert!(tracker.should_ack(&[1, 2, 4], hash));
  }

  #[test]
  fn idempotency_tracker_keys_on_session_and_hash() {
    let mut tracker = IdempotencyTracker::default();
    let session = SessionId::new();
    let hash = [9; 32];
    assert!(tracker.should_process(session, hash));
    tracker.record(session, hash);
    assert!(!tracker.should_process(session, hash));
    assert!(tracker.should_process(SessionId::new(), hash));
  }
}
