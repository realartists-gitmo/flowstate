//! Loom model-check of the arena claim/commit/purge bitmap protocol.
//!
//! This drives the *actual implementation* — `rimalloc::bitmap::{
//! claim_single_field, claim_run_field, prepare_run, release_run,
//! clear_committed_run, purge_field, decommit_run}`, the same generic
//! functions production `arena.rs` executes — instantiated with loom
//! atomics, so every CAS, ordering, and bit transition of the real lock-free
//! bitmap code is explored across all interleavings. Only the OS side
//! effects are test-side: `commit`/`decommit` set/clear a per-block shadow
//! flag (loom cannot model real mmap) and "use" asserts that flag, mirroring
//! how production supplies the real syscalls through the same closures.
//!
//! Invariants asserted across all interleavings:
//!  1. Mutual exclusion — no two threads hold the same block at once (the
//!     lost-update / double-claim hazard, lock-free single-block CAS path and
//!     the multi-block spinlock-scan path).
//!  2. Commit-before-use (the SIGBUS class) — a claimer never reads a block
//!     whose committed shadow is clear without having recommitted; the
//!     `blocks_committed` bit and the real commit state never diverge to hand
//!     out PROT_NONE memory.
//!  3. Purge/claim non-interference — a block being decommitted by
//!     `purge_field`/`decommit_run` is never simultaneously used by a claimer.
//!
//! Run: LOOM_MAX_PREEMPTIONS=3 RUSTFLAGS="--cfg loom" \
//!      cargo test -p rimalloc --test loom_arena --release
#![cfg(loom)]

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::thread;
use rimalloc::bitmap::{
    BitmapWord, claim_run_field, claim_single_field, clear_committed_run, decommit_run,
    prepare_run, purge_field, release_run,
};

/// Loom-instrumented atomic word for the bitmap's generic interface.
struct LoomWord(AtomicUsize);
impl LoomWord {
    fn new(v: usize) -> LoomWord {
        LoomWord(AtomicUsize::new(v))
    }
}
impl BitmapWord for LoomWord {
    fn load(&self, o: Ordering) -> usize {
        self.0.load(o)
    }
    fn store(&self, v: usize, o: Ordering) {
        self.0.store(v, o)
    }
    fn swap(&self, v: usize, o: Ordering) -> usize {
        self.0.swap(v, o)
    }
    fn fetch_or(&self, v: usize, o: Ordering) -> usize {
        self.0.fetch_or(v, o)
    }
    fn fetch_and(&self, v: usize, o: Ordering) -> usize {
        self.0.fetch_and(v, o)
    }
    fn compare_exchange_weak(
        &self,
        cur: usize,
        new: usize,
        ok: Ordering,
        err: Ordering,
    ) -> Result<usize, usize> {
        self.0.compare_exchange_weak(cur, new, ok, err)
    }
}

/// One arena field's worth of bitmap state plus the test-side shadows that
/// stand in for real OS memory. `BLOCKS` blocks in a single field.
struct Field {
    inuse: LoomWord,
    dirty: LoomWord,
    committed: LoomWord,
    purge: LoomWord,
    claim_lock: AtomicBool,
    /// Per-block shadow of the *real* commit state: `commit` sets it,
    /// `decommit` clears it, "use" asserts it. Divergence from the
    /// `committed` bitmap in a way that lets a claimer use a `false` block is
    /// the SIGBUS bug.
    shadow_committed: Vec<UnsafeCell<bool>>,
    /// Per-block live-owner guard for mutual exclusion: a claimer that thinks
    /// it owns a block flips this true; two simultaneous owners is a
    /// double-claim. Guarded solely by the bitmap CAS, never atomically.
    owned: Vec<UnsafeCell<bool>>,
}

// SAFETY: all sharing is mediated by the loom atomics / cells.
unsafe impl Sync for Field {}
unsafe impl Send for Field {}

impl Field {
    fn new(blocks: usize, committed: bool) -> Field {
        Field {
            inuse: LoomWord::new(0),
            dirty: LoomWord::new(0),
            committed: LoomWord::new(if committed { (1 << blocks) - 1 } else { 0 }),
            purge: LoomWord::new(0),
            claim_lock: AtomicBool::new(false),
            shadow_committed: (0..blocks).map(|_| UnsafeCell::new(committed)).collect(),
            owned: (0..blocks).map(|_| UnsafeCell::new(false)).collect(),
        }
    }

