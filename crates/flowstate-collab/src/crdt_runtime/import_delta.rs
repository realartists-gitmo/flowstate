//! Exact remote-import body-delta composition (Loro-first spec §6.3-6.4).
//!
//! One remote import can apply several Loro change batches, each emitting a
//! subscription event whose text diff is positioned against the doc state at
//! that batch's application point. Treating those per-batch positions as if
//! they shared one coordinate space is how the old runtime's lossy summaries
//! missed structural deletes and grew an O(doc) body-stringify backstop.
//!
//! This module composes the per-batch deltas into ONE net delta
//! (pre-import space → post-import space), from which the import path derives
//! exact answers in O(P + ops):
//! * which pre-import positions were deleted (→ structural-delete detection by
//!   binary search against the maintained boundary/object indexes);
//! * whether any inserted text carries structure (`\n`/U+FFFC);
//! * where every pre-import paragraph start lands post-import (position
//!   shifting — replaces the full-body rescan);
//! * the changed post-import ranges (invalidation + touched paragraphs).
//!
//! Copied-out data only: everything here operates on plain ops harvested from
//! the subscription callback; nothing touches the doc (spec I-9b).

/// One net-delta op over the body flow (unicode-codepoint units).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NetOp {
  Retain(usize),
  Insert { len: usize, structure: bool },
  Delete(usize),
}

/// The composed pre→post delta for one import, plus conservatism flags.
#[derive(Clone, Debug, Default)]
pub(crate) struct NetBodyDelta {
  pub ops: Vec<NetOp>,
  /// A structural insert was later (partially) deleted within the same
  /// import. The net delta can no longer prove structure-neutrality, so the
  /// caller must take the full-rebuild path. Rare; correctness over speed.
  pub structural_churn: bool,
}

impl NetBodyDelta {
  /// No content change (pure retains) — the import was projection-neutral.
  pub(crate) fn is_empty(&self) -> bool {
    self.ops.iter().all(|op| matches!(op, NetOp::Retain(_)))
  }

  /// Any inserted run in the net delta carries a paragraph boundary or object
  /// placeholder.
  pub(crate) fn inserts_structure(&self) -> bool {
    self
      .ops
      .iter()
      .any(|op| matches!(op, NetOp::Insert { structure: true, .. }))
  }

  /// Deleted ranges in PRE-import coordinates, merged and ordered.
  pub(crate) fn deleted_pre_ranges(&self) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut old_pos = 0usize;
    for op in &self.ops {
      match op {
        NetOp::Retain(n) => old_pos += n,
        NetOp::Insert { .. } => {},
        NetOp::Delete(n) => {
          ranges.push((old_pos, *n));
          old_pos += n;
        },
      }
    }
    ranges
  }

  /// Spec §6-R: the PRE-import unicode span this net delta touches — `(lo, hi)`
  /// covering every deleted range and every insertion point. `None` when the
  /// delta is retain-only.
  pub(crate) fn pre_change_span(&self) -> Option<(usize, usize)> {
    let mut old_pos = 0_usize;
    let mut lo = None;
    let mut hi: Option<usize> = None;
    for op in &self.ops {
      match op {
        NetOp::Retain(n) => old_pos += n,
        NetOp::Insert { .. } => {
          lo.get_or_insert(old_pos);
          hi = Some(hi.map_or(old_pos, |current| current.max(old_pos)));
        },
        NetOp::Delete(n) => {
          lo.get_or_insert(old_pos);
          let end = old_pos + n;
          hi = Some(hi.map_or(end, |current| current.max(end)));
          old_pos = end;
        },
      }
    }
    Some((lo?, hi?))
  }

  /// True when any deleted pre-import range covers one of `positions`
  /// (sorted ascending). O(ranges · log positions).
  pub(crate) fn deletes_any_position(&self, positions: &[usize]) -> bool {
    self.deleted_pre_ranges().iter().any(|(start, len)| {
      if *len == 0 {
        return false;
      }
      let from = positions.partition_point(|p| p < start);
      positions.get(from).is_some_and(|p| *p < start + len)
    })
  }

  /// Map sorted pre-import positions into post-import coordinates. Positions
  /// inside a deleted range clamp to the deletion point (callers only shift
  /// positions they have already proven undeleted).
  pub(crate) fn shift_positions(&self, positions: &[usize]) -> Vec<usize> {
    let mut shifted = Vec::with_capacity(positions.len());
    let mut op_ix = 0usize;
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;
    for &position in positions {
      // Advance through ops until `position` falls before/inside the current op.
      loop {
        match self.ops.get(op_ix) {
          Some(NetOp::Retain(n)) => {
            if position < old_pos + n {
              break;
            }
            old_pos += n;
            new_pos += n;
            op_ix += 1;
          },
          Some(NetOp::Insert { len, .. }) => {
            new_pos += len;
            op_ix += 1;
          },
          Some(NetOp::Delete(n)) => {
            if position < old_pos + n {
              break;
            }
            old_pos += n;
            op_ix += 1;
          },
          None => break,
        }
      }
      match self.ops.get(op_ix) {
        Some(NetOp::Retain(_)) | None => shifted.push(new_pos + (position - old_pos)),
        Some(NetOp::Delete(_)) => shifted.push(new_pos),
        Some(NetOp::Insert { .. }) => unreachable!("insert ops are always consumed in the advance loop"),
      }
    }
    shifted
  }
}

