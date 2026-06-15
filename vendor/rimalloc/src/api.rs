//! Public allocation API, mirroring `mimalloc.h` (`mi_*` functions) plus a
//! `GlobalAlloc` implementation.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

use crate::alloc::heap_malloc_zero;
use crate::constants::*;
use crate::heap::Heap;
use crate::init::heap_default_unchecked;
use crate::page_ops::malloc_generic;
use crate::segment::ptr_segment;

#[inline(always)]
fn as_ptr(p: Option<NonNull<u8>>) -> *mut u8 {
    p.map_or(ptr::null_mut(), NonNull::as_ptr)
}

// ---------------------------------------------------------------------------
// malloc / zalloc / calloc / free
// ---------------------------------------------------------------------------

/// `mi_malloc`
#[inline]
pub fn malloc(size: usize) -> *mut u8 {
    as_ptr(heap_malloc_zero(heap_default_unchecked(), size, false))
}

/// `mi_zalloc`
#[inline]
pub fn zalloc(size: usize) -> *mut u8 {
    as_ptr(heap_malloc_zero(heap_default_unchecked(), size, true))
}

/// `mi_malloc_small` (requires `size <= SMALL_SIZE_MAX`)
#[inline]
pub fn malloc_small(size: usize) -> *mut u8 {
    debug_assert!(size <= SMALL_SIZE_MAX);
    as_ptr(crate::alloc::heap_malloc_small_zero(
        heap_default_unchecked(),
        size,
        false,
    ))
}

/// `mi_count_size_overflow`-checked multiply.
#[inline]
fn count_size(count: usize, size: usize) -> Option<usize> {
    count.checked_mul(size).filter(|&t| t <= MAX_ALLOC_SIZE)
}

/// `mi_calloc`
#[inline]
pub fn calloc(count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => zalloc(total),
        None => ptr::null_mut(),
    }
}

/// `mi_mallocn`
#[inline]
pub fn mallocn(count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => malloc(total),
        None => ptr::null_mut(),
    }
}

/// `mi_free`
#[inline]
pub fn free(p: *mut u8) {
    crate::free::free(p)
}

/// `mi_usable_size`
#[inline]
pub fn usable_size(p: *const u8) -> usize {
    crate::free::usable_size(p)
}

/// `mi_good_size`
#[inline]
pub fn good_size(size: usize) -> usize {
    crate::bins::good_size(size)
}

// ---------------------------------------------------------------------------
// realloc family
// ---------------------------------------------------------------------------

/// Common tail of the realloc paths: copy into the new block (zeroing the
/// growth tail first), free the old block.
fn realloc_move(p: *mut u8, newp: *mut u8, oldsize: usize, newsize: usize, zero: bool) {
    if zero && newsize > oldsize {
        let start = oldsize.saturating_sub(size_of::<usize>());
        // SAFETY: newp has at least newsize usable bytes.
        unsafe { ptr::write_bytes(newp.add(start), 0, newsize - start) };
    }
    if newsize > 0 {
        // SAFETY: disjoint allocations, both at least min(old,new) long.
        unsafe { ptr::copy_nonoverlapping(p, newp, oldsize.min(newsize)) };
    }
    free(p);
}

/// `_mi_heap_realloc_zero`
fn heap_realloc_zero(heap: &Heap, p: *mut u8, newsize: usize, zero: bool) -> *mut u8 {
    if p.is_null() {
        return as_ptr(heap_malloc_zero(heap, newsize, zero));
    }
    let size = usable_size(p);
    // reallocation still fits and not more than 50% waste?
    if newsize <= size && newsize >= size / 2 && newsize > 0 {
        return p;
    }
    let newp = as_ptr(heap_malloc_zero(heap, newsize, false));
    if !newp.is_null() {
        if newsize == 0 {
            // applications expect zero-reallocation to be zeroed (C issue #725).
            // SAFETY: a zero-size block still has a usable first byte.
            unsafe { newp.write(0) };
        }
        realloc_move(p, newp, size, newsize, zero);
    }
    newp
}

