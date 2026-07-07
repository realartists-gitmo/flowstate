//! Always-on algorithmic-work counters for complexity-regression tests.
//!
//! These are relaxed atomics (negligible cost) that count the two operations whose
//! super-linear growth caused the large-document actor hangs: full projection rebuilds
//! (a repair storm fires many per edit) and per-cursor `get_cursor_pos` resolutions (the
//! O(records) scan the batched resolver replaced). Tests [`snapshot`] before/after an
//! operation and assert the delta is BOUNDED independent of document size — a signal that
//! does not depend on wall-clock time or on fidelity tracing being on, so it stays valid
//! in CI and catches a regression the moment it lands.

use std::sync::atomic::{AtomicU64, Ordering};

static FULL_PROJECTIONS: AtomicU64 = AtomicU64::new(0);
static CURSOR_POS_RESOLVES: AtomicU64 = AtomicU64::new(0);

/// One full `document_from_loro` rebuild of the whole document.
pub fn record_full_projection() {
  FULL_PROJECTIONS.fetch_add(1, Ordering::Relaxed);
}

/// One per-cursor `get_cursor_pos` resolution (history-traced; the op the batched
/// `query_text_id_positions` resolver exists to avoid calling O(records) times).
pub fn record_cursor_pos_resolve() {
  CURSOR_POS_RESOLVES.fetch_add(1, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkCounts {
  pub full_projections: u64,
  pub cursor_pos_resolves: u64,
}

pub fn snapshot() -> WorkCounts {
  WorkCounts {
    full_projections: FULL_PROJECTIONS.load(Ordering::Relaxed),
    cursor_pos_resolves: CURSOR_POS_RESOLVES.load(Ordering::Relaxed),
  }
}

impl WorkCounts {
  /// Work done between `earlier` and `self`.
  #[must_use]
  pub fn since(self, earlier: WorkCounts) -> WorkCounts {
    WorkCounts {
      full_projections: self.full_projections.saturating_sub(earlier.full_projections),
      cursor_pos_resolves: self.cursor_pos_resolves.saturating_sub(earlier.cursor_pos_resolves),
    }
  }
}