/// Compose a sequence of per-batch op lists (each old→new for its own batch)
/// into one net delta. Missing tails are implicit retains.
pub(crate) fn compose_batches(batches: &[Vec<NetOp>]) -> NetBodyDelta {
  let mut net = NetBodyDelta::default();
  for batch in batches {
    let (ops, churn) = compose(&net.ops, batch);
    net.ops = ops;
    net.structural_churn |= churn;
  }
  normalize(&mut net.ops);
  net
}

/// Standard delta composition: `a` maps old→mid, `b` maps mid→new; the result
/// maps old→new. Returns `(ops, structural_churn)` where churn = `b` deleted
/// (part of) a structural insert from `a`.
fn compose(a: &[NetOp], b: &[NetOp]) -> (Vec<NetOp>, bool) {
  let mut out: Vec<NetOp> = Vec::with_capacity(a.len() + b.len());
  let mut churn = false;
  let mut a_iter = a.iter().copied();
  let mut b_iter = b.iter().copied();
  let mut a_head: Option<NetOp> = a_iter.next();
  let mut b_head: Option<NetOp> = b_iter.next();

  loop {
    match (a_head, b_head) {
      (_, Some(NetOp::Insert { len, structure })) => {
        // b inserts exist only in the new space — emit directly.
        out.push(NetOp::Insert { len, structure });
        b_head = b_iter.next();
      },
      (Some(NetOp::Delete(n)), _) => {
        // a's deletes consumed old text b never saw — emit directly.
        out.push(NetOp::Delete(n));
        a_head = a_iter.next();
      },
      (None, None) => break,
      (None, Some(op)) => {
        // Implicit retain tail on a: b's op applies to untouched old text.
        out.push(op);
        b_head = b_iter.next();
      },
      (Some(op), None) => {
        // Implicit retain tail on b: a's op passes through.
        out.push(op);
        a_head = a_iter.next();
      },
      (Some(a_op), Some(b_op)) => {
        let a_len = match a_op {
          NetOp::Retain(n) => n,
          NetOp::Insert { len, .. } => len,
          NetOp::Delete(_) => unreachable!("deletes handled above"),
        };
        let b_len = match b_op {
          NetOp::Retain(n) => n,
          NetOp::Delete(n) => n,
          NetOp::Insert { .. } => unreachable!("inserts handled above"),
        };
        let step = a_len.min(b_len);
        match (a_op, b_op) {
          (NetOp::Retain(_), NetOp::Retain(_)) => out.push(NetOp::Retain(step)),
          (NetOp::Retain(_), NetOp::Delete(_)) => out.push(NetOp::Delete(step)),
          (NetOp::Insert { structure, .. }, NetOp::Retain(_)) => out.push(NetOp::Insert { len: step, structure }),
          (NetOp::Insert { structure, .. }, NetOp::Delete(_)) => {
            // b deleted text a inserted within this import: it cancels out of
            // the net delta entirely. If that text carried structure we can no
            // longer prove neutrality (the flag is per-op, not per-char).
            churn |= structure;
          },
          _ => unreachable!(),
        }
        a_head = shrink(a_op, step).or_else(|| a_iter.next());
        b_head = shrink(b_op, step).or_else(|| b_iter.next());
      },
    }
  }
  (out, churn)
}