/// `mi_realloc`
#[inline]
pub fn realloc(p: *mut u8, newsize: usize) -> *mut u8 {
    heap_realloc_zero(heap_default_unchecked(), p, newsize, false)
}

/// `mi_reallocn`
#[inline]
pub fn reallocn(p: *mut u8, count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => realloc(p, total),
        None => ptr::null_mut(),
    }
}

/// `mi_rezalloc`
#[inline]
pub fn rezalloc(p: *mut u8, newsize: usize) -> *mut u8 {
    heap_realloc_zero(heap_default_unchecked(), p, newsize, true)
}

/// `mi_recalloc`
#[inline]
pub fn recalloc(p: *mut u8, count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => rezalloc(p, total),
        None => ptr::null_mut(),
    }
}

/// `mi_reallocf`: free `p` on failure.
#[inline]
pub fn reallocf(p: *mut u8, newsize: usize) -> *mut u8 {
    let newp = realloc(p, newsize);
    if newp.is_null() && !p.is_null() {
        free(p);
    }
    newp
}

/// `mi_expand`: in-place only.
#[inline]
pub fn expand(p: *mut u8, newsize: usize) -> *mut u8 {
    if p.is_null() {
        return ptr::null_mut();
    }
    if newsize > usable_size(p) {
        ptr::null_mut()
    } else {
        p
    }
}

// ---------------------------------------------------------------------------
// Aligned allocation
// ---------------------------------------------------------------------------

/// `mi_malloc_is_naturally_aligned`
fn is_naturally_aligned(size: usize, alignment: usize) -> bool {
    debug_assert!(is_power_of_two(alignment));
    if alignment > size {
        return false;
    }
    if alignment <= MAX_ALIGN_SIZE {
        return true;
    }
    let bsize = good_size(size);
    bsize <= MAX_ALIGN_GUARANTEE && bsize & (alignment - 1) == 0
}

/// `mi_heap_malloc_zero_aligned_at_overalloc`
fn malloc_aligned_overalloc(
    heap: &Heap,
    size: usize,
    alignment: usize,
    offset: usize,
    zero: bool,
) -> *mut u8 {
    let p = if alignment > BLOCK_ALIGNMENT_MAX {
        // dedicated huge segment, aligned within the single page
        if offset != 0 {
            crate::error("aligned allocation with very large alignment cannot use an offset");
            return ptr::null_mut();
        }
        let oversize = if size <= SMALL_SIZE_MAX {
            SMALL_SIZE_MAX + 1
        } else {
            size
        };
        as_ptr(malloc_generic(heap, oversize, false, alignment))
    } else {
        let oversize = size.max(MAX_ALIGN_SIZE) + alignment - 1;
        as_ptr(heap_malloc_zero(heap, oversize, zero))
    };
    if p.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: p was just allocated from a live page.
    let page = unsafe { crate::segment::ptr_page(p) };
    let align_mask = alignment - 1;
    let poffset = (p.addr() + offset) & align_mask;
    let adjust = if poffset == 0 { 0 } else { alignment - poffset };
    // SAFETY: adjust < alignment <= usable slack of the oversized block.
    let aligned_p = unsafe { p.add(adjust) };
    if aligned_p != p {
        page.set_has_aligned(true);
    }
    debug_assert!((aligned_p.addr() + offset).is_multiple_of(alignment));
    if alignment > BLOCK_ALIGNMENT_MAX && zero {
        let usable = crate::free::usable_size(aligned_p);
        // SAFETY: aligned_p..+usable is within the block.
        unsafe { ptr::write_bytes(aligned_p, 0, usable) };
    }
    aligned_p
}

