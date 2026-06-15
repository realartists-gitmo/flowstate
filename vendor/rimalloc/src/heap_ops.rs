//! Heap lifetime operations (port of `heap.c`): collect, delete, destroy.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::constants::*;
use crate::heap::{Heap, PageQueue};
use crate::page::{Delayed, Page};
use crate::page_ops;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Collect {
    Normal,
    Force,
    Abandon,
}

/// `mi_heap_visit_pages`: visit all pages (the visitor may free the page).
/// Returns false if the visitor broke out.
pub fn heap_visit_pages(heap: &Heap, mut f: impl FnMut(&Heap, &PageQueue, &Page) -> bool) -> bool {
    if heap.page_count.get() == 0 {
        return true;
    }
    for pq in &heap.pages {
        let mut p = pq.first.get();
        // SAFETY: queue nodes are live pages; we read `next` before visiting
        // since the visitor may free the page.
        while let Some(page) = unsafe { p.as_ref() } {
            let next = page.next.get();
            if !f(heap, pq, page) {
                return false;
            }
            p = next;
        }
    }
    true
}

/// `mi_heap_collect_ex`
pub fn heap_collect_ex(heap: &Heap, collect: Collect) {
    if !heap.is_initialized() {
        return;
    }
    let force = collect >= Collect::Force;
    crate::hooks::deferred_free(heap, force);

    // The backing heap reclaims all abandoned segments on a forced collect
    // (end-of-program): if all memory is freed by now, all segments free too.
    if collect == Collect::Force && heap_is_backing(heap) && !heap.no_reclaim.get() {
        crate::segment::reclaim_all(heap, heap.segments_tld());
    }

    // If abandoning, mark all pages to no longer add to delayed_free.
    if collect == Collect::Abandon {
        heap_visit_pages(heap, |_, _, page| {
            page_ops::page_use_delayed_free(page, Delayed::NeverDelayedFree, false);
            true
        });
    }

    // Free all current thread delayed blocks (after this, no more
    // thread-delayed references into our pages if abandoning).
    page_ops::heap_delayed_free_all(heap);

    // Collect retired pages.
    page_ops::heap_collect_retired(heap, force);

    // Collect all pages owned by this thread.
    heap_visit_pages(heap, |_, pq, page| {
        page_ops::page_free_collect(page, collect >= Collect::Force);
        if collect == Collect::Force {
            page.segment().try_purge(true);
        }
        if page.all_free() {
            page_ops::page_free(page, pq, collect >= Collect::Force);
        } else if collect == Collect::Abandon {
            page_ops::page_abandon(page, pq);
        }
        true
    });

    // Collect abandoned segments.
    crate::segment::abandoned_collect(heap, collect == Collect::Force, heap.segments_tld());
}

pub fn heap_collect(heap: &Heap, force: bool) {
    heap_collect_ex(
        heap,
        if force {
            Collect::Force
        } else {
            Collect::Normal
        },
    );
}

pub fn heap_collect_abandon(heap: &Heap) {
    heap_collect_ex(heap, Collect::Abandon);
}

/// `mi_heap_reset_pages`
fn heap_reset_pages(heap: &Heap) {
    heap.init_direct_pages();
    for pq in &heap.pages {
        pq.first.set(ptr::null_mut());
        pq.last.set(ptr::null_mut());
    }
    heap.page_count.set(0);
    heap.page_retired_min.set(BIN_FULL);
    heap.page_retired_max.set(0);
    heap.pages_full_size.set(0);
}

/// `mi_heap_free`: free the heap struct itself (allocated on the backing heap).
fn heap_free(heap: &Heap) {
    if heap_is_backing(heap) {
        return; // dont free the backing heap
    }
    // Remove from the thread-local heaps list.
    // SAFETY: tld and the heaps list are owned by this thread.
    unsafe {
        let tld = &*heap.tld.get();
        let mut prev: *mut Heap = ptr::null_mut();
        let mut curr = tld.heaps.get();
        while !curr.is_null() && curr != ptr::from_ref(heap).cast_mut() {
            prev = curr;
            curr = (*curr).next.get();
        }
        if !curr.is_null() {
            match prev.as_ref() {
                Some(prev) => prev.next.set((*curr).next.get()),
                None => tld.heaps.set((*curr).next.get()),
            }
        }
    }
    heap.tld.set(ptr::null_mut());
    crate::free::free(ptr::from_ref(heap).cast_mut().cast());
}

pub fn heap_is_backing(heap: &Heap) -> bool {
    // SAFETY: tld is owned by this thread.
    unsafe { (*heap.tld.get()).heap_backing.get() == ptr::from_ref(heap).cast_mut() }
}

