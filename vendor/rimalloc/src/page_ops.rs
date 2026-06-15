//! The core of the allocator (port of `page.c`): page init/extend, free-list
//! collection, retire, and the generic allocation path.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::constants::*;
use crate::heap::{Heap, PageQueue};
use crate::page::{Block, Delayed, Page};

const MAX_RETIRE_CYCLES: u8 = 16;
const MAX_EXTEND_SIZE: usize = 4 * 1024;
const MIN_EXTEND: usize = 4;
const MAX_CANDIDATE_SEARCH: usize = 4;

/// Block `i` of a page (`mi_page_block_at`).
#[inline(always)]
fn page_block_at(page_start: *mut u8, block_size: usize, i: usize) -> *mut Block {
    // SAFETY: caller keeps i <= reserved; blocks lie inside the page area.
    unsafe { page_start.add(i * block_size).cast() }
}

// ---------------------------------------------------------------------------
// Delayed-free flag transitions
// ---------------------------------------------------------------------------

/// `_mi_page_try_use_delayed_free`
pub fn page_try_use_delayed_free(page: &Page, delay: Delayed, override_never: bool) -> bool {
    crate::protocol::try_use_delayed(
        &page.xthread_free,
        delay as usize,
        override_never,
        std::thread::yield_now,
    )
}

/// `_mi_page_use_delayed_free`
pub fn page_use_delayed_free(page: &Page, delay: Delayed, override_never: bool) {
    while !page_try_use_delayed_free(page, delay, override_never) {
        std::thread::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Collect free lists
// ---------------------------------------------------------------------------

/// `_mi_page_thread_free_collect`: atomically take the cross-thread free
/// list. The pushed count rides in the taken word, so the common case
/// (the malloc `free` list is exhausted — that is why we are collecting)
/// adopts the chain as-is without pointer-chasing remote cache lines;
/// only a non-empty `free` list requires walking to the tail to splice
/// into `local_free`.
fn page_thread_free_collect(page: &Page) {
    let taken = crate::page::ThreadFree(crate::protocol::collect_take(&page.xthread_free));
    let head = taken.block();
    if head.is_null() {
        return;
    }
    let count = taken.count();
    if count == 0 || count > page.capacity.get() as usize || count > page.used.get() as usize {
        crate::error("corrupted thread-free list");
        return;
    }
    if page.free.get().is_null() {
        page.free.set(head);
        page.free_is_zero.set(false);
    } else {
        let mut tail = head;
        for _ in 1..count {
            // SAFETY: list nodes are free blocks in this page (ownership of
            // the whole list was transferred by collect_take's AcqRel swap).
            tail = unsafe { page.block_next(tail) };
            if tail.is_null() {
                crate::error("corrupted thread-free list");
                return;
            }
        }
        // SAFETY: tail is a live node of this page.
        unsafe { page.block_set_next(tail, page.local_free.get()) };
        page.local_free.set(head);
    }
    page.used.set(page.used.get() - count as u16);
}

/// Assert the Lean-proven counting invariant (`FreeList.lean::Inv`) on a
/// live page: `used + |free| + |local_free| = capacity <= reserved`.
/// Debug builds only — list walks are linear.
#[cfg(debug_assertions)]
pub fn page_assert_invariant(page: &Page) {
    if page.capacity.get() == 0 {
        return; // not initialized yet (fresh or cleared page)
    }
    let count = |mut p: *mut Block| {
        let mut n = 0usize;
        while !p.is_null() {
            n += 1;
            debug_assert!(
                n <= page.capacity.get() as usize,
                "free list longer than capacity"
            );
            // SAFETY: list nodes are live blocks of this page.
            p = unsafe { page.block_next(p) };
        }
        n
    };
    let free = count(page.free.get());
    let local = count(page.local_free.get());
    debug_assert_eq!(
        page.used.get() as usize + free + local,
        page.capacity.get() as usize,
        "page counting invariant violated (used={} |free|={free} |local|={local} cap={})",
        page.used.get(),
        page.capacity.get(),
    );
    debug_assert!(page.capacity.get() <= page.reserved.get());
}

/// `_mi_page_free_collect`
#[inline]
pub fn page_free_collect(page: &Page, force: bool) {
    if force || !page.thread_free().block().is_null() {
        page_thread_free_collect(page);
    }
    if !page.local_free.get().is_null() {
        if page.free.get().is_null() {
            page.free.set(page.local_free.get());
            page.local_free.set(ptr::null_mut());
            page.free_is_zero.set(false);
        } else if force {
            // append (linear); only on shutdown
            // SAFETY: list nodes are free blocks in this page.
            unsafe {
                let mut tail = page.local_free.get();
                while !page.block_next(tail).is_null() {
                    tail = page.block_next(tail);
                }
                page.block_set_next(tail, page.free.get());
            }
            page.free.set(page.local_free.get());
            page.local_free.set(ptr::null_mut());
            page.free_is_zero.set(false);
        }
    }
    #[cfg(debug_assertions)]
    page_assert_invariant(page);
}

// ---------------------------------------------------------------------------
// Fresh pages and reclaim
// ---------------------------------------------------------------------------

/// `_mi_page_reclaim` (from segment reclaim).
pub fn page_reclaim(heap: &Heap, page: &Page) {
    debug_assert!(page.heap() == ptr::from_ref(heap).cast_mut());
    let pq = heap.page_queue(page.block_size());
    heap.page_queue_push(pq, page);
}

/// `mi_page_fresh_alloc`
fn page_fresh_alloc<'a>(
    heap: &'a Heap,
    pq: Option<&PageQueue>,
    block_size: usize,
    page_alignment: usize,
) -> Option<&'a Page> {
    let page =
        crate::segment::segment_page_alloc(heap, block_size, page_alignment, heap.segments_tld())?;
    let full_block_size = if pq.is_none() || page.is_huge.get() {
        page.block_size()
    } else {
        block_size
    };
    page_init(heap, page, full_block_size);
    crate::stats::STATS.pages.increase(1);
    if let Some(pq) = pq {
        heap.page_queue_push(pq, page);
    }
    Some(page)
}