/// `mi_heap_malloc_zero_aligned_at`
fn malloc_zero_aligned_at(
    heap: &Heap,
    size: usize,
    alignment: usize,
    offset: usize,
    zero: bool,
) -> *mut u8 {
    if alignment == 0 || !is_power_of_two(alignment) {
        return ptr::null_mut();
    }
    if size > MAX_ALLOC_SIZE {
        return ptr::null_mut();
    }
    // Fast path: a small block with the right alignment.
    if offset == 0 && alignment <= size && size <= SMALL_SIZE_MAX {
        // SAFETY: pages_free_direct entries are live or the static empty page.
        let page = unsafe { &*crate::heap::heap_get_free_small_page(heap, size + PADDING_SIZE) };
        let head = page.free.get();
        if !head.is_null() && head.addr() & (alignment - 1) == 0 {
            return as_ptr(crate::alloc::page_malloc_zero(
                heap,
                page,
                size + PADDING_SIZE,
                zero,
            ));
        }
    }
    if offset == 0 && is_naturally_aligned(size, alignment) {
        let p = as_ptr(heap_malloc_zero(heap, size, zero));
        debug_assert!(p.is_null() || p.addr().is_multiple_of(alignment));
        if p.addr() & (alignment - 1) == 0 {
            return p;
        }
        free(p);
    }
    malloc_aligned_overalloc(heap, size, alignment, offset, zero)
}

/// `mi_malloc_aligned`
#[inline]
pub fn malloc_aligned(size: usize, alignment: usize) -> *mut u8 {
    malloc_zero_aligned_at(heap_default_unchecked(), size, alignment, 0, false)
}

/// `mi_malloc_aligned_at`
#[inline]
pub fn malloc_aligned_at(size: usize, alignment: usize, offset: usize) -> *mut u8 {
    malloc_zero_aligned_at(heap_default_unchecked(), size, alignment, offset, false)
}

/// `mi_zalloc_aligned`
#[inline]
pub fn zalloc_aligned(size: usize, alignment: usize) -> *mut u8 {
    malloc_zero_aligned_at(heap_default_unchecked(), size, alignment, 0, true)
}

/// `mi_zalloc_aligned_at`
#[inline]
pub fn zalloc_aligned_at(size: usize, alignment: usize, offset: usize) -> *mut u8 {
    malloc_zero_aligned_at(heap_default_unchecked(), size, alignment, offset, true)
}

/// `mi_calloc_aligned`
#[inline]
pub fn calloc_aligned(count: usize, size: usize, alignment: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => zalloc_aligned(total, alignment),
        None => ptr::null_mut(),
    }
}

/// `mi_calloc_aligned_at`
#[inline]
pub fn calloc_aligned_at(count: usize, size: usize, alignment: usize, offset: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => zalloc_aligned_at(total, alignment, offset),
        None => ptr::null_mut(),
    }
}

fn realloc_zero_aligned_at(
    p: *mut u8,
    newsize: usize,
    alignment: usize,
    offset: usize,
    zero: bool,
) -> *mut u8 {
    if !is_power_of_two(alignment) {
        return ptr::null_mut();
    }
    if p.is_null() {
        return malloc_zero_aligned_at(heap_default_unchecked(), newsize, alignment, offset, zero);
    }
    let size = usable_size(p);
    // still fits, aligned, and not more than ~25% waste (matches C)?
    if newsize <= size
        && newsize >= size - size / 2
        && (p.addr() + offset).is_multiple_of(alignment)
    {
        return p;
    }
    let newp = malloc_zero_aligned_at(heap_default_unchecked(), newsize, alignment, offset, zero);
    if !newp.is_null() {
        if newsize == 0 {
            // SAFETY: a zero-size block still has a usable first byte.
            unsafe { newp.write(0) };
        }
        realloc_move(p, newp, size, newsize, zero);
    }
    newp
}

/// `mi_realloc_aligned`
#[inline]
pub fn realloc_aligned(p: *mut u8, newsize: usize, alignment: usize) -> *mut u8 {
    realloc_zero_aligned_at(p, newsize, alignment, 0, false)
}

/// `mi_realloc_aligned_at`
#[inline]
pub fn realloc_aligned_at(p: *mut u8, newsize: usize, alignment: usize, offset: usize) -> *mut u8 {
    realloc_zero_aligned_at(p, newsize, alignment, offset, false)
}

