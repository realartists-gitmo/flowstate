#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use flowstate_collab::{
    SessionId,
    net::anti_entropy::{AntiEntropyState, GapAction, VersionVectorRelation},
  };
  use iroh::{EndpointId, SecretKey};

  #[test]
  fn digest_gap_starts_one_pull_until_the_pull_finishes() {
    let session = SessionId::from_bytes([1; 32]);
    let peer = peer(9);
    let now = Instant::now();
    let mut state = AntiEntropyState::new(session, now);

    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::SenderHasMissingOps, vec![1, 2, 3], now),
      GapAction::Pull {
        from: peer,
        our_vv: vec![1, 2, 3],
      }
    );
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::Concurrent, vec![4, 5], now),
      GapAction::None
    );

    state.finish_pull(peer);
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::Concurrent, vec![6], now),
      GapAction::Pull { from: peer, our_vv: vec![6] }
    );
  }

  #[test]
  fn equal_or_sender_behind_digest_does_not_pull() {
    let session = SessionId::from_bytes([2; 32]);
    let peer = peer(10);
    let now = Instant::now();
    let mut state = AntiEntropyState::new(session, now);

    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::Equal, vec![1], now),
      GapAction::None
    );
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::WeHaveMissingOps, vec![2], now),
      GapAction::None
    );
  }

  #[test]
  fn lineage_mismatch_is_reported_without_starting_a_pull() {
    let session = SessionId::from_bytes([3; 32]);
    let other = SessionId::from_bytes([4; 32]);
    let peer = peer(11);
    let now = Instant::now();
    let mut state = AntiEntropyState::new(session, now);

    assert_eq!(
      state.consider_digest(peer, other, VersionVectorRelation::SenderHasMissingOps, vec![1], now),
      GapAction::LineageMismatch {
        from: peer,
        expected: session,
        got: other,
      }
    );
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::SenderHasMissingOps, vec![2], now),
      GapAction::Pull { from: peer, our_vv: vec![2] }
    );
  }

  #[test]
  fn lagged_uses_the_fallback_peer_and_deduplicates_in_flight_pulls() {
    let session = SessionId::from_bytes([5; 32]);
    let peer = peer(12);
    let now = Instant::now();
    let mut state = AntiEntropyState::new(session, now);

    assert_eq!(state.on_lagged(None, vec![1], now), GapAction::None);
    assert_eq!(state.on_lagged(Some(peer), vec![2], now), GapAction::Pull { from: peer, our_vv: vec![2] });
    assert_eq!(state.on_lagged(Some(peer), vec![3], now), GapAction::None);
  }

  #[test]
  fn a_pull_in_flight_for_one_peer_does_not_block_another_peer() {
    let session = SessionId::from_bytes([7; 32]);
    let peer_a = peer(20);
    let peer_b = peer(21);
    let now = Instant::now();
    let mut state = AntiEntropyState::new(session, now);

    assert_eq!(
      state.consider_digest(peer_a, session, VersionVectorRelation::SenderHasMissingOps, vec![1], now),
      GapAction::Pull { from: peer_a, our_vv: vec![1] }
    );
    // A second gap for A while its pull is outstanding is deduplicated...
    assert_eq!(
      state.consider_digest(peer_a, session, VersionVectorRelation::SenderHasMissingOps, vec![2], now),
      GapAction::None
    );
    // ...but a gap with a different peer B is served concurrently.
    assert_eq!(
      state.consider_digest(peer_b, session, VersionVectorRelation::SenderHasMissingOps, vec![3], now),
      GapAction::Pull { from: peer_b, our_vv: vec![3] }
    );
    // Finishing A's pull only frees A's dedup slot.
    state.finish_pull(peer_a);
    assert_eq!(
      state.consider_digest(peer_b, session, VersionVectorRelation::SenderHasMissingOps, vec![4], now),
      GapAction::None
    );
    assert_eq!(
      state.consider_digest(peer_a, session, VersionVectorRelation::SenderHasMissingOps, vec![5], now),
      GapAction::Pull { from: peer_a, our_vv: vec![5] }
    );
  }

  #[test]
  fn a_hung_pull_expires_after_its_deadline_so_the_peer_is_retried() {
    let session = SessionId::from_bytes([8; 32]);
    let peer_a = peer(22);
    let peer_b = peer(23);
    let start = Instant::now();
    let mut state = AntiEntropyState::new(session, start);

    // A pull to A starts and then wedges: `finish_pull` is never called.
    assert_eq!(
      state.consider_digest(peer_a, session, VersionVectorRelation::SenderHasMissingOps, vec![1], start),
      GapAction::Pull { from: peer_a, our_vv: vec![1] }
    );
    // Before the deadline, A stays deduplicated while B is unaffected.
    let soon = start + Duration::from_secs(1);
    assert_eq!(
      state.consider_digest(peer_a, session, VersionVectorRelation::SenderHasMissingOps, vec![2], soon),
      GapAction::None
    );
    assert_eq!(
      state.consider_digest(peer_b, session, VersionVectorRelation::SenderHasMissingOps, vec![3], soon),
      GapAction::Pull { from: peer_b, our_vv: vec![3] }
    );
    // Past the deadline, A's wedged pull is force-expired and A is retried even
    // though its original pull was never finished.
    let past_deadline = start + Duration::from_secs(120);
    assert_eq!(
      state.consider_digest(peer_a, session, VersionVectorRelation::SenderHasMissingOps, vec![4], past_deadline),
      GapAction::Pull { from: peer_a, our_vv: vec![4] }
    );
  }

  #[test]
  fn neighbor_and_reconnect_events_schedule_an_immediate_digest() {
    let session = SessionId::from_bytes([6; 32]);
    let now = Instant::now();
    let mut state = AntiEntropyState::new(session, now);

    assert!(!state.digest_due(now + Duration::from_secs(1)));
    state.on_neighbor_up();
    assert!(state.digest_due(Instant::now() + Duration::from_millis(1)));

    state.mark_digest_sent(Instant::now());
    assert!(!state.digest_due(Instant::now() + Duration::from_secs(1)));
    state.on_reconnect();
    assert!(state.digest_due(Instant::now() + Duration::from_millis(1)));
  }

  fn peer(seed: u8) -> EndpointId {
    SecretKey::from_bytes(&[seed; 32]).public()
  }
}
