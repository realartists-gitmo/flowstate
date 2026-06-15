//! Heaps (`mi_heap_t`): per-thread sets of page queues, one per size bin,
//! plus the `pages_free_direct` array that maps small word-sizes straight to
//! a page for the malloc fast path.

use core::cell::Cell;
use core::ptr;
use core::sync::atomic::AtomicUsize;

use crate::bins::{BIN_WSIZE, Bin};
use crate::constants::*;
use crate::page::Page;
use crate::segment::SegmentsTld;

/// `mi_page_queue_t`: doubly-linked queue of pages of one block size.
pub struct PageQueue {
    pub first: Cell<*mut Page>,
    pub last: Cell<*mut Page>,
    pub block_size: usize,
}

impl PageQueue {
    /// `mi_page_queue_is_huge`
    #[inline]
    pub fn is_huge(&self) -> bool {
        self.block_size == (MEDIUM_OBJ_SIZE_MAX + PTR_SIZE)
    }
    /// `mi_page_queue_is_full`
    #[inline]
    pub fn is_full(&self) -> bool {
        self.block_size == (MEDIUM_OBJ_SIZE_MAX + 2 * PTR_SIZE)
    }
    /// `mi_page_queue_is_special`
    #[inline]
    pub fn is_special(&self) -> bool {
        self.block_size > MEDIUM_OBJ_SIZE_MAX
    }
}

/// Thread-local data (`mi_tld_t`).
pub struct Tld {
    pub heartbeat: Cell<u64>,
    pub recurse: Cell<bool>,
    pub heap_backing: Cell<*mut Heap>,
    pub heaps: Cell<*mut Heap>, // list of this thread's heaps
    pub segments: SegmentsTld,
}

impl Tld {
    pub fn new() -> Tld {
        Tld {
            heartbeat: Cell::new(0),
            recurse: Cell::new(false),
            heap_backing: Cell::new(ptr::null_mut()),
            heaps: Cell::new(ptr::null_mut()),
            segments: SegmentsTld::new(),
        }
    }
}

/// `mi_heap_t`. Shared across threads only through `thread_delayed_free`
/// (atomic); everything else is owner-thread-only.
#[repr(C)]
pub struct Heap {
    pub tld: Cell<*mut Tld>,
    /// Exposed-address head of the delayed-free list (`*mut Block`).
    pub thread_delayed_free: AtomicUsize,
    pub thread_id: Cell<usize>,
    pub cookie: Cell<usize>,
    pub keys: [Cell<usize>; 2],
    pub page_count: Cell<usize>,
    pub page_retired_min: Cell<usize>,
    pub page_retired_max: Cell<usize>,
    pub pages_full_size: Cell<usize>,
    pub generic_count: Cell<usize>,
    pub generic_collect_count: Cell<usize>,
    pub next: Cell<*mut Heap>,
    pub no_reclaim: Cell<bool>,
    pub tag: Cell<u8>,
    /// Preferred/required arena (`mi_heap_t.arena_id`; 0 = any).
    pub arena_id: Cell<usize>,
    pub pages_free_direct: [Cell<*mut Page>; PAGES_DIRECT],
    pub pages: [PageQueue; BIN_FULL + 1],
}

// SAFETY: cross-thread access is restricted to `thread_delayed_free` and the
// immutable identity fields, mirroring mimalloc's discipline.
unsafe impl Sync for Heap {}

/// The static empty page that `pages_free_direct` points at when a bin has
/// no page: its `free` list is null so the fast path falls into the generic
/// path without a branch.
pub static EMPTY_PAGE: Page = Page::EMPTY;

#[inline(always)]
pub fn empty_page_ptr() -> *mut Page {
    ptr::from_ref(&EMPTY_PAGE).cast_mut()
}