/// `mi_rezalloc_aligned`
#[inline]
pub fn rezalloc_aligned(p: *mut u8, newsize: usize, alignment: usize) -> *mut u8 {
    realloc_zero_aligned_at(p, newsize, alignment, 0, true)
}

/// `mi_recalloc_aligned`
#[inline]
pub fn recalloc_aligned(p: *mut u8, count: usize, size: usize, alignment: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => rezalloc_aligned(p, total, alignment),
        None => ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// Heap API
// ---------------------------------------------------------------------------

/// `mi_heap_new`
pub fn heap_new() -> Option<&'static Heap> {
    crate::heap_ops::heap_new(true, 0, crate::arena::ARENA_ID_NONE)
}

/// `mi_heap_new_ex`: custom tag, destroy permission, and arena binding.
pub fn heap_new_ex(tag: u8, allow_destroy: bool, arena_id: usize) -> Option<&'static Heap> {
    crate::heap_ops::heap_new(allow_destroy, tag, arena_id)
}

/// `mi_heap_new_in_arena`: a heap allocating exclusively from `arena_id`.
pub fn heap_new_in_arena(arena_id: usize) -> Option<&'static Heap> {
    crate::heap_ops::heap_new(false, 0, arena_id)
}

/// `mi_reserve_os_memory_ex`: pre-reserve OS memory as an arena. Returns
/// the arena id (use with [`heap_new_in_arena`] when `exclusive`).
pub fn reserve_os_memory(size: usize, commit: bool, exclusive: bool) -> Option<usize> {
    crate::arena::reserve_os_memory(size, commit, exclusive)
}

/// `mi_manage_os_memory_ex`: adopt caller-provided memory as an arena
/// (never freed by rimalloc).
///
/// # Safety
/// See [`crate::arena::manage_os_memory`].
pub unsafe fn manage_os_memory(
    start: *mut u8,
    size: usize,
    is_committed: bool,
    is_zero: bool,
    numa_node: i32,
    exclusive: bool,
) -> Option<usize> {
    // SAFETY: per contract.
    unsafe {
        crate::arena::manage_os_memory(start, size, is_committed, is_zero, numa_node, exclusive)
    }
}

/// `mi_reserve_huge_os_pages_at`: reserve `pages` 1GiB huge OS pages as an
/// arena on `numa_node`. Returns the arena id, or `None` where huge OS
/// pages are unsupported (e.g. macOS arm64).
pub fn reserve_huge_os_pages_at(pages: usize, numa_node: i32) -> Option<usize> {
    crate::os::alloc_huge_os_pages(pages).and_then(|(start, mem)| {
        // SAFETY: fresh exclusive mapping from the OS.
        unsafe { crate::arena::manage_os_memory(start, mem.size, true, true, numa_node, false) }
    })
}

/// `mi_heap_get_default`
pub fn heap_get_default() -> &'static Heap {
    crate::init::heap_default()
}

/// `mi_heap_get_backing`
pub fn heap_get_backing() -> &'static Heap {
    let heap = heap_get_default();
    // SAFETY: tld and backing heap live for the thread lifetime.
    unsafe { &*(*heap.tld.get()).heap_backing.get() }
}

/// `mi_heap_set_default`
pub fn heap_set_default(heap: &Heap) -> &'static Heap {
    crate::init::heap_set_default(heap)
}

/// `mi_heap_delete`
pub fn heap_delete(heap: &Heap) {
    crate::heap_ops::heap_delete(heap)
}

/// `mi_heap_destroy`
pub fn heap_destroy(heap: &Heap) {
    crate::heap_ops::heap_destroy(heap)
}

/// `mi_heap_malloc`
#[inline]
pub fn heap_malloc(heap: &Heap, size: usize) -> *mut u8 {
    as_ptr(heap_malloc_zero(heap, size, false))
}

/// `mi_heap_zalloc`
#[inline]
pub fn heap_zalloc(heap: &Heap, size: usize) -> *mut u8 {
    as_ptr(heap_malloc_zero(heap, size, true))
}

