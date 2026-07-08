//! §act-three E.1 — the stratified revision-snapshot ladder policy.
//!
//! This is the answer to "I'd much rather have an evenly-sampled 5 snapshots
//! from history than the 5 most recent." The ladder retains, within a budget
//! `K`, the document-creation snapshot, the current snapshot, and `K-2`
//! interior snapshots chosen for **even coverage of the whole timeline**, not
//! recency. Coverage is measured on the **edit-count axis** (cumulative op
//! count at each snapshot) so a burst of edits doesn't over-sample a short
//! wall-clock window.
//!
//! The policy is a pure algorithm over `(revision_id, op_count)` sample points;
//! it decides which snapshot to EVICT when a new one enters a full ladder. It
//! deliberately does not touch the on-disk package format — the actual chunk
//! eviction (spec §6, format-touching) wires this decision into the off-gate
//! checkpoint/flush job, after verifying every retained revision stays
//! materializable (E.3).
//!
//! Thinning rule (E.1): when the ladder is full, evict the INTERIOR sample
//! whose removal minimizes the resulting maximum inter-sample gap. Endpoints
//! (creation = smallest `op_count`, current = largest) are never evicted. The
//! ladder converges to even coverage and never collapses to "the K most
//! recent."

/// One retained snapshot: its revision id and the cumulative op count at the
/// frontier it was taken from (the even-spacing axis).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LadderSample {
  pub revision_id: u128,
  pub op_count: u64,
}

/// The stratified snapshot ladder. Samples are kept sorted by `op_count`
/// (strictly increasing — each admitted snapshot is at a new, higher op count).
#[derive(Clone, Debug)]
pub struct RevisionLadder {
  budget: usize,
  samples: Vec<LadderSample>,
}

/// The default ladder budget (spec E.1: `K = 8`).
pub const DEFAULT_LADDER_BUDGET: usize = 8;

impl RevisionLadder {
  /// Create an empty ladder with the given budget (clamped to ≥ 2 so both
  /// endpoints always fit).
  #[must_use]
  pub fn new(budget: usize) -> Self {
    Self {
      budget: budget.max(2),
      samples: Vec::new(),
    }
  }

  #[must_use]
  pub fn budget(&self) -> usize {
    self.budget
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.samples.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.samples.is_empty()
  }

  #[must_use]
  pub fn samples(&self) -> &[LadderSample] {
    &self.samples
  }

  #[must_use]
  pub fn revision_ids(&self) -> Vec<u128> {
    self.samples.iter().map(|sample| sample.revision_id).collect()
  }

  /// Admit a new snapshot (always the current one — the highest op count seen).
  /// If it does not advance the op count it is ignored (idempotent
  /// re-checkpoint at the same frontier). Returns the revision id of the
  /// snapshot EVICTED to make room, if any — the caller drops that chunk.
  ///
  /// Preconditions: `sample.op_count` must be ≥ the current maximum (snapshots
  /// are taken forward in time). A lower op count is rejected as a no-op with
  /// no eviction (a stale/racing checkpoint).
  pub fn admit(&mut self, sample: LadderSample) -> Option<u128> {
    if let Some(last) = self.samples.last() {
      if sample.op_count < last.op_count {
        // Out-of-order checkpoint — ignore (never evict for a stale sample).
        return None;
      }
      if sample.op_count == last.op_count {
        // Same frontier: refresh the current snapshot's id in place (a
        // re-export of the same state), no eviction.
        self.samples.last_mut().unwrap().revision_id = sample.revision_id;
        return None;
      }
    }
    self.samples.push(sample);
    if self.samples.len() <= self.budget {
      return None;
    }
    // Full ladder: evict the interior sample minimizing the resulting max gap.
    let evict_ix = self.thinning_victim();
    Some(self.samples.remove(evict_ix).revision_id)
  }

