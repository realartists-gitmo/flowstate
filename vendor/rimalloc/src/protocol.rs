//! The cross-thread delayed-free protocol — the *single* implementation,
//! generic over the atomic word type. Production (`free.rs`,
//! `page_ops.rs`) instantiates it with `core` atomics on the live
//! `Page`/`Heap` fields; `tests/loom_protocol.rs` instantiates the *same
//! functions* with loom atomics, so every interleaving of exactly this
//! code is model-checked. Block storage (next links) is abstracted as
//! read/write closures because blocks live in raw page memory in
//! production and in instrumented cells under loom; all CAS structure,
//! memory orderings, and flag transitions live here and only here.

use core::sync::atomic::Ordering;

pub const USE_DELAYED: usize = 0;
pub const FREEING: usize = 1;
pub const NO_DELAYED: usize = 2;
pub const NEVER_DELAYED: usize = 3;
pub const FLAG_MASK: usize = 3;

/// The thread-free word packs `count << 48 | block_ptr | delayed_flags`:
/// pushers increment the count so the owner's collect learns the list
/// length without walking it (pointer-chasing remote cache lines). User
/// addresses fit in 48 bits on all supported targets, and the count cannot
/// exceed a page's block capacity (< 2^16).
pub const TF_COUNT_UNIT: usize = 1 << TF_COUNT_SHIFT;
pub const TF_COUNT_SHIFT: u32 = 48;
pub const TF_PTR_MASK: usize = TF_COUNT_UNIT - 1 - FLAG_MASK;

/// Minimal atomic-word interface implemented by `core` and loom atomics.
pub trait AtomicWord {
    fn load(&self, order: Ordering) -> usize;
    fn store(&self, val: usize, order: Ordering);
    fn swap(&self, val: usize, order: Ordering) -> usize;
    fn compare_exchange_weak(
        &self,
        current: usize,
        new: usize,
        success: Ordering,
        failure: Ordering,
    ) -> Result<usize, usize>;
}

impl AtomicWord for core::sync::atomic::AtomicUsize {
    fn load(&self, order: Ordering) -> usize {
        self.load(order)
    }
    fn store(&self, val: usize, order: Ordering) {
        self.store(val, order)
    }
    fn swap(&self, val: usize, order: Ordering) -> usize {
        self.swap(val, order)
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

/// `mi_free_block_delayed_mt`: push a foreign-owned block (`block` is its
/// address-with-provenance-exposed) onto the page's thread-free list, or —
/// on the first concurrent free into a full page — onto the owning heap's
/// delayed list, guarded by the FREEING handshake. `write_next` stores a
/// next-link into the (exclusively owned) dying block without reading its
/// old contents.
pub fn mt_push<'d, A: AtomicWord, D: AtomicWord + 'd>(
    xthread: &A,
    delayed: impl FnOnce() -> Option<&'d D>,
    block: usize,
    mut write_next: impl FnMut(usize),
) {
    debug_assert!(block & !TF_PTR_MASK == 0 && block != 0);
    let mut tfree = xthread.load(Ordering::Relaxed);
    let use_delayed = loop {
        let use_delayed = tfree & FLAG_MASK == USE_DELAYED;
        let tfreex = if use_delayed {
            (tfree & !FLAG_MASK) | FREEING
        } else {
            write_next(tfree & TF_PTR_MASK);
            block | (tfree & !TF_PTR_MASK).wrapping_add(TF_COUNT_UNIT)
        };
        match xthread.compare_exchange_weak(tfree, tfreex, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => break use_delayed,
            Err(v) => tfree = v,
        }
    };
    if use_delayed {
        // First concurrent free into a full page: push on the heap's
        // delayed list (the heap stays valid while FREEING is held).
        if let Some(dlist) = delayed() {
            let mut dfree = dlist.load(Ordering::Relaxed);
            loop {
                write_next(dfree);
                match dlist.compare_exchange_weak(
                    dfree,
                    block,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(v) => dfree = v,
                }
            }
        }
        // reset FREEING to NO_DELAYED
        let mut tfree = xthread.load(Ordering::Relaxed);
        loop {
            debug_assert!(tfree & FLAG_MASK == FREEING);
            let tfreex = (tfree & !FLAG_MASK) | NO_DELAYED;
            match xthread.compare_exchange_weak(tfree, tfreex, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(v) => tfree = v,
            }
        }
    }
}

/// `_mi_page_thread_free_collect`'s atomic half: take the whole
/// thread-free list (head pointer *and* count), keeping the delayed flag
/// bits. Returns the taken word: decode with `TF_PTR_MASK` /
/// `TF_COUNT_SHIFT`.
pub fn collect_take<A: AtomicWord>(xthread: &A) -> usize {
    let mut tfree = xthread.load(Ordering::Relaxed);
    loop {
        match xthread.compare_exchange_weak(
            tfree,
            tfree & FLAG_MASK,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return tfree & !FLAG_MASK,
            Err(v) => tfree = v,
        }
    }
}

/// `_mi_page_try_use_delayed_free`: transition the delayed flag, spinning
/// (boundedly) while another thread holds FREEING.
pub fn try_use_delayed<A: AtomicWord>(
    xthread: &A,
    delay: usize,
    override_never: bool,
    mut yield_now: impl FnMut(),
) -> bool {
    let mut yield_count = 0;
    let mut tfree = xthread.load(Ordering::Acquire);
    loop {
        let old_delay = tfree & FLAG_MASK;
        if old_delay == FREEING {
            if yield_count >= 4 {
                return false;
            }
            yield_count += 1;
            yield_now();
            tfree = xthread.load(Ordering::Acquire);
            continue;
        }
        if delay == old_delay {
            return true;
        }
        if !override_never && old_delay == NEVER_DELAYED {
            return true;
        }
        let tfreex = (tfree & !FLAG_MASK) | delay;
        match xthread.compare_exchange_weak(tfree, tfreex, Ordering::Release, Ordering::Acquire) {
            Ok(_) => return true,
            Err(v) => tfree = v,
        }
    }
}

/// `_mi_heap_delayed_free_partial`'s atomic shell: take the heap's delayed
/// list, process each block with `process` (which returns `false` when
/// contended), and re-queue failures. `read_next` reads the link of a
/// block we own; `write_next` re-links one for re-queueing. Returns true
/// if everything was freed.
pub fn delayed_partial<D: AtomicWord>(
    delayed: &D,
    mut read_next: impl FnMut(usize) -> usize,
    mut write_next: impl FnMut(usize, usize),
    mut process: impl FnMut(usize) -> bool,
) -> bool {
    let mut block = delayed.load(Ordering::Relaxed);
    while block != 0 {
        match delayed.compare_exchange_weak(block, 0, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(v) => block = v,
        }
    }
    let mut all_freed = true;
    while block != 0 {
        let next = read_next(block);
        if !process(block) {
            // another thread still holds FREEING: requeue
            all_freed = false;
            let mut dfree = delayed.load(Ordering::Relaxed);
            loop {
                write_next(block, dfree);
                match delayed.compare_exchange_weak(
                    dfree,
                    block,
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(v) => dfree = v,
                }
            }
        }
        block = next;
    }
    all_freed
}
