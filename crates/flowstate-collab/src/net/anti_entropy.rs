use std::{
  collections::HashMap,
  time::{Duration, Instant},
};

use iroh::EndpointId;

use crate::ids::SessionId;

pub const DIGEST_INTERVAL: Duration = Duration::from_secs(10);
pub const DIGEST_JITTER_PERCENT: u32 = 20;
/// FS-077: safety net for a wedged direct pull. A per-peer in-flight pull that
/// is never finished (e.g. the runtime reply is dropped) is force-expired after
/// this deadline, so one stuck peer can never block anti-entropy pulls to every
/// other peer.
pub const PULL_IN_FLIGHT_DEADLINE: Duration = Duration::from_secs(60);

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
  // FS-077: per-peer in-flight pulls keyed by peer, valued by an expiry
  // deadline. Replaces a single global `pull_in_flight` flag so one wedged peer
  // cannot starve pulls to the rest of the swarm, while still deduplicating a
  // second pull for the same peer while one is outstanding.
  pulls_in_flight: HashMap<EndpointId, Instant>,
}

impl AntiEntropyState {
  #[must_use]
  pub fn new(session: SessionId, now: Instant) -> Self {
    Self {
      session,
      next_digest: next_jittered_deadline(now),
      pulls_in_flight: HashMap::new(),
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
  pub fn on_lagged(&mut self, fallback_peer: Option<EndpointId>, our_vv: Vec<u8>, now: Instant) -> GapAction {
    tracing::warn!(session = %self.session, fallback_peer = ?fallback_peer, our_vv_bytes = our_vv.len(), "collaboration anti-entropy gossip lagged");
    self.schedule_immediate_digest();
    let Some(from) = fallback_peer else {
      tracing::warn!(session = %self.session, "collaboration anti-entropy lagged without fallback peer");
      return GapAction::None;
    };
    self.begin_pull(from, our_vv, now)
  }

  #[must_use]
  pub fn consider_digest(
    &mut self,
    from: EndpointId,
    digest_session: SessionId,
    relation: VersionVectorRelation,
    our_vv: Vec<u8>,
    now: Instant,
  ) -> GapAction {
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
      VersionVectorRelation::SenderHasMissingOps | VersionVectorRelation::Concurrent => self.begin_pull(from, our_vv, now),
      VersionVectorRelation::Equal | VersionVectorRelation::WeHaveMissingOps => {
        tracing::trace!(session = %self.session, from = %from, ?relation, "collaboration anti-entropy digest needs no pull");
        GapAction::None
      },
    }
  }

  /// Mark the outstanding pull to `from` as finished, clearing its dedup slot so
  /// a fresh gap for that peer can start a new pull.
  pub fn finish_pull(&mut self, from: EndpointId) {
    let was_in_flight = self.pulls_in_flight.remove(&from).is_some();
    tracing::debug!(session = %self.session, from = %from, was_in_flight, in_flight = self.pulls_in_flight.len(), "collaboration anti-entropy pull finished");
  }

  /// Start a deduplicated pull to `from`: returns [`GapAction::Pull`] and registers
  /// an in-flight slot (with a [`PULL_IN_FLIGHT_DEADLINE`] expiry), or
  /// [`GapAction::None`] if a pull to that peer is already outstanding. Every pull —
  /// digest-driven or triggered by a pending-dependency import — MUST go through
  /// here so a single peer gets at most one in-flight pull, and so the matching
  /// [`Self::finish_pull`] clears a slot this actually set.
  pub fn begin_pull(&mut self, from: EndpointId, our_vv: Vec<u8>, now: Instant) -> GapAction {
    self.expire_stale_pulls(now);
    if self.pulls_in_flight.contains_key(&from) {
      tracing::debug!(session = %self.session, from = %from, "collaboration anti-entropy skipped pull because one is already in flight for this peer");
      return GapAction::None;
    }
    self.pulls_in_flight.insert(from, now + PULL_IN_FLIGHT_DEADLINE);
    tracing::debug!(session = %self.session, from = %from, our_vv_bytes = our_vv.len(), in_flight = self.pulls_in_flight.len(), "collaboration anti-entropy pull started");
    GapAction::Pull { from, our_vv }
  }

  /// Drop in-flight pulls whose deadline has passed. A pull whose reply is never
  /// delivered would otherwise pin its peer's dedup slot forever; expiring it
  /// lets anti-entropy retry that peer without waiting on the wedged request.
  fn expire_stale_pulls(&mut self, now: Instant) {
    let before = self.pulls_in_flight.len();
    self.pulls_in_flight.retain(|_, deadline| *deadline > now);
    let expired = before - self.pulls_in_flight.len();
    if expired > 0 {
      tracing::warn!(session = %self.session, expired, in_flight = self.pulls_in_flight.len(), "collaboration anti-entropy expired wedged in-flight pulls past their deadline");
    }
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