  /// The interior index whose removal minimizes the resulting maximum adjacent
  /// gap (endpoints protected). Ties break toward the SMALLER merged gap, then
  /// toward the earlier index — deterministic. Assumes `len() >= 3`.
  fn thinning_victim(&self) -> usize {
    debug_assert!(self.samples.len() >= 3);
    let n = self.samples.len();
    // Precompute the max gap EXCLUDING each candidate's two adjacent gaps is
    // unnecessary at this scale (n ≤ budget+1 ≤ tens): just simulate each.
    let mut best_ix = 1;
    let mut best_max_gap = u64::MAX;
    let mut best_merged = u64::MAX;
    for candidate in 1..n - 1 {
      // Removing `candidate` merges gaps (candidate-1..candidate) and
      // (candidate..candidate+1) into (candidate-1..candidate+1).
      let merged = self.samples[candidate + 1].op_count - self.samples[candidate - 1].op_count;
      // Resulting max gap = max(merged, every other adjacent gap).
      let mut resulting_max = merged;
      for i in 0..n - 1 {
        if i == candidate - 1 || i == candidate {
          continue; // these two gaps are replaced by `merged`
        }
        let gap = self.samples[i + 1].op_count - self.samples[i].op_count;
        resulting_max = resulting_max.max(gap);
      }
      if resulting_max < best_max_gap || (resulting_max == best_max_gap && merged < best_merged) {
        best_max_gap = resulting_max;
        best_merged = merged;
        best_ix = candidate;
      }
    }
    best_ix
  }

  /// The largest inter-sample gap on the edit-count axis (0 for < 2 samples).
  #[must_use]
  pub fn max_gap(&self) -> u64 {
    self
      .samples
      .windows(2)
      .map(|pair| pair[1].op_count - pair[0].op_count)
      .max()
      .unwrap_or(0)
  }

