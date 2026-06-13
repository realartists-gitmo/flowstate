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
    let mut state = AntiEntropyState::new(session, Instant::now());

    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::SenderHasMissingOps, vec![1, 2, 3]),
      GapAction::Pull {
        from: peer,
        our_vv: vec![1, 2, 3],
      }
    );
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::Concurrent, vec![4, 5]),
      GapAction::None
    );

    state.finish_pull();
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::Concurrent, vec![6]),
      GapAction::Pull {
        from: peer,
        our_vv: vec![6],
      }
    );
  }

  #[test]
  fn equal_or_sender_behind_digest_does_not_pull() {
    let session = SessionId::from_bytes([2; 32]);
    let peer = peer(10);
    let mut state = AntiEntropyState::new(session, Instant::now());

    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::Equal, vec![1]),
      GapAction::None
    );
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::WeHaveMissingOps, vec![2]),
      GapAction::None
    );
  }

  #[test]
  fn lineage_mismatch_is_reported_without_starting_a_pull() {
    let session = SessionId::from_bytes([3; 32]);
    let other = SessionId::from_bytes([4; 32]);
    let peer = peer(11);
    let mut state = AntiEntropyState::new(session, Instant::now());

    assert_eq!(
      state.consider_digest(peer, other, VersionVectorRelation::SenderHasMissingOps, vec![1]),
      GapAction::LineageMismatch {
        from: peer,
        expected: session,
        got: other,
      }
    );
    assert_eq!(
      state.consider_digest(peer, session, VersionVectorRelation::SenderHasMissingOps, vec![2]),
      GapAction::Pull {
        from: peer,
        our_vv: vec![2],
      }
    );
  }

  #[test]
  fn lagged_uses_the_fallback_peer_and_deduplicates_in_flight_pulls() {
    let session = SessionId::from_bytes([5; 32]);
    let peer = peer(12);
    let mut state = AntiEntropyState::new(session, Instant::now());

    assert_eq!(state.on_lagged(None, vec![1]), GapAction::None);
    assert_eq!(
      state.on_lagged(Some(peer), vec![2]),
      GapAction::Pull {
        from: peer,
        our_vv: vec![2],
      }
    );
    assert_eq!(state.on_lagged(Some(peer), vec![3]), GapAction::None);
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