/// `_mi_heap_init` for a fresh (non-backing) heap.
pub fn heap_init(
    heap: &Heap,
    tld: *mut crate::heap::Tld,
    no_reclaim: bool,
    tag: u8,
    arena_id: usize,
) {
    heap.tld.set(tld);
    heap.thread_id.set(crate::init::thread_id());
    heap.cookie.set(crate::init::random_next() | 1);
    heap.keys[0].set(crate::init::random_next());
    heap.keys[1].set(crate::init::random_next());
    heap.no_reclaim.set(no_reclaim);
    heap.tag.set(tag);
    heap.arena_id.set(arena_id);
    heap.init_direct_pages();
    // push on the thread-local heaps list
    // SAFETY: tld is owned by this thread.
    unsafe {
        heap.next.set((*tld).heaps.get());
        (*tld).heaps.set(ptr::from_ref(heap).cast_mut());
    }
}

/// `mi_heap_new_ex`
pub fn heap_new(allow_destroy: bool, tag: u8, arena_id: usize) -> Option<&'static Heap> {
    let bheap = crate::init::heap_default();
    let p = crate::alloc::heap_malloc_zero(bheap, size_of::<Heap>(), false)?;
    let heap_ptr = p.as_ptr().cast::<Heap>();
    // SAFETY: freshly allocated, sized and aligned for a Heap.
    unsafe { heap_ptr.write(Heap::new_empty()) };
    // SAFETY: just initialized above.
    let heap = unsafe { &*heap_ptr };
    heap_init(heap, bheap.tld.get(), allow_destroy, tag, arena_id);
    Some(heap)
}

/// `_mi_heap_page_destroy`
fn heap_page_destroy(heap: &Heap, page: &Page) {
    page_ops::page_use_delayed_free(page, Delayed::NeverDelayedFree, false);
    // pretend it is all free now
    page.used.set(0);
    page.next.set(ptr::null_mut());
    page.prev.set(ptr::null_mut());
    page.set_heap(ptr::null_mut());
    page.segment().page_free(page, false, heap.segments_tld());
}

/// `_mi_heap_destroy_pages`
pub fn heap_destroy_pages(heap: &Heap) {
    heap_visit_pages(heap, |heap, _, page| {
        heap_page_destroy(heap, page);
        true
    });
    heap_reset_pages(heap);
}

/// `mi_heap_destroy`: free all pages without freeing individual blocks.
pub fn heap_destroy(heap: &Heap) {
    if !heap.is_initialized() {
        return;
    }
    if !heap.no_reclaim.get() {
        // unsafe to destroy a heap that may contain reclaimed pages
        heap_delete(heap);
    } else {
        heap_destroy_pages(heap);
        heap_free(heap);
    }
}

/// `mi_heap_absorb`: move all pages of `from` into `heap`.
fn heap_absorb(heap: &Heap, from: &Heap) {
    if from.page_count.get() == 0 {
        return;
    }
    page_ops::heap_delayed_free_partial(from);
    for (pq, append) in heap.pages.iter().zip(&from.pages) {
        // set new heap field on every page; spins out DELAYED_FREEING
        let mut p = append.first.get();
        let mut count = 0;
        // SAFETY: queue nodes are live pages of `from`.
        while let Some(page) = unsafe { p.as_ref() } {
            page.xheap
                .store(ptr::from_ref(heap).expose_provenance(), Ordering::Release);
            page_ops::page_use_delayed_free(page, Delayed::UseDelayedFree, false);
            if page.in_full() {
                // transfer the full-queue size accounting along with the page
                // (`from`'s counter is wiped by heap_reset_pages below)
                let size = page.capacity.get() as usize * page.block_size();
                heap.pages_full_size.set(heap.pages_full_size.get() + size);
            }
            count += 1;
            p = page.next.get();
        }
        // append the queue
        if append.first.get().is_null() {
            continue;
        }
        // SAFETY: live pages.
        unsafe {
            if pq.last.get().is_null() {
                pq.first.set(append.first.get());
                pq.last.set(append.last.get());
                heap.queue_first_update(pq);
            } else {
                (*pq.last.get()).next.set(append.first.get());
                (*append.first.get()).prev.set(pq.last.get());
                pq.last.set(append.last.get());
            }
        }
        append.first.set(ptr::null_mut());
        append.last.set(ptr::null_mut());
        heap.page_count.set(heap.page_count.get() + count);
        from.page_count.set(from.page_count.get() - count);
    }
    debug_assert!(from.page_count.get() == 0);
    page_ops::heap_delayed_free_all(from);
    heap_reset_pages(from);
}

/// `mi_heap_delete`: delete a heap, transferring live pages to the backing
/// heap (or abandoning them for a backing heap).
pub fn heap_delete(heap: &Heap) {
    if !heap.is_initialized() {
        return;
    }
    // SAFETY: tld is owned by this thread.
    let bheap = unsafe { &*(*heap.tld.get()).heap_backing.get() };
    if !ptr::eq(bheap, heap) && bheap.tag.get() == heap.tag.get() {
        heap_absorb(bheap, heap);
    } else {
        heap_collect_abandon(heap);
    }
    heap_free(heap);
}
