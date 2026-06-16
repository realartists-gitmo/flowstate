use std::time::{Duration, Instant};

use iroh::EndpointId;

use crate::ids::SessionId;

pub const DIGEST_INTERVAL: Duration = Duration::from_secs(10);
pub const DIGEST_JITTER_PERCENT: u32 = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionVectorRelation {
  Equal,
  SenderHasMissingOps,
  WeHaveMissingOps,
  Concurrent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GapAction {
  None,
  Pull { from: EndpointId, our_vv: Vec<u8> },
  LineageMismatch { from: EndpointId, expected: SessionId, got: SessionId },
}

#[derive(Clone, Debug)]
pub struct AntiEntropyState {
  session: SessionId,
  next_digest: Instant,
  pull_in_flight: bool,
}

impl AntiEntropyState {
  #[must_use]
  pub fn new(session: SessionId, now: Instant) -> Self {
    Self {
      session,
      next_digest: next_jittered_deadline(now),
      pull_in_flight: false,
    }
  }

  #[must_use]
  pub const fn session(&self) -> SessionId {
    self.session
  }

  #[must_use]
  pub fn digest_due(&self, now: Instant) -> bool {
    now >= self.next_digest
  }

  #[must_use]
  pub fn duration_until_digest(&self, now: Instant) -> Duration {
    self.next_digest.saturating_duration_since(now)
  }

  pub fn mark_digest_sent(&mut self, now: Instant) {
    self.next_digest = next_jittered_deadline(now);
    tracing::trace!(session = %self.session, next_digest_in = ?self.next_digest.saturating_duration_since(now), "collaboration anti-entropy digest sent");
  }

  pub fn schedule_immediate_digest(&mut self) {
    self.next_digest = Instant::now();
    tracing::trace!(session = %self.session, "collaboration anti-entropy digest scheduled immediately");
  }

  pub fn on_neighbor_up(&mut self) {
    tracing::debug!(session = %self.session, "collaboration anti-entropy neighbor-up recovery triggered");
    self.schedule_immediate_digest();
  }

  pub fn on_reconnect(&mut self) {
    tracing::debug!(session = %self.session, "collaboration anti-entropy reconnect recovery triggered");
    self.schedule_immediate_digest();
  }

  #[must_use]
  pub fn on_lagged(&mut self, fallback_peer: Option<EndpointId>, our_vv: Vec<u8>) -> GapAction {
    tracing::warn!(session = %self.session, fallback_peer = ?fallback_peer, our_vv_bytes = our_vv.len(), "collaboration anti-entropy gossip lagged");
    self.schedule_immediate_digest();
    let Some(from) = fallback_peer else {
      tracing::warn!(session = %self.session, "collaboration anti-entropy lagged without fallback peer");
      return GapAction::None;
    };
    self.begin_pull(from, our_vv)
  }

  #[must_use]
  pub fn consider_digest(&mut self, from: EndpointId, digest_session: SessionId, relation: VersionVectorRelation, our_vv: Vec<u8>) -> GapAction {
    tracing::trace!(
      session = %self.session,
      from = %from,
      digest_session = %digest_session,
      ?relation,
      our_vv_bytes = our_vv.len(),
      "collaboration anti-entropy digest considered",
    );
    if digest_session != self.session {
      tracing::warn!(
        session = %self.session,
        from = %from,
        digest_session = %digest_session,
        "collaboration anti-entropy digest lineage mismatch",
      );
      return GapAction::LineageMismatch {
        from,
        expected: self.session,
        got: digest_session,
      };
    }

    match relation {
      VersionVectorRelation::SenderHasMissingOps | VersionVectorRelation::Concurrent => self.begin_pull(from, our_vv),
      VersionVectorRelation::Equal | VersionVectorRelation::WeHaveMissingOps => {
        tracing::trace!(session = %self.session, from = %from, ?relation, "collaboration anti-entropy digest needs no pull");
        GapAction::None
      },
    }
  }

  pub fn finish_pull(&mut self) {
    tracing::debug!(session = %self.session, was_in_flight = self.pull_in_flight, "collaboration anti-entropy pull finished");
    self.pull_in_flight = false;
  }

  fn begin_pull(&mut self, from: EndpointId, our_vv: Vec<u8>) -> GapAction {
    if self.pull_in_flight {
      tracing::debug!(session = %self.session, from = %from, "collaboration anti-entropy skipped pull because one is already in flight");
      return GapAction::None;
    }
    self.pull_in_flight = true;
    tracing::debug!(session = %self.session, from = %from, our_vv_bytes = our_vv.len(), "collaboration anti-entropy pull started");
    GapAction::Pull { from, our_vv }
  }
}

#[must_use]
pub fn next_jittered_deadline(now: Instant) -> Instant {
  let base_millis = DIGEST_INTERVAL.as_millis() as u64;
  let jitter_millis = base_millis * u64::from(DIGEST_JITTER_PERCENT) / 100;
  let spread = jitter_millis.saturating_mul(2).saturating_add(1);
  let offset = rand::random::<u64>() % spread;
  let delay_millis = base_millis
    .saturating_add(offset)
    .saturating_sub(jitter_millis);
  now + Duration::from_millis(delay_millis)
}
