//! The free path (port of `free.c`), including cross-thread frees that push
//! onto the page's atomic `xthread_free` list or the heap's delayed list.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::page::{Block, Delayed, Page};
use crate::page_ops;
use crate::segment::{Segment, SegmentKind, ptr_segment};

/// `mi_check_is_double_free` (`secure` feature): if the block's first word
/// decodes to a plausible pointer into the same page, confirm by walking
/// the page's free lists for an exact match. Reads one word of possibly
/// uninitialized user memory, so it is compiled out under Miri.
#[cfg(all(feature = "secure", not(miri)))]
fn check_is_double_free(page: &Page, block: *mut Block) -> bool {
    // SAFETY: block points into the page's committed area.
    let n = unsafe { page.block_next(block) };
    let start = page.page_start.get().addr();
    let end = start + page.capacity.get() as usize * page.block_size();
    let suspicious = n.addr() % size_of::<usize>() == 0
        && (n.is_null() || (n.addr() >= start && n.addr() < end));
    if !suspicious {
        return false;
    }
    // confirm: is `block` actually on one of the free lists?
    let on_list = |mut p: *mut Block| {
        let mut count = page.capacity.get() as usize;
        while !p.is_null() && count > 0 {
            if p == block {
                return true;
            }
            // SAFETY: list nodes are live blocks of this page.
            unsafe {
                p = page.block_next(p);
            }
            count -= 1;
        }
        false
    };
    if on_list(page.free.get())
        || on_list(page.local_free.get())
        || on_list(page.thread_free().block())
    {
        crate::error("double free detected");
        return true;
    }
    false
}

#[cfg(not(all(feature = "secure", not(miri))))]
#[inline(always)]
fn check_is_double_free(_page: &Page, _block: *mut Block) -> bool {
    false
}

/// `mi_free_block_local`: thread-local free; push on `local_free`.
#[inline(always)]
fn free_block_local(page: &Page, block: *mut Block, check_full: bool) {
    if check_is_double_free(page, block) {
        return;
    }
    if cfg!(feature = "debug-fill") && !page.is_huge.get() {
        // MI_DEBUG_FREED: stamp freed blocks to surface use-after-free.
        // SAFETY: the block is dead; the next-pointer word is rewritten below.
        unsafe {
            core::ptr::write_bytes(block.cast::<u8>(), 0xDF, page.block_size());
        }
    }
    // SAFETY: the caller owns this (possibly partially-initialized) block;
    // write-only, as the old contents may be uninitialized.
    unsafe { page.block_set_next(block, page.local_free.get()) };
    page.local_free.set(block);
    let used = page.used.get() - 1;
    page.used.set(used);
    if used == 0 {
        page_ops::page_retire(page);
    } else if check_full && page.in_full() {
        page_ops::page_unfull(page);
    }
}

/// `_mi_page_ptr_unalign`: start of the block containing `p`.
pub fn page_ptr_unalign(page: &Page, p: *mut u8) -> *mut Block {
    let diff = p.addr() - page.page_start.get().addr();
    let shift = page.block_size_shift.get();
    let adjust = if shift != 0 {
        diff & ((1 << shift) - 1)
    } else {
        let recip = page.block_recip.get();
        if recip != 0 {
            // Lemire fastmod, exact since `diff` and `block_size` are < 2^32
            // whenever the reciprocal is set (the page area of any page with
            // a < 4 GiB block size is itself < 4 GiB).
            let low = recip.wrapping_mul(diff as u64);
            let adjust = ((low as u128 * page.block_size.get() as u128) >> 64) as usize;
            debug_assert_eq!(adjust, diff % page.block_size.get());
            adjust
        } else {
            diff % page.block_size.get()
        }
    };
    p.map_addr(|a| a - adjust).cast()
}

/// `mi_free_generic_local`
fn free_generic_local(page: &Page, p: *mut u8) {
    let block = if page.has_aligned() {
        page_ptr_unalign(page, p)
    } else {
        Block::at(p)
    };
    free_block_local(page, block, true);
}

