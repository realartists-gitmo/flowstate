//! Zero-cost-when-off diagnostics for Flowstate CRDT/document fidelity.
//!
//! This crate is a thin, dependency-light (only `tracing`) instrumentation
//! layer. Every entry point is gated by a single relaxed atomic load, so when
//! the `FLOWSTATE_TRACE_FIDELITY` environment variable is unset the detail
//! closures are never evaluated and instrumentation is effectively free.
//!
//! Instrumentation is strictly additive: nothing here mutates caller state or
//! changes control flow. [`check`] returns its input condition unchanged so it
//! can wrap an existing boolean without altering behavior.

use std::{
  cell::RefCell,
  collections::VecDeque,
  sync::atomic::{AtomicBool, Ordering},
};

/// Global on/off switch. A single relaxed load gates every event, so the cost
/// of disabled instrumentation is one atomic read and nothing else.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Number of recent firehose entries retained per thread for post-mortem dumps.
const RING_CAPACITY: usize = 512;

thread_local! {
  /// Recent firehose entries for the current thread. On a violation the tail of
  /// this buffer is dumped so the events leading up to the failure are visible.
  static RING: RefCell<VecDeque<String>> = RefCell::new(VecDeque::with_capacity(RING_CAPACITY));

  /// Test/diagnostic sink: every fired [`violation`] (only ever when enabled)
  /// appends its rendered line here so an integration or soak test can assert the
  /// invariant set stayed silent for a whole run. Drained by [`take_violations`].
  /// Independent of the firehose [`RING`]; empty and untouched when diagnostics
  /// are off (a disabled `violation` returns before recording anything).
  static VIOLATIONS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Categories of fidelity events. `Display` renders a short lowercase tag used
/// both in ring-buffer lines and as the `class` field on `tracing` events; keep
/// these strings stable, downstream log filters and instrumentation match them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FidelityClass {
  Caret,
  Text,
  Reconcile,
  Projection,
  Identity,
  Structure,
  Frontier,
  Convergence,
  Persistence,
  Presence,
  Undo,
  ImportExport,
  Asset,
}

impl std::fmt::Display for FidelityClass {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let name = match self {
      Self::Caret => "caret",
      Self::Text => "text",
      Self::Reconcile => "reconcile",
      Self::Projection => "projection",
      Self::Identity => "identity",
      Self::Structure => "structure",
      Self::Frontier => "frontier",
      Self::Convergence => "convergence",
      Self::Persistence => "persistence",
      Self::Presence => "presence",
      Self::Undo => "undo",
      Self::ImportExport => "importexport",
      Self::Asset => "asset",
    };
    f.write_str(name)
  }
}

/// Enables diagnostics when `FLOWSTATE_TRACE_FIDELITY` holds any non-empty value
/// other than `0`. Call once at startup after logging is initialized.
pub fn init_from_env() {
  let on = std::env::var("FLOWSTATE_TRACE_FIDELITY").is_ok_and(|value| {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed != "0"
  });
  set_enabled(on);
}

/// Turns diagnostics on or off at runtime.
pub fn set_enabled(on: bool) {
  ENABLED.store(on, Ordering::Relaxed);
}

/// Returns whether diagnostics are currently enabled. A single relaxed atomic
/// load; safe to call on hot paths to guard nontrivial invariant computation.
#[inline]
#[must_use]
pub fn enabled() -> bool {
  ENABLED.load(Ordering::Relaxed)
}

/// Records a gated firehose event. The `detail` closure runs only when enabled,
/// so instrumentation is free when off. The rendered line is pushed to the
/// thread-local ring buffer and emitted at `debug` under target `fidelity`.
pub fn event(class: FidelityClass, kind: &str, detail: impl FnOnce() -> String) {
  if !enabled() {
    return;
  }
  let detail = detail();
  RING.with(|ring| {
    let mut ring = ring.borrow_mut();
    if ring.len() >= RING_CAPACITY {
      ring.pop_front();
    }
    ring.push_back(format!("[{class}] {kind}: {detail}"));
  });
  tracing::debug!(target: "fidelity", %class, kind, detail = %detail);
}

