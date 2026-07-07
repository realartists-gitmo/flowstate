//! The exclusive write gate (spec I-9/I-9a/I-9b).
//!
//! Every access to the shared `LoroDoc` — mutation, import, export, fork,
//! checkout, cursor resolution — happens inside this gate. Loro 1.13.1 makes
//! virtually every API call a potential commit barrier (export/fork/diff/
//! checkout/`get_cursor_pos`-on-deleted-anchor all `with_barrier`, which
//! commits any pending transaction), so an ungated call from another thread
//! mid-intent would seal and publish a half-applied intent. The gate widens
//! Loro's per-call serialization to whole-intent serialization.
//!
//! The gate is a *measured critical section*: every acquisition records the
//! holder, wait time, hold time, and whether another thread was blocked on it
//! (`contended`). The import-chunk hold-time budget in the perf suite is
//! enforced against these records — one import chunk's gate hold is the
//! maximum possible typing stall.
//!
//! Panic policy (spec I-10d): a panic while holding the gate poisons it, and a
//! poisoned gate means the underlying Loro doc may hold a half-applied intent
//! whose Loro mutexes are themselves poisoned. There is no in-place recovery;
//! the owner must reload from persisted state. `WriteGate::lock` therefore
//! propagates poisoning as a hard error instead of hiding it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

/// Who is holding (or asking for) the gate. Used for structured hold records
/// and the per-holder hold-time budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateHolder {
  /// A local editing intent on the UI thread (resolve → mutate → commit → patch).
  LocalIntent,
  /// Undo/redo executed through the Loro `UndoManager`.
  UndoRedo,
  /// One remote import chunk (a single update blob or one coalesced
  /// `import_batch` slice) plus its post-import patch derivation.
  ImportChunk,
  /// An incremental update export (`export(updates(vv))`) for publish or
  /// anti-entropy.
  ExportUpdates,
  /// The brief hold needed to `fork()` the doc so a snapshot/package export can
  /// run off-gate against the fork.
  ExportFork,
  /// Presence caret/selection resolution (cursor decode + position lookup).
  /// Gate-held because `get_cursor_pos` on a deleted anchor is a commit
  /// barrier (spec I-9a).
  Presence,
  /// Revision open/fork, package checkpoint, and other document services.
  DocumentService,
  /// Test-only holder.
  #[cfg(test)]
  Test,
}

impl GateHolder {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::LocalIntent => "local-intent",
      Self::UndoRedo => "undo-redo",
      Self::ImportChunk => "import-chunk",
      Self::ExportUpdates => "export-updates",
      Self::ExportFork => "export-fork",
      Self::Presence => "presence",
      Self::DocumentService => "document-service",
      #[cfg(test)]
      Self::Test => "test",
    }
  }
}

/// One completed gate hold, emitted to tracing and folded into [`GateMetrics`].
#[derive(Clone, Copy, Debug)]
pub struct GateHoldRecord {
  pub holder: GateHolder,
  pub wait_micros: u64,
  pub hold_micros: u64,
  /// True when at least one other thread started waiting on the gate while this
  /// hold was in progress (i.e. this hold *caused* blocking), or when this
  /// acquisition itself had to wait. Either direction is a contention signal.
  pub contended: bool,
}

/// Aggregate gate counters (always-on; cheap atomics). The perf suite asserts
/// budget ceilings against the per-holder maxima.
#[derive(Debug, Default)]
pub struct GateMetrics {
  pub acquisitions: AtomicU64,
  pub contended_acquisitions: AtomicU64,
  pub total_wait_micros: AtomicU64,
  pub total_hold_micros: AtomicU64,
  pub max_hold_micros_local_intent: AtomicU64,
  pub max_hold_micros_import_chunk: AtomicU64,
  pub max_hold_micros_other: AtomicU64,
  pub max_wait_micros_local_intent: AtomicU64,
}

impl GateMetrics {
  fn record(&self, record: GateHoldRecord) {
    self.acquisitions.fetch_add(1, Ordering::Relaxed);
    if record.contended {
      self.contended_acquisitions.fetch_add(1, Ordering::Relaxed);
    }
    self.total_wait_micros.fetch_add(record.wait_micros, Ordering::Relaxed);
    self.total_hold_micros.fetch_add(record.hold_micros, Ordering::Relaxed);
    let max_hold = match record.holder {
      GateHolder::LocalIntent | GateHolder::UndoRedo => &self.max_hold_micros_local_intent,
      GateHolder::ImportChunk => &self.max_hold_micros_import_chunk,
      _ => &self.max_hold_micros_other,
    };
    max_hold.fetch_max(record.hold_micros, Ordering::Relaxed);
    if matches!(record.holder, GateHolder::LocalIntent | GateHolder::UndoRedo) {
      self.max_wait_micros_local_intent.fetch_max(record.wait_micros, Ordering::Relaxed);
    }
  }
}

/// The write gate: a poisoning mutex around the shared document core, with
/// measured acquisitions.
pub struct WriteGate<T> {
  inner: Mutex<T>,
  metrics: Arc<GateMetrics>,
  /// Number of threads currently blocked in `lock()`. Lets a holder learn, at
  /// release time, that somebody was waiting on it (the `contended` flag on its
  /// hold record).
  waiters: AtomicU64,
  /// Set while any hold is active with waiters observed; cleared on release.
  contention_seen: AtomicBool,
}