impl Heap {
    /// An uninitialized heap with properly sized queues (`_mi_heap_empty`).
    pub fn new_empty() -> Heap {
        Heap {
            tld: Cell::new(ptr::null_mut()),
            thread_delayed_free: AtomicUsize::new(0),
            thread_id: Cell::new(0),
            cookie: Cell::new(0),
            keys: [Cell::new(0), Cell::new(0)],
            page_count: Cell::new(0),
            page_retired_min: Cell::new(BIN_FULL),
            page_retired_max: Cell::new(0),
            pages_full_size: Cell::new(0),
            generic_count: Cell::new(0),
            generic_collect_count: Cell::new(0),
            next: Cell::new(ptr::null_mut()),
            no_reclaim: Cell::new(false),
            tag: Cell::new(0),
            arena_id: Cell::new(0),
            pages_free_direct: [const { Cell::new(ptr::null_mut()) }; PAGES_DIRECT],
            pages: core::array::from_fn(|bin| PageQueue {
                first: Cell::new(ptr::null_mut()),
                last: Cell::new(ptr::null_mut()),
                block_size: BIN_WSIZE[bin] * PTR_SIZE,
            }),
        }
    }

    pub fn init_direct_pages(&self) {
        for slot in &self.pages_free_direct {
            slot.set(empty_page_ptr());
        }
    }

    #[inline(always)]
    pub fn segments_tld(&self) -> &SegmentsTld {
        // SAFETY: tld outlives its heaps.
        unsafe { &(*self.tld.get()).segments }
    }

    #[inline(always)]
    pub fn is_initialized(&self) -> bool {
        !self.tld.get().is_null()
    }

    /// `mi_page_queue` (by size).
    #[inline(always)]
    pub fn page_queue(&self, size: usize) -> &PageQueue {
        &self.pages[Bin::from_size(size).index()]
    }

    /// `mi_page_bin` + `mi_heap_page_queue_of`.
    pub fn page_queue_of(&self, page: &Page) -> &PageQueue {
        let bin = if page.in_full() {
            Bin::FULL
        } else if page.is_huge.get() {
            Bin::HUGE
        } else {
            Bin::from_size(page.block_size())
        };
        &self.pages[bin.index()]
    }

    /// `mi_page_set_in_full` (maintains `pages_full_size`).
    pub fn page_set_in_full(&self, page: &Page, in_full: bool) {
        if page.in_full() != in_full {
            let size = page.capacity.get() as usize * page.block_size();
            if in_full {
                self.pages_full_size.set(self.pages_full_size.get() + size);
            } else {
                self.pages_full_size.set(self.pages_full_size.get() - size);
            }
        }
        page.set_in_full(in_full);
    }

    /// `mi_heap_queue_first_update`: keep `pages_free_direct` in sync after
    /// the first page of a small queue changed.
    pub fn queue_first_update(&self, pq: &PageQueue) {
        let size = pq.block_size;
        if size > SMALL_SIZE_MAX {
            return;
        }
        let mut page = pq.first.get();
        if page.is_null() {
            page = empty_page_ptr();
        }
        let idx = wsize_from_size(size);
        if self.pages_free_direct[idx].get() == page {
            return;
        }
        let start = if idx <= 1 {
            0
        } else {
            // Skip back over queues that map to the same bin.
            let bin = Bin::from_size(size);
            let pq_index =
                (ptr::from_ref(pq).addr() - self.pages.as_ptr().addr()) / size_of::<PageQueue>();
            let mut prev = pq_index - 1;
            while prev > 0 && bin == Bin::from_size(self.pages[prev].block_size) {
                prev -= 1;
            }
            (1 + wsize_from_size(self.pages[prev].block_size)).min(idx)
        };
        for slot in &self.pages_free_direct[start..=idx] {
            slot.set(page);
        }
    }

