//! Always-on algorithmic-work counters for complexity-regression tests.
//!
//! These count the two operations whose super-linear growth caused the large-document
//! actor hangs: full projection rebuilds (a repair storm fires many per edit) and
//! per-cursor `get_cursor_pos` resolutions (the O(records) scan the batched resolver
//! replaced). Tests [`snapshot`] before/after an operation and assert the delta is BOUNDED
//! independent of document size — a signal that does not depend on wall-clock time or on
//! fidelity tracing being on, so it stays valid in CI and catches a regression the moment
//! it lands.
//!
//! The counters are THREAD-LOCAL: a test measures work done synchronously on its own
//! thread, so parallel tests do not contaminate each other's deltas. (Work a runtime does
//! on a background thread — e.g. behind the CRDT actor — is not visible to a test measuring
//! on the test thread; harnesses that need to measure that drive the runtime directly.)

use std::cell::Cell;

thread_local! {
  static FULL_PROJECTIONS: Cell<u64> = const { Cell::new(0) };
  static CURSOR_POS_RESOLVES: Cell<u64> = const { Cell::new(0) };
  static BODY_TO_DELTA_BUILDS: Cell<u64> = const { Cell::new(0) };
}

/// One full `document_from_loro` rebuild of the whole document.
pub fn record_full_projection() {
  FULL_PROJECTIONS.with(|count| count.set(count.get().wrapping_add(1)));
}

/// One per-cursor `get_cursor_pos` resolution (history-traced; the op the batched
/// `query_text_id_positions` resolver exists to avoid calling O(records) times).
pub fn record_cursor_pos_resolve() {
  CURSOR_POS_RESOLVES.with(|count| count.set(count.get().wrapping_add(1)));
}

/// One WHOLE-BODY `text.to_delta()` materialization (§perf-heaven T3): a full
/// projection allocates the entire body as a `Vec<TextDelta>` of run chunks.
/// This is a COLD-path event (reopen / receiving-peer full rebuild). A
/// per-keystroke or single-op edit must go through the REGIONAL rematerializer
/// (§6-R) and trigger ZERO of these — a complexity test asserts the delta stays
/// bounded independent of document size, so a regression back to whole-body
/// rebuild-per-edit trips loudly.
pub fn record_body_to_delta() {
  record_body_to_delta_tagged("untagged");
}

/// §oom-leads #4: per-call-site whole-body materialization census, printed per
/// call when `FLOWSTATE_BODY_DELTA_PROBE` is set (env-gated, zero cost off).
pub fn record_body_to_delta_tagged(site: &'static str) {
  static PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  if *PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_BODY_DELTA_PROBE").is_some()) {
    eprintln!("[flowstate-body-delta] site={site}");
  }
  record_body_to_delta_inner();
}

fn record_body_to_delta_inner() {
  BODY_TO_DELTA_BUILDS.with(|count| count.set(count.get().wrapping_add(1)));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkCounts {
  pub full_projections: u64,
  pub cursor_pos_resolves: u64,
  pub body_to_delta_builds: u64,
}

pub fn snapshot() -> WorkCounts {
  WorkCounts {
    full_projections: FULL_PROJECTIONS.with(Cell::get),
    cursor_pos_resolves: CURSOR_POS_RESOLVES.with(Cell::get),
    body_to_delta_builds: BODY_TO_DELTA_BUILDS.with(Cell::get),
  }
}

impl WorkCounts {
  /// Work done between `earlier` and `self`.
  #[must_use]
  pub fn since(self, earlier: WorkCounts) -> WorkCounts {
    WorkCounts {
      full_projections: self
        .full_projections
        .saturating_sub(earlier.full_projections),
      cursor_pos_resolves: self
        .cursor_pos_resolves
        .saturating_sub(earlier.cursor_pos_resolves),
      body_to_delta_builds: self
        .body_to_delta_builds
        .saturating_sub(earlier.body_to_delta_builds),
    }
  }
}