/// A held gate. Releasing (dropping) emits the hold record.
pub struct GateGuard<'a, T> {
  guard: Option<MutexGuard<'a, T>>,
  gate: &'a WriteGate<T>,
  holder: GateHolder,
  wait_micros: u64,
  acquired_at: Instant,
  waited: bool,
}

/// The gate was poisoned by a panic while held: the doc may contain a
/// half-applied intent and Loro's own mutexes are unusable. Spec I-10d: reload
/// from persisted state; do not attempt in-place recovery.
#[derive(Clone, Copy, Debug)]
pub struct GatePoisonedError;

impl std::fmt::Display for GatePoisonedError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("write gate poisoned by a panic while held; document core must be reloaded from persisted state (spec I-10d)")
  }
}

impl std::error::Error for GatePoisonedError {}

impl<T> WriteGate<T> {
  #[must_use]
  pub fn new(inner: T) -> Self {
    Self {
      inner: Mutex::new(inner),
      metrics: Arc::new(GateMetrics::default()),
      waiters: AtomicU64::new(0),
      contention_seen: AtomicBool::new(false),
    }
  }

  #[must_use]
  pub fn metrics(&self) -> Arc<GateMetrics> {
    Arc::clone(&self.metrics)
  }

  /// Acquire the gate for `holder`. Blocks until exclusive; the returned guard
  /// measures the hold and emits a structured record on release.
  pub fn lock(&self, holder: GateHolder) -> Result<GateGuard<'_, T>, GatePoisonedError> {
    let requested_at = Instant::now();
    let (guard, waited) = match self.inner.try_lock() {
      Ok(guard) => (guard, false),
      Err(std::sync::TryLockError::Poisoned(_)) => return Err(GatePoisonedError),
      Err(std::sync::TryLockError::WouldBlock) => {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        self.contention_seen.store(true, Ordering::SeqCst);
        let result = self.inner.lock();
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        match result {
          Ok(guard) => (guard, true),
          Err(_) => return Err(GatePoisonedError),
        }
      },
    };
    let wait_micros = u64::try_from(requested_at.elapsed().as_micros()).unwrap_or(u64::MAX);
    // A fresh hold starts with no observed waiters; anyone who blocks from here
    // on flips `contention_seen`, which this hold reads at release.
    self.contention_seen.store(self.waiters.load(Ordering::SeqCst) > 0, Ordering::SeqCst);
    Ok(GateGuard {
      guard: Some(guard),
      gate: self,
      holder,
      wait_micros,
      acquired_at: Instant::now(),
      waited,
    })
  }
}

impl<T> std::ops::Deref for GateGuard<'_, T> {
  type Target = T;

  fn deref(&self) -> &T {
    self.guard.as_ref().expect("gate guard accessed after release")
  }
}

impl<T> std::ops::DerefMut for GateGuard<'_, T> {
  fn deref_mut(&mut self) -> &mut T {
    self.guard.as_mut().expect("gate guard accessed after release")
  }
}

impl<T> Drop for GateGuard<'_, T> {
  fn drop(&mut self) {
    let hold_micros = u64::try_from(self.acquired_at.elapsed().as_micros()).unwrap_or(u64::MAX);
    let others_waited = self.gate.contention_seen.swap(false, Ordering::SeqCst) || self.gate.waiters.load(Ordering::SeqCst) > 0;
    let record = GateHoldRecord {
      holder: self.holder,
      wait_micros: self.wait_micros,
      hold_micros,
      contended: self.waited || others_waited,
    };
    // Release the doc before doing any bookkeeping I/O.
    drop(self.guard.take());
    self.gate.metrics.record(record);
    tracing::trace!(
      holder = record.holder.as_str(),
      wait_us = record.wait_micros,
      hold_us = record.hold_micros,
      contended = record.contended,
      "write-gate hold",
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::Ordering;

  #[test]
  fn gate_measures_holds_and_contention() {
    let gate = Arc::new(WriteGate::new(0_u64));
    let guard_scope = gate.lock(GateHolder::Test).expect("gate healthy");
    drop(guard_scope);
    let metrics = gate.metrics();
    assert_eq!(metrics.acquisitions.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.contended_acquisitions.load(Ordering::Relaxed), 0);

    // Contention: hold the gate on one thread while another waits.
    let gate_b = Arc::clone(&gate);
    let held = gate.lock(GateHolder::Test).expect("gate healthy");
    let waiter = std::thread::spawn(move || {
      let guard = gate_b.lock(GateHolder::Test).expect("gate healthy");
      *guard
    });
    // Give the waiter time to block, then release.
    std::thread::sleep(std::time::Duration::from_millis(20));
    drop(held);
    waiter.join().expect("waiter completes");
    assert!(gate.metrics().contended_acquisitions.load(Ordering::Relaxed) >= 1, "waiting acquisition must be recorded as contended");
  }

  #[test]
  fn gate_reports_poisoning_as_error() {
    let gate = Arc::new(WriteGate::new(0_u64));
    let gate_b = Arc::clone(&gate);
    let _ = std::thread::spawn(move || {
      let _guard = gate_b.lock(GateHolder::Test).expect("gate healthy");
      panic!("poison the gate");
    })
    .join();
    assert!(gate.lock(GateHolder::Test).is_err(), "poisoned gate must surface as an error, not silence");
  }
}