/// `mi_page_fresh`
fn page_fresh<'a>(heap: &'a Heap, pq: &PageQueue) -> Option<&'a Page> {
    page_fresh_alloc(heap, Some(pq), pq.block_size, 0)
}

// ---------------------------------------------------------------------------
// Heap delayed free
// ---------------------------------------------------------------------------

/// `_mi_heap_delayed_free_partial`: free blocks other threads pushed on our
/// heap's delayed list. Returns false if some were contended.
pub fn heap_delayed_free_partial(heap: &Heap) -> bool {
    // Links of delayed blocks are encoded with their *page's* keys (set by
    // `mt_push` via `block_set_next`); the page is recovered from the
    // block address.
    let link_page = |addr: usize| -> &Page {
        // SAFETY: delayed blocks live in pages of segments owned by us.
        unsafe { crate::segment::ptr_page(ptr::with_exposed_provenance(addr)) }
    };
    crate::protocol::delayed_partial(
        &heap.thread_delayed_free,
        |addr| {
            // SAFETY: we own the taken list.
            unsafe { link_page(addr).block_next(ptr::with_exposed_provenance_mut(addr)) }
                .expose_provenance()
        },
        |addr, next| {
            // SAFETY: re-queueing a block we own; write-only.
            unsafe {
                link_page(addr).block_set_next(
                    ptr::with_exposed_provenance_mut(addr),
                    ptr::with_exposed_provenance_mut(next),
                )
            }
        },
        |addr| crate::free::free_delayed_block(ptr::with_exposed_provenance_mut(addr)),
    )
}

/// `_mi_heap_delayed_free_all`
pub fn heap_delayed_free_all(heap: &Heap) {
    while !heap_delayed_free_partial(heap) {
        std::thread::yield_now();
    }
}

// ---------------------------------------------------------------------------
// Unfull, abandon, free, retire
// ---------------------------------------------------------------------------

/// `_mi_page_unfull`
pub fn page_unfull(page: &Page) {
    debug_assert!(page.in_full());
    if !page.in_full() {
        return;
    }
    // SAFETY: called on the owning thread; the heap is live.
    let heap = unsafe { &*page.heap() };
    let pqfull = &heap.pages[crate::bins::Bin::FULL.index()];
    heap.page_set_in_full(page, false); // to compute the right queue
    let pq = heap.page_queue_of(page);
    heap.page_set_in_full(page, true);
    heap.page_queue_enqueue_from(pq, pqfull, page);
}