/// `mi_free_block_delayed_mt`: push a foreign-owned block on the page's
/// thread-free list, or the owning heap's delayed list if this is the first
/// concurrent free into a full page.
fn free_block_delayed_mt(page: &Page, block: *mut Block) {
    crate::protocol::mt_push(
        &page.xthread_free,
        || {
            // The racy heap read is safe because FREEING is held.
            let heap = page.xheap.load(Ordering::Acquire);
            // SAFETY: heap stays valid while any page holds FREEING.
            unsafe {
                ptr::with_exposed_provenance::<crate::heap::Heap>(heap)
                    .as_ref()
                    .map(|h| &h.thread_delayed_free)
            }
        },
        block.expose_provenance(),
        |next| {
            // SAFETY: we own the dying block; write-only (its contents may
            // be uninitialized user memory). Page-keyed encoding for the
            // page list; the heap-keyed delayed encoding is applied by the
            // owner's decode in heap_delayed_free_partial symmetrically.
            unsafe {
                page.block_set_next(block, ptr::with_exposed_provenance_mut(next));
            }
        },
    );
}

/// Abandoned-segment handling for a cross-thread free: try to reclaim the
/// segment into our heap so the free becomes local. Outlined so the hot
/// `free_block_mt` path pays no TLS access for this rare case.
#[cold]
#[inline(never)]
fn free_block_mt_reclaim(segment: &Segment, p: *mut u8) -> bool {
    if !crate::init::thread_is_initialized() {
        return false;
    }
    let heap = crate::init::heap_default();
    if crate::segment::attempt_reclaim(heap, segment) {
        // now it is a local free in our heap
        free(p);
        true
    } else {
        false
    }
}

/// `mi_free_block_mt`
fn free_block_mt(page: &Page, segment: &Segment, block: *mut Block, p: *mut u8) {
    // Try to reclaim an abandoned segment into our heap first (off by
    // default, as in C mimalloc's `abandoned_reclaim_on_free`).
    if segment.is_abandoned()
        && crate::options::abandoned_reclaim_on_free() != 0
        && free_block_mt_reclaim(segment, p)
    {
        return;
    }
    if segment.kind.get() == SegmentKind::Huge {
        crate::segment::huge_page_reset(segment, page, block);
    }
    free_block_delayed_mt(page, block);
}

/// `mi_free_generic_mt`
#[inline(always)]
fn free_generic_mt(page: &Page, segment: &Segment, p: *mut u8) {
    // don't check `has_aligned` to avoid a race (issue #865)
    let block = page_ptr_unalign(page, p);
    free_block_mt(page, segment, block, p);
}

/// `mi_free`
#[inline(always)]
pub fn free(p: *mut u8) {
    if p.is_null() || (p.addr() as isize) <= 0 {
        return;
    }
    // SAFETY: a non-null `p` handed to free must come from our allocator,
    // hence points into a live segment.
    let segment = unsafe { ptr_segment(p) };
    let is_local = crate::init::thread_id() == segment.thread_id.load(Ordering::Relaxed);
    let page = segment.page_of(p);
    if is_local {
        if !page.full_or_aligned() {
            free_block_local(page, Block::at(p), false);
        } else {
            free_generic_local(page, p);
        }
    } else {
        free_generic_mt(page, segment, p);
    }
}

/// `_mi_free_delayed_block`: returns false if contended.
pub fn free_delayed_block(block: *mut Block) -> bool {
    // SAFETY: delayed blocks live in pages of segments owned by this thread.
    let segment = unsafe { ptr_segment(block.cast()) };
    debug_assert!(crate::init::thread_id() == segment.thread_id.load(Ordering::Relaxed));
    let page = segment.page_of(block.cast());

    // Re-enable delayed freeing before collecting, or blocks could get
    // stranded on the page thread_free list with nothing on the heap list.
    if !page_ops::page_try_use_delayed_free(page, Delayed::UseDelayedFree, false) {
        return false;
    }
    page_ops::page_free_collect(page, false);
    free_block_local(page, block, true);
    true
}

/// `mi_usable_size`
pub fn usable_size(p: *const u8) -> usize {
    if p.is_null() {
        return 0;
    }
    // SAFETY: p was allocated by us (API contract).
    let segment = unsafe { ptr_segment(p) };
    let page = segment.page_of(p);
    if !page.has_aligned() {
        page.usable_block_size()
    } else {
        // adjust for interior aligned pointers
        let block = page_ptr_unalign(page, p.cast_mut());
        let size = page.usable_block_size();
        let adjust = p.addr() - block.addr();
        debug_assert!(adjust <= size);
        size - adjust
    }
}