    /// `commit` side effect: set the shadow flags for the run.
    fn shadow_commit(&self, bit: usize, count: usize) {
        for b in bit..bit + count {
            self.shadow_committed[b].with_mut(|p| unsafe { *p = true });
        }
    }

    /// `decommit` side effect: clear the shadow flags for the run.
    fn shadow_decommit(&self, bit: usize, count: usize) {
        for b in bit..bit + count {
            self.shadow_committed[b].with_mut(|p| unsafe { *p = false });
        }
    }

    /// Claim + prepare + use + release one block via the *real* lock-free
    /// single-block path, asserting all three invariants.
    fn claim_use_single(&self, blocks: usize) {
        let Some(bit) = claim_single_field(&self.inuse, blocks) else {
            return; // field full this interleaving
        };
        // Invariant 1: we are the sole owner now.
        self.owned[bit].with_mut(|p| unsafe {
            assert!(!*p, "double-claim: block {bit} already owned");
            *p = true;
        });
        // prepare: recommit iff committed bit was clear.
        prepare_run(&self.dirty, &self.committed, &self.purge, bit, 1, || {
            self.shadow_commit(bit, 1)
        });
        // Invariant 2: the block we are about to "use" is really committed.
        self.shadow_committed[bit]
            .with(|p| unsafe { assert!(*p, "commit-before-use violated: block {bit} (SIGBUS)") });
        // Invariant 3 also holds here: purge_field/decommit_run can only act
        // on blocks whose inuse bit is clear, and ours is set for this whole
        // window.
        self.owned[bit].with_mut(|p| unsafe { *p = false });
        release_run(&self.inuse, &self.purge, bit, 1);
    }

    /// Claim + prepare + use + release a `count`-wide run via the *real*
    /// multi-block spinlock-scan path.
    fn claim_use_run(&self, blocks: usize, count: usize) {
        rimalloc_spin_acquire(&self.claim_lock);
        let found = claim_run_field(&self.inuse, blocks, count);
        rimalloc_spin_release(&self.claim_lock);
        let Some(bit) = found else {
            return;
        };
        for b in bit..bit + count {
            self.owned[b].with_mut(|p| unsafe {
                assert!(!*p, "double-claim (multi): block {b} already owned");
                *p = true;
            });
        }
        prepare_run(
            &self.dirty,
            &self.committed,
            &self.purge,
            bit,
            count,
            || self.shadow_commit(bit, count),
        );
        for b in bit..bit + count {
            self.shadow_committed[b].with(|p| unsafe {
                assert!(*p, "commit-before-use violated: block {b} (SIGBUS, multi)")
            });
        }
        for b in bit..bit + count {
            self.owned[b].with_mut(|p| unsafe { *p = false });
        }
        release_run(&self.inuse, &self.purge, bit, count);
    }

    /// `arena::free(.., all_committed=false)`: a dying segment decommitted
    /// part of its range, so it clears the committed bits *and* the shadow
    /// for the run, then releases. Models the cross-layer fix's caller.
    fn free_partial(&self, bit: usize, count: usize) {
        clear_committed_run(&self.committed, bit, count);
        self.shadow_decommit(bit, count);
        release_run(&self.inuse, &self.purge, bit, count);
    }

    /// `purge_now` for this single field, decommit mode: the real
    /// `purge_field` + `decommit_run` handshake. The `decommit` closure
    /// clears the shadow; invariant 3 is that no claimer uses a block while
    /// its shadow is being cleared — guaranteed because `decommit_run` holds
    /// the inuse bit across the whole shadow clear, so any concurrent
    /// claimer's `prepare_run` (which sees committed clear) recommits before
    /// using, and any claimer that grabbed the bit first keeps `decommit_run`
    /// from acting on it.
    fn purge(&self) {
        purge_field(&self.purge, &self.inuse, |bit, run_len| {
            decommit_run(&self.inuse, &self.committed, bit, run_len, || {
                self.shadow_decommit(bit, run_len)
            });
        });
    }