/// `mi_page_to_full`
fn page_to_full(page: &Page, pq: &PageQueue) {
    debug_assert!(!page.immediate_available() && !page.in_full());
    if page.in_full() {
        return;
    }
    // SAFETY: owning thread; heap is live.
    let heap = unsafe { &*page.heap() };
    heap.page_queue_enqueue_from(&heap.pages[crate::bins::Bin::FULL.index()], pq, page);
    page_free_collect(page, false); // collect frees racing with the move
}

/// `_mi_page_abandon`
pub fn page_abandon(page: &Page, pq: &PageQueue) {
    // SAFETY: owning thread; heap is live.
    let heap = unsafe { &*page.heap() };
    let segments_tld = heap.segments_tld();
    heap.page_queue_remove(pq, page);
    debug_assert!(page.thread_free().delayed() == Delayed::NeverDelayedFree);
    page.set_heap(ptr::null_mut());
    page.segment().page_abandon(page, segments_tld);
}

/// `_mi_page_force_abandon` (used by `segments_try_abandon`, not yet ported)
pub fn page_force_abandon(page: &Page) {
    // SAFETY: owning thread; heap is live.
    let heap = unsafe { &*page.heap() };
    page_use_delayed_free(page, Delayed::NeverDelayedFree, false);
    heap_delayed_free_all(heap);
    if page.capacity.get() == 0 {
        return; // freed already
    }
    let pq = heap.page_queue_of(page);
    if page.all_free() {
        page_free(page, pq, false);
    } else {
        page_abandon(page, pq);
    }
}

/// `_mi_page_free`: free a page with no used blocks.
pub fn page_free(page: &Page, pq: &PageQueue, force: bool) {
    debug_assert!(page.all_free());
    debug_assert!(page.thread_free().delayed() != Delayed::Freeing);
    page.set_has_aligned(false);
    // SAFETY: owning thread; heap is live.
    let heap = unsafe { &*page.heap() };
    let segments_tld = heap.segments_tld();
    heap.page_queue_remove(pq, page);
    page.set_heap(ptr::null_mut());
    page.segment().page_free(page, force, segments_tld);
}

/// `_mi_page_retire`: a page just became all-free; retire it lazily if it is
/// the only page in its queue (new allocations are likely coming).
pub fn page_retire(page: &Page) {
    debug_assert!(page.all_free());
    page.set_has_aligned(false);
    // SAFETY: owning thread; heap is live.
    let heap = unsafe { &*page.heap() };
    let pq = heap.page_queue_of(page);
    let bsize = page.block_size();
    if !pq.is_special() {
        let page_ptr = ptr::from_ref(page).cast_mut();
        if pq.last.get() == page_ptr && pq.first.get() == page_ptr {
            page.retire_expire.set(if bsize <= SMALL_OBJ_SIZE_MAX {
                MAX_RETIRE_CYCLES
            } else {
                MAX_RETIRE_CYCLES / 4
            });
            let index =
                (ptr::from_ref(pq).addr() - heap.pages.as_ptr().addr()) / size_of::<PageQueue>();
            heap.page_retired_min
                .set(heap.page_retired_min.get().min(index));
            heap.page_retired_max
                .set(heap.page_retired_max.get().max(index));
            return; // don't free after all
        }
    }
    page_free(page, pq, false);
}

/// `_mi_heap_collect_retired`
pub fn heap_collect_retired(heap: &Heap, force: bool) {
    let mut min = BIN_FULL;
    let mut max = 0;
    for bin in heap.page_retired_min.get()..=heap.page_retired_max.get().min(BIN_FULL) {
        let pq = &heap.pages[bin];
        // SAFETY: queue nodes are live pages of this heap.
        if let Some(page) = unsafe { pq.first.get().as_ref() }
            && page.retire_expire.get() != 0
        {
            if page.all_free() {
                page.retire_expire.set(page.retire_expire.get() - 1);
                if force || page.retire_expire.get() == 0 {
                    page_free(page, pq, force);
                } else {
                    min = min.min(bin);
                    max = max.max(bin);
                }
            } else {
                page.retire_expire.set(0);
            }
        }
    }
    heap.page_retired_min.set(min);
    heap.page_retired_max.set(max);
}

// ---------------------------------------------------------------------------
// Page init / extend
// ---------------------------------------------------------------------------