/// `mi_heap_calloc`
#[inline]
pub fn heap_calloc(heap: &Heap, count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => heap_zalloc(heap, total),
        None => ptr::null_mut(),
    }
}

/// `mi_heap_mallocn`
#[inline]
pub fn heap_mallocn(heap: &Heap, count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => heap_malloc(heap, total),
        None => ptr::null_mut(),
    }
}

/// `mi_heap_realloc`
#[inline]
pub fn heap_realloc(heap: &Heap, p: *mut u8, newsize: usize) -> *mut u8 {
    heap_realloc_zero(heap, p, newsize, false)
}

/// `mi_heap_malloc_aligned`
#[inline]
pub fn heap_malloc_aligned(heap: &Heap, size: usize, alignment: usize) -> *mut u8 {
    malloc_zero_aligned_at(heap, size, alignment, 0, false)
}

/// `mi_collect`
pub fn collect(force: bool) {
    crate::heap_ops::heap_collect(heap_get_default(), force);
    crate::arena::try_purge(force);
    if force {
        crate::os::cache_collect();
    }
    crate::os::drain_deferred();
}

/// `mi_thread_init`
pub fn thread_init() {
    crate::init::thread_init();
}

/// `mi_thread_done`
pub fn thread_done() {
    crate::init::thread_done();
}

// ---------------------------------------------------------------------------
// Introspection
// ---------------------------------------------------------------------------

/// `mi_is_in_heap_region` (approximation: do we own the containing segment).
///
/// # Safety
/// `p` must be null or a pointer into memory obtained from this allocator.
pub unsafe fn check_owned(p: *const u8) -> bool {
    if p.is_null() || p.addr() & (PTR_SIZE - 1) != 0 {
        return false;
    }
    if crate::arena::contains(p) {
        return true;
    }
    // SAFETY: only reads the segment header derived from the address; a
    // wrong address may fault (debug-only helper).
    let segment = unsafe { ptr_segment(p) };
    segment.cookie.get() == segment.as_ptr().addr() ^ 0xa5a5_5a5a_1234_5678
}

/// `mi_is_in_heap_region`: true if `p` lies inside rimalloc-managed memory
/// (arena reservations); always safe to call.
pub fn is_in_heap_region(p: *const u8) -> bool {
    !p.is_null() && crate::arena::contains(p)
}

// ---------------------------------------------------------------------------
// GlobalAlloc
// ---------------------------------------------------------------------------

/// A `#[global_allocator]`-compatible handle to rimalloc.
#[derive(Clone, Copy, Default, Debug)]
pub struct Rimalloc;

// SAFETY: rimalloc is a conforming allocator: blocks stay valid until freed,
// Layout size/align contracts are respected via the aligned paths.
unsafe impl GlobalAlloc for Rimalloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `align <= MAX_ALIGN_SIZE` is NOT sufficient: the 8-byte size class
        // is only 8-aligned, so a `{size: 1..=8, align: 16}` layout needs the
        // aligned path. `is_naturally_aligned` is C's exact predicate.
        if is_naturally_aligned(layout.size(), layout.align()) {
            malloc(layout.size())
        } else {
            malloc_aligned(layout.size(), layout.align())
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        free(ptr)
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if is_naturally_aligned(layout.size(), layout.align()) {
            zalloc(layout.size())
        } else {
            zalloc_aligned(layout.size(), layout.align())
        }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if is_naturally_aligned(new_size, layout.align()) {
            realloc(ptr, new_size)
        } else {
            realloc_aligned(ptr, new_size, layout.align())
        }
    }
}

// ---------------------------------------------------------------------------
// POSIX-style API (port of `alloc-posix.c`)
// ---------------------------------------------------------------------------

/// `mi_posix_memalign`: returns 0, or EINVAL/ENOMEM like posix_memalign.
pub fn posix_memalign(out: &mut *mut u8, alignment: usize, size: usize) -> i32 {
    if !alignment.is_multiple_of(size_of::<usize>()) || !is_power_of_two(alignment) {
        return crate::EINVAL;
    }
    let p = malloc_zero_aligned_at(heap_default_unchecked(), size, alignment, 0, false);
    if p.is_null() {
        return crate::ENOMEM;
    }
    *out = p;
    0
}

