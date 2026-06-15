//! The arena claim/commit/purge bitmap protocol — the *single*
//! implementation, generic over the atomic word type. Production
//! (`arena.rs`) instantiates it with `core` atomics on the live `Arena`
//! bitmap fields (`blocks_inuse`/`blocks_committed`/`blocks_dirty`/
//! `blocks_purge`); `tests/loom_arena.rs` instantiates the *same functions*
//! with loom atomics, so every CAS, ordering, and bit transition of the
//! real lock-free protocol is model-checked across all interleavings.
//!
//! The OS side effects (`commit`/`decommit`/`reset`) are abstracted as
//! closures, mirroring how `protocol.rs` abstracts block storage: production
//! supplies the real syscalls, loom supplies shadow-flag mutations (loom
//! cannot model real mmap). All bit twiddling, memory orderings, and the
//! claim/purge non-interference handshake live here and only here.

use core::sync::atomic::Ordering;

pub const FIELD_BITS: usize = usize::BITS as usize;

/// Atomic-word interface implemented by `core` and loom `AtomicUsize`.
/// Richer than `protocol::AtomicWord` (adds `fetch_or`/`fetch_and`) because
/// the bitmaps set and clear bit ranges, not just CAS a packed word.
pub trait BitmapWord {
    fn load(&self, order: Ordering) -> usize;
    fn store(&self, val: usize, order: Ordering);
    fn swap(&self, val: usize, order: Ordering) -> usize;
    fn fetch_or(&self, val: usize, order: Ordering) -> usize;
    fn fetch_and(&self, val: usize, order: Ordering) -> usize;
    fn compare_exchange_weak(
        &self,
        current: usize,
        new: usize,
        success: Ordering,
        failure: Ordering,
    ) -> Result<usize, usize>;
}

impl BitmapWord for core::sync::atomic::AtomicUsize {
    fn load(&self, order: Ordering) -> usize {
        self.load(order)
    }
    fn store(&self, val: usize, order: Ordering) {
        self.store(val, order)
    }
    fn swap(&self, val: usize, order: Ordering) -> usize {
        self.swap(val, order)
    }
    fn fetch_or(&self, val: usize, order: Ordering) -> usize {
        self.fetch_or(val, order)
    }
    fn fetch_and(&self, val: usize, order: Ordering) -> usize {
        self.fetch_and(val, order)
    }
    fn compare_exchange_weak(
        &self,
        current: usize,
        new: usize,
        success: Ordering,
        failure: Ordering,
    ) -> Result<usize, usize> {
        self.compare_exchange_weak(current, new, success, failure)
    }
}

/// `1`-bits for `[bit, bit+count)` within one field.
#[inline]
pub fn field_mask(bit: usize, count: usize) -> usize {
    debug_assert!(count >= 1 && bit + count <= FIELD_BITS);
    if count == FIELD_BITS {
        !0
    } else {
        ((1usize << count) - 1) << bit
    }
}

/// Lock-free single-block claim within one field (the hot path's inner CAS
/// loop). `limit` is the number of valid blocks in this field (`<=
/// FIELD_BITS`). Returns the claimed bit index on success, or `None` if the
/// field is full. This is the lost-update / double-claim hazard: the CAS
/// guarantees no two threads ever take the same bit.
pub fn claim_single_field<W: BitmapWord>(inuse: &W, limit: usize) -> Option<usize> {
    let mut cur = inuse.load(Ordering::Relaxed);
    loop {
        let avail = if limit == FIELD_BITS {
            !0
        } else {
            (1usize << limit) - 1
        };
        let free = !cur & avail;
        if free == 0 {
            return None;
        }
        let bit = free.trailing_zeros() as usize;
        match inuse.compare_exchange_weak(
            cur,
            cur | (1 << bit),
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(bit),
            Err(v) => cur = v,
        }
    }
}

/// Multi-block claim within one field, *under the caller's claim lock*: scan
/// for a `count`-wide free run and take it with a CAS. `limit` is the number
/// of valid blocks in this field. Returns the run's start bit on success.
/// The caller serializes these on the per-arena spinlock (excluding other
/// multi-block claims), but the lock-free single-block path and purge
/// race-modify the same field, so the take must be a CAS that re-validates
/// the run is *still* free — a blind `fetch_or` would co-claim a bit a
/// single-block CAS grabbed between the scan's load and the take.
pub fn claim_run_field<W: BitmapWord>(inuse: &W, limit: usize, count: usize) -> Option<usize> {
    if limit < count {
        return None;
    }
    let mut cur = inuse.load(Ordering::Relaxed);
    let mut bit = 0;
    while bit <= limit - count {
        let mask = field_mask(bit, count);
        if cur & mask != 0 {
            bit += 1;
            continue;
        }
        match inuse.compare_exchange_weak(cur, cur | mask, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return Some(bit),
            // a racing single-block claim or purge moved the field: rescan
            // from the start against the fresh value without advancing `bit`.
            Err(v) => {
                cur = v;
                bit = 0;
            }
        }
    }
    None
}