/// `mi_page_free_list_extend`: build a sequential free list for blocks
/// `[capacity, capacity+extend)`.
fn page_free_list_extend(page: &Page, bsize: usize, extend: usize) {
    debug_assert!(page.free.get().is_null() && page.local_free.get().is_null());
    let page_area = page.start();
    let start = page_block_at(page_area, bsize, page.capacity.get() as usize);
    let last = page_block_at(page_area, bsize, page.capacity.get() as usize + extend - 1);
    let mut block = start;
    while block < last {
        // SAFETY: fresh blocks within the page area; write-only since their
        // memory is uninitialized.
        let next = unsafe { block.cast::<u8>().add(bsize).cast::<Block>() };
        // SAFETY: `block` is a fresh allocator-owned block.
        unsafe { page.block_set_next(block, next) };
        block = next;
    }
    // SAFETY: last is a fresh block; prepend to the existing free list.
    unsafe { page.block_set_next(last, page.free.get()) };
    page.free.set(start);
}

/// `mi_page_is_expandable`
#[inline]
fn page_is_expandable(page: &Page) -> bool {
    debug_assert!(page.capacity.get() <= page.reserved.get());
    page.capacity.get() < page.reserved.get()
}

/// `mi_page_extend_free`: extend capacity by initializing more free blocks
/// (at most one OS-page worth at a time to limit commit).
pub fn page_extend_free(page: &Page) -> bool {
    if !page.free.get().is_null() {
        return true;
    }
    if page.capacity.get() >= page.reserved.get() {
        return true;
    }
    let bsize = page.block_size();
    let mut extend = (page.reserved.get() - page.capacity.get()) as usize;
    let max_extend = if bsize >= MAX_EXTEND_SIZE {
        MIN_EXTEND
    } else {
        (MAX_EXTEND_SIZE / bsize).max(MIN_EXTEND)
    };
    extend = extend.min(max_extend);
    crate::stats::STATS
        .pages_extended
        .fetch_add(1, Ordering::Relaxed);
    page_free_list_extend(page, bsize, extend);
    page.capacity.set(page.capacity.get() + extend as u16);
    true
}

/// `mi_page_init`
fn page_init(heap: &Heap, page: &Page, block_size: usize) {
    let segment = page.segment();
    page.set_heap(ptr::from_ref(heap).cast_mut());
    page.set_block_size(block_size);
    let (start, page_size) = segment.page_start(page);
    page.page_start.set(start);
    debug_assert!(block_size <= page_size);
    debug_assert!(page_size / block_size < (1 << 16));
    page.reserved.set((page_size / block_size) as u16);
    debug_assert!(page.reserved.get() > 0);
    page.free_is_zero.set(page.is_zero_init.get());
    page.keys[0].set(crate::init::random_next() | 1);
    page.keys[1].set(crate::init::random_next() | 1);
    page.block_size_shift.set(if block_size.is_power_of_two() {
        block_size.trailing_zeros() as u8
    } else {
        0
    });
    debug_assert!(page.capacity.get() == 0 && page.free.get().is_null());
    debug_assert!(page.used.get() == 0 && page.next.get().is_null());
    page_extend_free(page);
}

// ---------------------------------------------------------------------------
// Find pages with free blocks
// ---------------------------------------------------------------------------

/// `mi_page_queue_find_free_ex`: next-fit search with candidate preference.
fn page_queue_find_free_ex<'a>(
    heap: &'a Heap,
    pq: &PageQueue,
    first_try: bool,
) -> Option<&'a Page> {
    let mut candidate_count = 0;
    let mut page_candidate: Option<&Page> = None;
    let mut p = pq.first.get();

    // SAFETY: queue nodes are live pages of this heap.
    while let Some(page) = unsafe { p.as_ref() } {
        let next = page.next.get();
        candidate_count += 1;
        page_free_collect(page, false);
        let immediate_available = page.immediate_available();

        if !immediate_available && !page_is_expandable(page) {
            // full: move to the full queue so we don't revisit it
            page_to_full(page, pq);
        } else {
            match page_candidate {
                None => {
                    page_candidate = Some(page);
                    candidate_count = 0;
                }
                // prefer to reuse fuller pages (so emptier ones can free up)
                Some(c)
                    if page.used.get() >= c.used.get()
                        && !page.is_mostly_used()
                        && !page_is_expandable(page) =>
                {
                    page_candidate = Some(page);
                }
                _ => {}
            }
            if immediate_available || candidate_count > MAX_CANDIDATE_SEARCH {
                break;
            }
        }
        p = next;
    }

    let mut page = page_candidate.or(
        // SAFETY: p is either null or a live page.
        unsafe { p.as_ref() },
    );
    if let Some(pg) = page
        && !pg.immediate_available()
    {
        debug_assert!(page_is_expandable(pg));
        if !page_extend_free(pg) {
            page = None;
        }
    }

    match page {
        None => {
            heap_collect_retired(heap, false); // perhaps make a page available
            let page = page_fresh(heap, pq);
            if page.is_none() && first_try {
                // out-of-memory _or_ reclaimed an abandoned page with blocks
                page_queue_find_free_ex(heap, pq, false)
            } else {
                page
            }
        }
        Some(page) => {
            heap.page_queue_move_to_front(pq, page);
            page.retire_expire.set(0);
            Some(page)
        }
    }
}