/// `mi_memalign`
pub fn memalign(alignment: usize, size: usize) -> *mut u8 {
    malloc_aligned(size, alignment)
}

/// `mi_valloc`
pub fn valloc(size: usize) -> *mut u8 {
    malloc_aligned(size, crate::os::page_size())
}

/// `mi_reallocarray`
pub fn reallocarray(p: *mut u8, count: usize, size: usize) -> *mut u8 {
    match count_size(count, size) {
        Some(total) => realloc(p, total),
        None => ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// Usable-size returning variants (`mi_umalloc` family)
// ---------------------------------------------------------------------------

/// `mi_umalloc`
pub fn umalloc(size: usize, usable: &mut usize) -> *mut u8 {
    let p = malloc(size);
    *usable = usable_size(p);
    p
}

/// `mi_urealloc`
pub fn urealloc(p: *mut u8, newsize: usize, pre: &mut usize, post: &mut usize) -> *mut u8 {
    *pre = usable_size(p);
    let q = realloc(p, newsize);
    *post = usable_size(q);
    q
}

/// `mi_ufree`
pub fn ufree(p: *mut u8, usable: &mut usize) {
    *usable = usable_size(p);
    free(p);
}

// ---------------------------------------------------------------------------
// Heap introspection
// ---------------------------------------------------------------------------

/// `mi_heap_contains_block`
///
/// # Safety
/// `p` must be null or a block allocated by this allocator.
pub unsafe fn heap_contains_block(heap: &Heap, p: *const u8) -> bool {
    if p.is_null() {
        return false;
    }
    // SAFETY: p must be a block allocated by us (API contract).
    let page = unsafe { crate::segment::ptr_page(p) };
    page.heap() == ptr::from_ref(heap).cast_mut()
}

/// `mi_heap_check_owned`
///
/// # Safety
/// `p` must be null or a pointer into memory obtained from this allocator.
pub unsafe fn heap_check_owned(_heap: &Heap, p: *const u8) -> bool {
    // SAFETY: per contract.
    unsafe { check_owned(p) }
}

/// `mi_zalloc_small`
#[inline]
pub fn zalloc_small(size: usize) -> *mut u8 {
    debug_assert!(size <= SMALL_SIZE_MAX);
    as_ptr(crate::alloc::heap_malloc_small_zero(
        heap_default_unchecked(),
        size,
        true,
    ))
}

/// Current committed-byte count from the stats (diagnostic).
pub fn stats_committed_current() -> i64 {
    crate::stats::STATS
        .committed
        .current
        .load(core::sync::atomic::Ordering::Relaxed)
}

/// Syscall counters `[mmap, munmap, madvise, mprotect]` (diagnostics).
pub fn os_syscall_counts() -> [usize; 4] {
    crate::os::syscall_counts()
}

/// Diagnostics: (abandoned segments, reclaim tries, reclaim hits).
pub fn abandoned_stats() -> (usize, usize, usize) {
    use core::sync::atomic::Ordering;
    (
        crate::segment::abandoned::count(),
        crate::segment::abandoned::RECLAIM_TRIES.load(Ordering::Relaxed),
        crate::segment::abandoned::RECLAIM_HITS.load(Ordering::Relaxed),
    )
}

/// Diagnostics: number of initialized live threads.
pub fn current_thread_count_dbg() -> usize {
    crate::init::current_thread_count()
}

/// `mi_stats_print`: write allocator statistics to stderr.
pub fn stats_print() {
    let _ = crate::stats::print(&mut std::io::stderr());
}

/// Write allocator statistics to an arbitrary writer.
pub fn stats_write(out: &mut dyn std::io::Write) -> std::io::Result<()> {
    crate::stats::print(out)
}

pub use crate::hooks::{register_deferred_free, register_error, register_output};