/// Records a gated LOUD event: emits at `error` under target `fidelity.violation`
/// and dumps the recent ring buffer so the lead-up to the failure is visible.
pub fn violation(class: FidelityClass, kind: &str, detail: impl FnOnce() -> String) {
  if !enabled() {
    return;
  }
  let detail = detail();
  // Record for `take_violations` before the (potentially verbose) dump. This is
  // strictly additive diagnostics: it never changes control flow and only runs
  // on the already-cold violation path while enabled.
  VIOLATIONS.with(|sink| sink.borrow_mut().push(format!("[{class}] {kind}: {detail}")));
  tracing::error!(
    target: "fidelity.violation",
    %class,
    kind,
    detail = %detail,
    "FIDELITY INVARIANT VIOLATED"
  );
  dump_recent(64);
}

/// Drains and returns the fidelity violations recorded on the **current thread**
/// since the last drain, each rendered as `[class] kind: detail`.
///
/// Intended for tests: arm diagnostics with [`set_enabled`], drive a workload,
/// then assert this returns empty. Because the sink is thread-local, the workload
/// under test must run its invariant checks on the calling thread (the soak
/// harness drives a synchronous `CrdtRuntime` + editor entirely on the test
/// thread, so every `violation` lands here). Records only while [`enabled`].
#[must_use]
pub fn take_violations() -> Vec<String> {
  VIOLATIONS.with(|sink| std::mem::take(&mut *sink.borrow_mut()))
}

/// Drains and returns the thread-local firehose ring buffer (every event
/// recorded since the last drain, oldest first). Intended for tests that want
/// the full event trail leading up to a failure printed alongside the assertion.
#[must_use]
pub fn drain_ring() -> Vec<String> {
  RING.with(|ring| ring.borrow_mut().drain(..).collect())
}

/// Asserts an invariant without changing behavior: returns `condition`
/// unchanged, and only when enabled and the condition is false does it fire a
/// [`violation`]. The `detail` closure is not evaluated unless it fires.
pub fn check(condition: bool, class: FidelityClass, kind: &str, detail: impl FnOnce() -> String) -> bool {
  if enabled() && !condition {
    violation(class, kind, detail);
  }
  condition
}

/// On-demand marker: stamps "the bug is happening now" into the diagnostics
/// stream and dumps the entire recent ring buffer as a self-contained context
/// blob, so a bug that trips no automatic invariant still hands its lead-up to a
/// parsing agent. Wire this to a UI action/keybinding. No-op when disabled.
pub fn marker(label: &str) {
  if !enabled() {
    return;
  }
  tracing::error!(target: "fidelity.marker", label, "FIDELITY MARKER");
  dump_recent(RING_CAPACITY);
}

/// Emits the last `n` entries of the thread-local ring buffer at `error` under
/// target `fidelity.violation`. No-op contribution when the buffer is empty.
pub fn dump_recent(n: usize) {
  RING.with(|ring| {
    let ring = ring.borrow();
    let skip = ring.len().saturating_sub(n);
    for entry in ring.iter().skip(skip) {
      tracing::error!(target: "fidelity.violation", "{entry}");
    }
  });
}

#[cfg(test)]
mod tests {
  use std::{cell::Cell, sync::Mutex};

  use super::{FidelityClass, RING, RING_CAPACITY, check, enabled, event, set_enabled, take_violations};

  // Tests toggle the global `ENABLED`, so serialize them to keep the flag
  // deterministic under the parallel test harness.
  static TEST_LOCK: Mutex<()> = Mutex::new(());

  fn guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn clear_ring() {
    RING.with(|ring| ring.borrow_mut().clear());
  }

  fn ring_len() -> usize {
    RING.with(|ring| ring.borrow().len())
  }

  fn ring_front() -> Option<String> {
    RING.with(|ring| ring.borrow().front().cloned())
  }

  #[test]
  fn event_records_only_when_enabled() {
    let _lock = guard();
    clear_ring();

    set_enabled(false);
    event(FidelityClass::Caret, "disabled", || "should not appear".to_string());
    assert_eq!(ring_len(), 0, "disabled events must not touch the ring");

    set_enabled(true);
    event(FidelityClass::Caret, "enabled", || "value".to_string());
    assert_eq!(ring_len(), 1, "enabled events must be recorded");
    assert_eq!(ring_front().as_deref(), Some("[caret] enabled: value"));

    set_enabled(false);
    clear_ring();
  }