    /// `mi_page_queue_remove`
    pub fn page_queue_remove(&self, pq: &PageQueue, page: &Page) {
        // SAFETY: queue nodes are live pages of this heap.
        unsafe {
            if let Some(prev) = page.prev.get().as_ref() {
                prev.next.set(page.next.get());
            }
            if let Some(next) = page.next.get().as_ref() {
                next.prev.set(page.prev.get());
            }
        }
        let page_ptr = ptr::from_ref(page).cast_mut();
        if page_ptr == pq.last.get() {
            pq.last.set(page.prev.get());
        }
        if page_ptr == pq.first.get() {
            pq.first.set(page.next.get());
            self.queue_first_update(pq);
        }
        self.page_count.set(self.page_count.get() - 1);
        page.next.set(ptr::null_mut());
        page.prev.set(ptr::null_mut());
        self.page_set_in_full(page, false);
    }

    /// `mi_page_queue_push`
    pub fn page_queue_push(&self, pq: &PageQueue, page: &Page) {
        let page_ptr = ptr::from_ref(page).cast_mut();
        self.page_set_in_full(page, pq.is_full());
        page.next.set(pq.first.get());
        page.prev.set(ptr::null_mut());
        // SAFETY: queue nodes are live pages of this heap.
        match unsafe { pq.first.get().as_ref() } {
            Some(first) => {
                first.prev.set(page_ptr);
                pq.first.set(page_ptr);
            }
            None => {
                pq.first.set(page_ptr);
                pq.last.set(page_ptr);
            }
        }
        self.queue_first_update(pq);
        self.page_count.set(self.page_count.get() + 1);
    }

    /// `mi_page_queue_move_to_front`
    pub fn page_queue_move_to_front(&self, pq: &PageQueue, page: &Page) {
        if pq.first.get() == ptr::from_ref(page).cast_mut() {
            return;
        }
        self.page_queue_remove(pq, page);
        self.page_queue_push(pq, page);
    }

    /// `mi_page_queue_enqueue_from` (always at the end, like mimalloc).
    pub fn page_queue_enqueue_from(&self, to: &PageQueue, from: &PageQueue, page: &Page) {
        let page_ptr = ptr::from_ref(page).cast_mut();
        // delete from `from`
        // SAFETY: queue nodes are live pages of this heap.
        unsafe {
            if let Some(prev) = page.prev.get().as_ref() {
                prev.next.set(page.next.get());
            }
            if let Some(next) = page.next.get().as_ref() {
                next.prev.set(page.prev.get());
            }
        }
        if page_ptr == from.last.get() {
            from.last.set(page.prev.get());
        }
        if page_ptr == from.first.get() {
            from.first.set(page.next.get());
            self.queue_first_update(from);
        }
        // insert at the end of `to`
        page.prev.set(to.last.get());
        page.next.set(ptr::null_mut());
        // SAFETY: queue nodes are live pages of this heap.
        match unsafe { to.last.get().as_ref() } {
            Some(last) => {
                last.next.set(page_ptr);
                to.last.set(page_ptr);
            }
            None => {
                to.first.set(page_ptr);
                to.last.set(page_ptr);
                self.queue_first_update(to);
            }
        }
        self.page_set_in_full(page, to.is_full());
    }

    /// The heap on this thread with the given tag (`_mi_heap_by_tag`).
    pub fn by_tag(&self, tag: u8) -> Option<&Heap> {
        if self.tag.get() == tag {
            return Some(self);
        }
        // SAFETY: heaps list nodes are live heaps of this thread.
        unsafe {
            let mut h = (*self.tld.get()).heaps.get();
            while let Some(heap) = h.as_ref() {
                if heap.tag.get() == tag {
                    return Some(heap);
                }
                h = heap.next.get();
            }
        }
        None
    }
}

/// Direct-fit page for very small sizes (`_mi_heap_get_free_small_page`).
#[inline(always)]
pub fn heap_get_free_small_page(heap: &Heap, size: usize) -> *mut Page {
    let idx = wsize_from_size(size);
    debug_assert!(idx < PAGES_DIRECT);
    heap.pages_free_direct[idx].get()
}