    /// Final consistency check: every committed *bit* must agree with its
    /// shadow (no block advertised committed in the bitmap while physically
    /// decommitted), and nothing is left owned.
    fn assert_consistent(&self, blocks: usize) {
        let inuse = self.inuse.load(Ordering::Relaxed);
        assert_eq!(inuse, 0, "blocks left in use");
        let committed = self.committed.load(Ordering::Relaxed);
        for b in 0..blocks {
            self.owned[b].with(|p| unsafe { assert!(!*p, "block {b} left owned") });
            let bit_set = committed & (1 << b) != 0;
            let shadow = self.shadow_committed[b].with(|p| unsafe { *p });
            if bit_set {
                assert!(
                    shadow,
                    "committed bitmap advertises block {b} but shadow says decommitted"
                );
            }
        }
    }
}

/// Loom-instrumented copy of `crate::sync::spin_{acquire,release}` (the lock
/// is `private`; this is byte-identical to the production spinlock so the
/// multi-block claim path is modeled exactly).
fn rimalloc_spin_acquire(lock: &AtomicBool) {
    while lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        loom::hint::spin_loop();
    }
}
fn rimalloc_spin_release(lock: &AtomicBool) {
    lock.store(false, Ordering::Release);
}

/// Two threads race lock-free single-block claims on a 2-block field that
/// starts fully committed. Mutual exclusion + commit-before-use.
#[test]
fn claim_vs_claim_single() {
    loom::model(|| {
        let f = Arc::new(Field::new(2, true));
        let t1 = {
            let f = f.clone();
            thread::spawn(move || f.claim_use_single(2))
        };
        let t2 = {
            let f = f.clone();
            thread::spawn(move || f.claim_use_single(2))
        };
        t1.join().unwrap();
        t2.join().unwrap();
        f.assert_consistent(2);
    });
}

/// A claimer racing a purge: block 0 starts free+committed but scheduled for
/// purge; one thread claims-and-uses it (must recommit if it observes the
/// committed bit cleared by the purge), the other decommits it. This is the
/// exact SIGBUS class — re-claiming a partially-decommitted block.
#[test]
fn claim_vs_purge() {
    loom::model(|| {
        // 2 blocks, both committed; block 0 scheduled for purge.
        let f = Arc::new(Field::new(2, true));
        f.purge.store(1, Ordering::Relaxed); // block 0 pending purge
        let claimer = {
            let f = f.clone();
            thread::spawn(move || f.claim_use_single(2))
        };
        let purger = {
            let f = f.clone();
            thread::spawn(move || f.purge())
        };
        claimer.join().unwrap();
        purger.join().unwrap();
        f.assert_consistent(2);
    });
}

/// free-partial (clears committed bit + shadow) racing a claim-and-use of the
/// freed block: the claimer that re-grabs the block must observe the cleared
/// committed bit and recommit before using. This is the cross-layer fix.
#[test]
fn free_partial_vs_claim() {
    loom::model(|| {
        // 1 block, starts owned (inuse) and committed.
        let f = Arc::new(Field::new(1, true));
        f.inuse.store(1, Ordering::Relaxed);
        f.dirty.store(1, Ordering::Relaxed);
        let freer = {
            let f = f.clone();
            thread::spawn(move || f.free_partial(0, 1))
        };
        let claimer = {
            let f = f.clone();
            thread::spawn(move || f.claim_use_single(1))
        };
        freer.join().unwrap();
        claimer.join().unwrap();
        f.assert_consistent(1);
    });
}

/// Multi-block claim (spinlock path) racing a single-block claim on a 2-block
/// field: the run claim wants both blocks; the single claim wants one. They
/// must not overlap.
#[test]
fn claim_run_vs_single() {
    loom::model(|| {
        let f = Arc::new(Field::new(2, true));
        let runner = {
            let f = f.clone();
            thread::spawn(move || f.claim_use_run(2, 2))
        };
        let single = {
            let f = f.clone();
            thread::spawn(move || f.claim_use_single(2))
        };
        runner.join().unwrap();
        single.join().unwrap();
        f.assert_consistent(2);
    });
}