/// Post-claim commit bookkeeping for a `[bit, bit+count)` run in one field.
/// Sets the dirty and committed bits, clears any pending purge, and — when
/// the committed bit was *not* already set — invokes `commit` (the OS
/// recommit, or under loom the shadow-flag set). Returns `is_zero` (the
/// range was never dirtied). This is the commit-before-use guarantee: a
/// claimer that observes the committed bit clear MUST recommit before the
/// caller reads the memory.
pub fn prepare_run<W: BitmapWord>(
    dirty: &W,
    committed: &W,
    purge: &W,
    bit: usize,
    count: usize,
    commit: impl FnOnce(),
) -> bool {
    let mask = field_mask(bit, count);
    let was_dirty = dirty.fetch_or(mask, Ordering::AcqRel) & mask;
    let was_committed = committed.fetch_or(mask, Ordering::AcqRel) & mask;
    purge.fetch_and(!mask, Ordering::AcqRel);
    if was_committed != mask {
        commit();
    }
    was_dirty == 0
}

/// Schedule a `[bit, bit+count)` run for purge and make it claimable again.
/// Sets the purge bits (so a later `purge_field` can decommit the still-free
/// run), then clears the inuse bits. The order matters: purge must be
/// visible before the blocks become claimable, so a re-claimer that sees the
/// committed bit set is the one responsible for the live commit state.
/// Returns the previous inuse word (the caller asserts the run was held).
pub fn release_run<W: BitmapWord>(inuse: &W, purge: &W, bit: usize, count: usize) -> usize {
    let mask = field_mask(bit, count);
    purge.fetch_or(mask, Ordering::AcqRel);
    inuse.fetch_and(!mask, Ordering::AcqRel)
}

/// Clear the committed bits for a `[bit, bit+count)` run (the cross-layer
/// fix in `arena::free`): a dying segment that decommitted parts of its
/// range marks the whole range uncommitted so the next claimer recommits it,
/// never handing out PROT_NONE memory.
pub fn clear_committed_run<W: BitmapWord>(committed: &W, bit: usize, count: usize) {
    committed.fetch_and(!field_mask(bit, count), Ordering::AcqRel);
}

/// Drive the decommit half of `purge_now` for one field. Takes the pending
/// purge word, restricts it to blocks still free (not reclaimed since), and
/// for each maximal free run in *decommit* mode hands it to `decommit_run`.
///
/// `decommit_run(bit, len)` performs the non-interference handshake: take the
/// run out of circulation via `inuse.fetch_or` so a concurrent claimer that
/// saw the committed bits set cannot touch memory mid-decommit, clear the
/// committed bit, decommit (OS, or under loom clear the shadow flag), then
/// return the run to circulation. Runs reclaimed between the snapshot and the
/// `fetch_or` are skipped. `decommit_run` returns the run mask it actually
/// decommitted (0 if skipped). In *reset* mode the caller passes a
/// `decommit_run` that just resets without touching the bitmaps.
pub fn purge_field<W: BitmapWord>(purge: &W, inuse: &W, mut purge_run: impl FnMut(usize, usize)) {
    let pending = purge.swap(0, Ordering::AcqRel);
    if pending == 0 {
        return;
    }
    let purgeable = pending & !inuse.load(Ordering::Acquire);
    let mut m = purgeable;
    while m != 0 {
        let bit = m.trailing_zeros() as usize;
        let mut run_len = 0;
        while bit + run_len < FIELD_BITS && m & (1 << (bit + run_len)) != 0 {
            run_len += 1;
        }
        purge_run(bit, run_len);
        m &= !field_mask(bit, run_len);
    }
}

/// The decommit-mode handshake for a single run in `purge_field`: take it out
/// of circulation, and if it was still free, clear committed + decommit +
/// return it. `decommit(bit, len)` is the OS syscall (or loom shadow clear).
/// Returns `true` if the run was decommitted (vs. skipped because reclaimed).
pub fn decommit_run<W: BitmapWord>(
    inuse: &W,
    committed: &W,
    bit: usize,
    count: usize,
    decommit: impl FnOnce(),
) -> bool {
    let run = field_mask(bit, count);
    if inuse.fetch_or(run, Ordering::AcqRel) & run == 0 {
        committed.fetch_and(!run, Ordering::AcqRel);
        decommit();
        inuse.fetch_and(!run, Ordering::AcqRel);
        true
    } else {
        false
    }
}