fn shrink(op: NetOp, by: usize) -> Option<NetOp> {
  let remaining = match op {
    NetOp::Retain(n) | NetOp::Delete(n) => n - by,
    NetOp::Insert { len, .. } => len - by,
  };
  if remaining == 0 {
    return None;
  }
  Some(match op {
    NetOp::Retain(_) => NetOp::Retain(remaining),
    NetOp::Delete(_) => NetOp::Delete(remaining),
    NetOp::Insert { structure, .. } => NetOp::Insert { len: remaining, structure },
  })
}

/// Merge adjacent same-kind ops and drop zero-length ones.
fn normalize(ops: &mut Vec<NetOp>) {
  let mut merged: Vec<NetOp> = Vec::with_capacity(ops.len());
  for op in ops.drain(..) {
    let zero = matches!(op, NetOp::Retain(0) | NetOp::Delete(0) | NetOp::Insert { len: 0, .. });
    if zero {
      continue;
    }
    match (merged.last_mut(), op) {
      (Some(NetOp::Retain(a)), NetOp::Retain(b)) => *a += b,
      (Some(NetOp::Delete(a)), NetOp::Delete(b)) => *a += b,
      (Some(NetOp::Insert { len: a, structure: sa }), NetOp::Insert { len: b, structure: sb }) => {
        *a += b;
        *sa |= sb;
      },
      (_, op) => merged.push(op),
    }
  }
  *ops = merged;
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ins(len: usize) -> NetOp {
    NetOp::Insert { len, structure: false }
  }

  fn ins_s(len: usize) -> NetOp {
    NetOp::Insert { len, structure: true }
  }

  #[test]
  fn composes_insert_then_delete_of_other_text() {
    // batch1: insert 3 at 5. batch2: delete 2 at 0 (old text).
    let net = compose_batches(&[vec![NetOp::Retain(5), ins(3)], vec![NetOp::Delete(2)]]);
    assert_eq!(net.ops, vec![NetOp::Delete(2), NetOp::Retain(3), ins(3)]);
    assert_eq!(net.deleted_pre_ranges(), vec![(0, 2)]);
    assert!(!net.inserts_structure());
    assert!(!net.structural_churn);
  }

  #[test]
  fn delete_of_own_structural_insert_flags_churn() {
    // batch1 inserts a paragraph boundary; batch2 deletes it again.
    let net = compose_batches(&[vec![NetOp::Retain(4), ins_s(1)], vec![NetOp::Retain(4), NetOp::Delete(1)]]);
    assert!(net.structural_churn, "cancelled structural insert must force conservatism");
    assert!(net.is_empty(), "net content change cancels out");
  }

  #[test]
  fn detects_deleted_boundary_positions() {
    // Boundaries at 0, 6, 12. One delete of [5, 8).
    let net = compose_batches(&[vec![NetOp::Retain(5), NetOp::Delete(3)]]);
    assert!(net.deletes_any_position(&[0, 6, 12]));
    assert!(!net.deletes_any_position(&[0, 12]));
  }

  #[test]
  fn shifts_positions_through_inserts_and_deletes() {
    // delete [2,4), insert 5 at old-10.
    let net = compose_batches(&[vec![NetOp::Retain(2), NetOp::Delete(2), NetOp::Retain(6), ins(5)]]);
    // old positions:      0  1  5  9  10  20
    // post positions:     0  1  3  7  8+5=13? position 10 is AT the insert point:
    // advance consumes retain(6) ending at old 10, then hits Insert → new_pos += 5,
    // then implicit tail retain maps 10 → 2+6+5 = 13. old 20 → 23.
    assert_eq!(net.shift_positions(&[0, 1, 5, 9, 10, 20]), vec![0, 1, 3, 7, 13, 23]);
  }

  #[test]
  fn multi_batch_positions_compose_exactly() {
    // batch1: insert 4 at 0. batch2 positions are mid-space: delete [2,6) spans
    // half the fresh insert and 2 old chars.
    let net = compose_batches(&[vec![ins(4)], vec![NetOp::Retain(2), NetOp::Delete(4)]]);
    assert_eq!(net.ops, vec![ins(2), NetOp::Delete(2)]);
    assert_eq!(net.deleted_pre_ranges(), vec![(0, 2)]);
  }
}