  #[test]
  fn ring_buffer_evicts_at_capacity() {
    let _lock = guard();
    clear_ring();
    set_enabled(true);

    for index in 0..(RING_CAPACITY + 10) {
      event(FidelityClass::Text, "fill", || index.to_string());
    }

    assert_eq!(ring_len(), RING_CAPACITY, "ring must cap at capacity");
    // The first 10 entries (indices 0..10) must have been evicted; the oldest
    // surviving entry is index 10.
    assert_eq!(ring_front().as_deref(), Some("[text] fill: 10"));

    set_enabled(false);
    clear_ring();
  }

  #[test]
  fn check_returns_condition_and_fires_only_on_false_when_enabled() {
    let _lock = guard();
    set_enabled(true);

    let evaluated = Cell::new(false);
    let ok = check(true, FidelityClass::Reconcile, "ok", || {
      evaluated.set(true);
      "unused".to_string()
    });
    assert!(ok, "check must return its condition unchanged");
    assert!(!evaluated.get(), "detail must not run when condition holds");

    let evaluated = Cell::new(false);
    let bad = check(false, FidelityClass::Reconcile, "bad", || {
      evaluated.set(true);
      "violated".to_string()
    });
    assert!(!bad, "check must return its condition unchanged");
    assert!(evaluated.get(), "detail must run for a violation when enabled");

    set_enabled(false);
    clear_ring();
  }

  #[test]
  fn check_does_not_evaluate_detail_when_disabled() {
    let _lock = guard();
    set_enabled(false);

    let evaluated = Cell::new(false);
    let result = check(false, FidelityClass::Convergence, "disabled", || {
      evaluated.set(true);
      "should not run".to_string()
    });
    assert!(!result, "check must still return its condition when disabled");
    assert!(!evaluated.get(), "detail closure must not run while disabled");
  }

  #[test]
  fn event_detail_not_evaluated_when_disabled() {
    let _lock = guard();
    set_enabled(false);

    let evaluated = Cell::new(false);
    event(FidelityClass::Persistence, "disabled", || {
      evaluated.set(true);
      "should not run".to_string()
    });
    assert!(!evaluated.get(), "event detail must not run while disabled");
    assert!(!enabled());
  }

  #[test]
  fn take_violations_collects_fired_violations_and_drains() {
    let _lock = guard();
    set_enabled(true);
    // Clear any residue from a prior test that shared this thread.
    let _ = take_violations();

    // A passing check records nothing.
    check(true, FidelityClass::Caret, "ok", || "unused".to_string());
    assert!(take_violations().is_empty(), "a satisfied invariant must not record a violation");

    // A failing check records exactly one rendered line.
    check(false, FidelityClass::Caret, "reconcile-regressed", || "before=1 after=0".to_string());
    let fired = take_violations();
    assert_eq!(fired.len(), 1, "one failed invariant must record one violation");
    assert_eq!(fired[0], "[caret] reconcile-regressed: before=1 after=0");

    // Draining is destructive: a second drain is empty.
    assert!(take_violations().is_empty(), "take_violations must drain the sink");

    set_enabled(false);
  }

  #[test]
  fn take_violations_records_nothing_while_disabled() {
    let _lock = guard();
    set_enabled(false);
    let _ = take_violations();
    check(false, FidelityClass::Caret, "reconcile-regressed", || "must not record".to_string());
    assert!(take_violations().is_empty(), "disabled violations must never be recorded");
  }

  #[test]
  fn display_tags_are_stable_lowercase() {
    assert_eq!(FidelityClass::Caret.to_string(), "caret");
    assert_eq!(FidelityClass::Text.to_string(), "text");
    assert_eq!(FidelityClass::Reconcile.to_string(), "reconcile");
    assert_eq!(FidelityClass::Projection.to_string(), "projection");
    assert_eq!(FidelityClass::Identity.to_string(), "identity");
    assert_eq!(FidelityClass::Structure.to_string(), "structure");
    assert_eq!(FidelityClass::Frontier.to_string(), "frontier");
    assert_eq!(FidelityClass::Convergence.to_string(), "convergence");
    assert_eq!(FidelityClass::Persistence.to_string(), "persistence");
    assert_eq!(FidelityClass::Presence.to_string(), "presence");
    assert_eq!(FidelityClass::Undo.to_string(), "undo");
    assert_eq!(FidelityClass::ImportExport.to_string(), "importexport");
    assert_eq!(FidelityClass::Asset.to_string(), "asset");
  }
}