/// `mi_find_free_page`
fn find_free_page(heap: &Heap, size: usize) -> Option<&Page> {
    let pq = heap.page_queue(size);
    // SAFETY: queue nodes are live pages of this heap.
    if let Some(page) = unsafe { pq.first.get().as_ref() } {
        page_free_collect(page, false);
        if page.immediate_available() {
            page.retire_expire.set(0);
            return Some(page);
        }
    }
    page_queue_find_free_ex(heap, pq, true)
}

// ---------------------------------------------------------------------------
// General allocation
// ---------------------------------------------------------------------------

/// `mi_large_huge_page_alloc`
fn large_huge_page_alloc(heap: &Heap, size: usize, page_alignment: usize) -> Option<&Page> {
    let block_size = crate::os::good_alloc_size(size);
    let is_huge = block_size > LARGE_OBJ_SIZE_MAX || page_alignment > 0;
    let pq = heap.page_queue(if is_huge {
        LARGE_OBJ_SIZE_MAX + 1
    } else {
        block_size
    });
    debug_assert!(!is_huge || pq.is_huge());
    page_fresh_alloc(heap, Some(pq), block_size, page_alignment)
}

/// `mi_find_page`
fn find_page(heap: &Heap, size: usize, huge_alignment: usize) -> Option<&Page> {
    let req_size = size - PADDING_SIZE;
    if req_size > MEDIUM_OBJ_SIZE_MAX - PADDING_SIZE || huge_alignment > 0 {
        if req_size > MAX_ALLOC_SIZE {
            crate::error("allocation request is too large");
            None
        } else {
            large_huge_page_alloc(heap, size, huge_alignment)
        }
    } else {
        find_free_page(heap, size)
    }
}

/// `_mi_malloc_generic`: the slow path behind the malloc fast path.
pub fn malloc_generic(
    heap: &Heap,
    size: usize,
    zero: bool,
    huge_alignment: usize,
) -> Option<ptr::NonNull<u8>> {
    crate::os::drain_deferred();

    // initialize if necessary
    let heap = if !heap.is_initialized() {
        crate::init::heap_default()
    } else {
        heap
    };

    // administrative tasks every N generic calls
    heap.generic_count.set(heap.generic_count.get() + 1);
    if heap.generic_count.get() >= 100 {
        heap.generic_collect_count
            .set(heap.generic_collect_count.get() + heap.generic_count.get());
        heap.generic_count.set(0);
        crate::hooks::deferred_free(heap, false);
        heap_delayed_free_partial(heap);
        if heap.generic_collect_count.get() >= 10000 {
            heap.generic_collect_count.set(0);
            crate::heap_ops::heap_collect(heap, false);
        }
    }

    let page = match find_page(heap, size, huge_alignment) {
        Some(p) => Some(p),
        None => {
            // out of memory: collect hard and retry once
            crate::heap_ops::heap_collect(heap, true);
            find_page(heap, size, huge_alignment)
        }
    }?;

    debug_assert!(page.immediate_available());
    debug_assert!(page.block_size() >= size);

    crate::stats::stat_malloc(page.usable_block_size(), page.is_huge.get());
    let p = crate::alloc::page_malloc_zero(heap, page, size, zero);
    if page.reserved.get() == page.used.get() {
        page_to_full(page, heap.page_queue_of(page));
    }
    p
}