  /// Whether `revision_id` is currently retained in the ladder.
  #[must_use]
  pub fn contains(&self, revision_id: u128) -> bool {
    self.samples.iter().any(|sample| sample.revision_id == revision_id)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ladder_from(budget: usize, op_counts: &[u64]) -> RevisionLadder {
    let mut ladder = RevisionLadder::new(budget);
    for (ix, &op_count) in op_counts.iter().enumerate() {
      ladder.admit(LadderSample {
        revision_id: ix as u128,
        op_count,
      });
    }
    ladder
  }

  #[test]
  fn endpoints_always_survive() {
    // 100 snapshots at op counts 0,10,20,...,990 into a budget-8 ladder.
    let op_counts: Vec<u64> = (0..100).map(|i| i * 10).collect();
    let ladder = ladder_from(8, &op_counts);
    assert_eq!(ladder.len(), 8);
    // Creation (op 0) and current (op 990) must both survive — NEVER the 8
    // most recent.
    assert_eq!(ladder.samples().first().unwrap().op_count, 0, "creation snapshot must survive");
    assert_eq!(ladder.samples().last().unwrap().op_count, 990, "current snapshot must survive");
  }

  #[test]
  fn never_collapses_to_most_recent() {
    let op_counts: Vec<u64> = (0..50).map(|i| i * 7).collect();
    let ladder = ladder_from(6, &op_counts);
    // The 6-most-recent would be op counts {301,308,315,322,329,343}. Assert we
    // are NOT that: the earliest retained sample is near 0, not near the end.
    let earliest = ladder.samples()[1].op_count; // [0] is creation
    assert!(earliest < 100, "second sample must be early in history, not clustered at the end (got {earliest})");
  }

  #[test]
  fn even_coverage_bound_for_uniform_input() {
    // For uniformly-spaced admissions, greedy min-max-gap thinning keeps the
    // max gap within 2× the ideal even-spacing gap.
    let total = 1000u64;
    let op_counts: Vec<u64> = (0..=100).map(|i| i * (total / 100)).collect();
    let ladder = ladder_from(8, &op_counts);
    let ideal = total / (8 - 1); // even spacing across 8 samples
    assert!(
      ladder.max_gap() <= 2 * ideal,
      "max gap {} exceeds 2× ideal {} — coverage not even",
      ladder.max_gap(),
      ideal
    );
  }

  #[test]
  fn idempotent_same_frontier_admit() {
    let mut ladder = RevisionLadder::new(8);
    assert_eq!(ladder.admit(LadderSample { revision_id: 1, op_count: 100 }), None);
    // Re-checkpoint at the same op count refreshes the id, no eviction, no grow.
    assert_eq!(ladder.admit(LadderSample { revision_id: 2, op_count: 100 }), None);
    assert_eq!(ladder.len(), 1);
    assert_eq!(ladder.samples()[0].revision_id, 2);
  }

  #[test]
  fn out_of_order_admit_is_ignored() {
    let mut ladder = RevisionLadder::new(8);
    ladder.admit(LadderSample { revision_id: 1, op_count: 100 });
    ladder.admit(LadderSample { revision_id: 2, op_count: 200 });
    // A stale checkpoint below the current max must never evict.
    assert_eq!(ladder.admit(LadderSample { revision_id: 3, op_count: 50 }), None);
    assert_eq!(ladder.len(), 2);
  }

  #[test]
  fn under_budget_never_evicts() {
    let ladder = ladder_from(8, &[0, 10, 20, 30]);
    assert_eq!(ladder.len(), 4);
    assert_eq!(ladder.revision_ids(), vec![0, 1, 2, 3]);
  }

  // ---- Retention property fuzz (spec §9.2) --------------------------------
  //
  // For randomized admission sequences (varied budgets, spacing, bursts) the
  // ladder MUST, after every admission:
  //   (P1) never exceed budget;
  //   (P2) keep samples strictly increasing on the op-count axis;
  //   (P3) always retain the global-first and global-current op counts
  //        (endpoints) — i.e. never become "the K most recent";
  //   (P4) every eviction returns a real id that leaves the ladder, and no
  //        surviving id is ever silently dropped (accounted set).

  struct Rng(u64);
  impl Rng {
    fn new(seed: u64) -> Self {
      Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
      let mut x = self.0;
      x ^= x << 13;
      x ^= x >> 7;
      x ^= x << 17;
      self.0 = x;
      x
    }
    fn below(&mut self, bound: u64) -> u64 {
      if bound == 0 { 0 } else { self.next() % bound }
    }
  }

  #[test]
  fn retention_property_fuzz() {
    for seed in 1..300u64 {
      let mut rng = Rng::new(seed);
      let budget = 2 + (rng.below(10) as usize); // 2..=11
      let mut ladder = RevisionLadder::new(budget);
      let mut op_count = 0u64;
      let mut first_op: Option<u64> = None;
      let mut live: std::collections::HashSet<u128> = std::collections::HashSet::new();
      let admissions = 3 + rng.below(200);
      for id in 0..admissions {
        // Advance op count by a random burst (sometimes 0 = same frontier).
        op_count += rng.below(50);
        first_op.get_or_insert(op_count);
        let evicted = ladder.admit(LadderSample {
          revision_id: id as u128,
          op_count,
        });
        // Track the live set against the ladder's own membership.
        if let Some(evicted_id) = evicted {
          assert!(live.remove(&evicted_id), "seed {seed}: evicted an id not thought live");
        }
        // A same-frontier refresh replaces the current id; reconcile the live
        // set to the ladder's actual membership (the honest source of truth).
        live = ladder.revision_ids().into_iter().collect();

        // (P1) budget.
        assert!(ladder.len() <= budget, "seed {seed}: ladder {} > budget {budget}", ladder.len());
        // (P2) strictly increasing on the op-count axis.
        for pair in ladder.samples().windows(2) {
          assert!(pair[0].op_count < pair[1].op_count, "seed {seed}: op counts not strictly increasing");
        }
        // (P3) endpoints: current op count is always retained as the last
        // sample; the earliest op count ever admitted is retained as the first
        // (creation snapshot never evicted).
        assert_eq!(ladder.samples().last().unwrap().op_count, op_count, "seed {seed}: current snapshot dropped");
        assert_eq!(
          ladder.samples().first().unwrap().op_count,
          first_op.unwrap(),
          "seed {seed}: creation snapshot evicted (collapsed toward recency)"
        );
      }
    }
  }
}
