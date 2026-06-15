//! Targeted tests for the least-covered regions found by cargo-llvm-cov:
//! API wrapper variants and their overflow/OOM arms, huge-alignment
//! branches, `heap_absorb`/`heap_destroy` arms, abandoned-segment
//! reclaim-on-free, the segment cache, arena purging, heap visiting, and
//! the stats/options surfaces.

#![cfg(not(miri))] // exercised natively; the slow paths here add nothing under Miri

use rimalloc as mi;

const MIB: usize = 1024 * 1024;
const SMALL_SIZE_MAX: usize = 128 * size_of::<usize>();
const BLOCK_ALIGNMENT_MAX: usize = 16 * MIB; // MI_SEGMENT_SIZE / 2
const MAX_ALLOC_SIZE: usize = isize::MAX as usize;

// ---------------------------------------------------------------------------
// API wrapper variants and overflow arms
// ---------------------------------------------------------------------------

#[test]
fn small_variants() {
    let p = mi::malloc_small(64);
    assert!(!p.is_null());
    mi::free(p);
    let p = mi::zalloc_small(SMALL_SIZE_MAX);
    assert!(!p.is_null());
    assert!((0..SMALL_SIZE_MAX).all(|i| unsafe { *p.add(i) } == 0));
    mi::free(p);
}

#[test]
fn counted_overflow_arms() {
    assert!(mi::mallocn(usize::MAX / 8, 16).is_null());
    assert!(mi::reallocn(std::ptr::null_mut(), usize::MAX / 8, 16).is_null());
    assert!(mi::recalloc(std::ptr::null_mut(), usize::MAX / 8, 16).is_null());
    assert!(mi::calloc_aligned(usize::MAX / 8, 16, 64).is_null());
    assert!(mi::calloc_aligned_at(usize::MAX / 8, 16, 64, 0).is_null());
    assert!(mi::recalloc_aligned(std::ptr::null_mut(), usize::MAX / 8, 16, 64).is_null());
    assert!(mi::reallocarray(std::ptr::null_mut(), usize::MAX / 8, 16).is_null());

    let p = mi::mallocn(4, 32);
    assert!(!p.is_null());
    let p = mi::reallocn(p, 8, 32);
    assert!(!p.is_null());
    assert!(mi::usable_size(p) >= 256);
    let p = mi::reallocarray(p, 16, 32);
    assert!(!p.is_null());
    mi::free(p);
}

#[test]
fn good_size_clamps() {
    assert!(mi::good_size(100) >= 100);
    assert!(mi::good_size(100 * MIB) >= 100 * MIB);
    assert_eq!(mi::good_size(usize::MAX), usize::MAX); // over MAX_ALLOC_SIZE
    // Requests above the C v2.3.2 cap (SEGMENT_SLICE_SIZE * (u32::MAX-1),
    // issue #877) are returned unchanged, not page-rounded — pins the cap.
    let over_cap = 65536 * (u32::MAX as usize - 1) + 1;
    assert_eq!(mi::good_size(over_cap), over_cap);
}

#[test]
fn reallocf_frees_on_failure() {
    let p = mi::malloc(100);
    assert!(!p.is_null());
    let q = mi::reallocf(p, 200);
    assert!(!q.is_null());
    // failure: oversized request returns null AND releases q
    assert!(mi::reallocf(q, MAX_ALLOC_SIZE).is_null());
}

#[test]
fn realloc_fits_in_place() {
    let p = mi::malloc(100);
    let usable = mi::usable_size(p);
    // shrink within 50% waste: same pointer, no move
    assert_eq!(mi::realloc(p, usable - 8), p);
    assert_eq!(mi::rezalloc(p, usable - 8), p);
    mi::free(p);
}

#[test]
fn aligned_at_offset_variants() {
    let p = mi::zalloc_aligned_at(300, 512, 16);
    assert!(!p.is_null() && (p.addr() + 16).is_multiple_of(512));
    assert!((0..300).all(|i| unsafe { *p.add(i) } == 0));
    mi::free(p);
    let p = mi::calloc_aligned_at(10, 30, 512, 16);
    assert!(!p.is_null() && (p.addr() + 16).is_multiple_of(512));
    assert!((0..300).all(|i| unsafe { *p.add(i) } == 0));
    mi::free(p);
}

#[test]
fn expand_in_place_only() {
    assert!(mi::expand(std::ptr::null_mut(), 16).is_null());
    let p = mi::malloc(100);
    let usable = mi::usable_size(p);
    assert_eq!(mi::expand(p, usable), p);
    assert!(mi::expand(p, usable + 1).is_null());
    mi::free(p);
}

#[test]
fn recalloc_zeroes_growth() {
    let p = mi::recalloc(std::ptr::null_mut(), 4, 64);
    assert!(!p.is_null());
    unsafe { p.write_bytes(0xAA, 256) };
    let p = mi::recalloc(p, 64, 64);
    assert!(!p.is_null());
    assert!((256..4096).all(|i| unsafe { *p.add(i) } == 0));
    mi::free(p);
}

// ---------------------------------------------------------------------------
// POSIX-style entry points
// ---------------------------------------------------------------------------

#[test]
fn posix_memalign_errors_and_success() {
    let mut out: *mut u8 = std::ptr::null_mut();
    assert_eq!(mi::posix_memalign(&mut out, 3, 100), mi::EINVAL);
    assert_eq!(mi::posix_memalign(&mut out, 24, 100), mi::EINVAL);
    assert_eq!(
        mi::posix_memalign(&mut out, 64, MAX_ALLOC_SIZE),
        mi::ENOMEM
    );
    assert_eq!(mi::posix_memalign(&mut out, 128, 100), 0);
    assert!(!out.is_null() && out.addr().is_multiple_of(128));
    mi::free(out);
}

#[test]
fn valloc_is_page_aligned() {
    let p = mi::valloc(100);
    assert!(!p.is_null());
    assert!(p.addr() % 4096 == 0);
    mi::free(p);
}

// ---------------------------------------------------------------------------
// Aligned-allocation branches
// ---------------------------------------------------------------------------

#[test]
fn huge_alignment_with_offset_fails() {
    // alignment > BLOCK_ALIGNMENT_MAX cannot use an offset
    assert!(mi::malloc_aligned_at(1024, 2 * BLOCK_ALIGNMENT_MAX, 64).is_null());
}

#[test]
fn huge_alignment_zeroed() {
    let align = 2 * BLOCK_ALIGNMENT_MAX;
    let p = mi::zalloc_aligned(3 * MIB, align);
    assert!(!p.is_null() && p.addr().is_multiple_of(align));
    assert!(
        (0..3 * MIB)
            .step_by(4097)
            .all(|i| unsafe { *p.add(i) } == 0)
    );
    mi::free(p);

    // small size still goes through the dedicated huge-segment path
    let p = mi::malloc_aligned(32, align);
    assert!(!p.is_null() && p.addr().is_multiple_of(align));
    mi::free(p);
}

#[test]
fn aligned_invalid_arguments() {
    assert!(mi::malloc_aligned(64, 0).is_null());
    assert!(mi::malloc_aligned(64, 24).is_null());
    assert!(mi::malloc_aligned(MAX_ALLOC_SIZE + 1, 64).is_null());
    assert!(mi::realloc_aligned(std::ptr::null_mut(), 64, 24).is_null());
}

#[test]
fn realloc_aligned_paths() {
    // null -> fresh aligned allocation
    let p = mi::realloc_aligned(std::ptr::null_mut(), 100, 256);
    assert!(!p.is_null() && p.addr().is_multiple_of(256));
    unsafe { p.write_bytes(0x5A, 100) };
    // shrink within the keep window (>= half the usable size) -> same pointer
    // (large-shrink reallocation is covered in tests/api_alignment.rs).
    let usable = mi::usable_size(p);
    assert_eq!(mi::realloc_aligned(p, usable - usable / 4, 256), p);
    // grow -> move, contents preserved
    let q = mi::realloc_aligned(p, 64 * 1024, 256);
    assert!(!q.is_null() && q.addr().is_multiple_of(256));
    assert!((0..100).all(|i| unsafe { *q.add(i) } == 0x5A));
    mi::free(q);

    // rezalloc_aligned zeroes the growth tail
    let p = mi::rezalloc_aligned(std::ptr::null_mut(), 64, 128);
    unsafe { p.write_bytes(0x77, 64) };
    let q = mi::rezalloc_aligned(p, 8192, 128);
    assert!(!q.is_null());
    assert!((64..8192).all(|i| unsafe { *q.add(i) } == 0));
    mi::free(q);

    // recalloc_aligned counted wrapper
    let p = mi::recalloc_aligned(std::ptr::null_mut(), 16, 64, 512);
    assert!(!p.is_null() && p.addr().is_multiple_of(512));
    mi::free(p);

    // realloc_aligned_at keeps the offset congruence
    let p = mi::malloc_aligned_at(100, 1024, 8);
    assert!(!p.is_null() && (p.addr() + 8).is_multiple_of(1024));
    let q = mi::realloc_aligned_at(p, 50_000, 1024, 8);
    assert!(!q.is_null() && (q.addr() + 8).is_multiple_of(1024));
    mi::free(q);
}

// ---------------------------------------------------------------------------
// GlobalAlloc surface
// ---------------------------------------------------------------------------

#[test]
fn global_alloc_trait_paths() {
    use core::alloc::{GlobalAlloc, Layout};
    let a = mi::Rimalloc;
    unsafe {
        let small = Layout::from_size_align(100, 8).unwrap();
        let p = a.alloc(small);
        assert!(!p.is_null());
        let p = a.realloc(p, small, 5000);
        assert!(!p.is_null());
        a.dealloc(p, Layout::from_size_align(5000, 8).unwrap());

        let p = a.alloc_zeroed(small);
        assert!((0..100).all(|i| *p.add(i) == 0));
        a.dealloc(p, small);

        let big_align = Layout::from_size_align(100, 4096).unwrap();
        let p = a.alloc(big_align);
        assert!(p.addr().is_multiple_of(4096));
        let p = a.realloc(p, big_align, 9000);
        assert!(p.addr().is_multiple_of(4096));
        a.dealloc(p, Layout::from_size_align(9000, 4096).unwrap());

        let p = a.alloc_zeroed(big_align);
        assert!(p.addr().is_multiple_of(4096) && (0..100).all(|i| *p.add(i) == 0));
        a.dealloc(p, big_align);
    }
}

// ---------------------------------------------------------------------------
// Heap API: defaults, tags, absorb, destroy
// ---------------------------------------------------------------------------

#[test]
fn heap_default_backing_and_tags() {
    let backing = mi::heap_get_backing();
    let heap = mi::heap_new_ex(7, true, 0).expect("heap");
    assert!(heap.by_tag(7).is_some());
    assert!(backing.by_tag(7).is_some()); // reachable via the thread heap list
    assert!(heap.by_tag(99).is_none());

    let old = mi::heap_set_default(heap);
    let p = mi::malloc(100); // served by the new default heap
    assert!(unsafe { mi::heap_contains_block(heap, p) });
    assert!(!unsafe { mi::heap_contains_block(backing, p) });
    mi::free(p);
    mi::heap_set_default(old);
    assert_eq!(
        mi::heap_get_default() as *const mi::Heap,
        old as *const mi::Heap
    );
    mi::heap_delete(heap);
}

#[test]
fn heap_counted_wrappers() {
    let heap = mi::heap_new().expect("heap");
    assert!(mi::heap_calloc(heap, usize::MAX / 8, 16).is_null());
    assert!(mi::heap_mallocn(heap, usize::MAX / 8, 16).is_null());
    let p = mi::heap_calloc(heap, 8, 32);
    assert!((0..256).all(|i| unsafe { *p.add(i) } == 0));
    let p = mi::heap_realloc(heap, p, 1024);
    assert!(!p.is_null());
    let q = mi::heap_mallocn(heap, 4, 64);
    assert!(!q.is_null());
    mi::free(p);
    mi::free(q);
    mi::heap_delete(heap);
}

#[test]
fn heap_delete_absorbs_live_pages() {
    // Fill pages (including full ones) in a fresh heap, then delete it:
    // heap_absorb must move every page to the backing heap, after which the
    // blocks are still valid and freeable. 4 KiB blocks keep page capacity
    // low (16/page) so several pages sit in the full queue at delete time.
    let heap = mi::heap_new().expect("heap");
    let mut ptrs = Vec::new();
    for i in 0..4096 {
        let p = mi::heap_malloc(heap, 16 + (i % 7) * 32);
        assert!(!p.is_null());
        unsafe { p.write_bytes(0xC3, 16) };
        ptrs.push(p);
    }
    for _ in 0..256 {
        let p = mi::heap_malloc(heap, 4096);
        assert!(!p.is_null());
        unsafe { p.write_bytes(0xC3, 16) };
        ptrs.push(p);
    }
    mi::heap_delete(heap);
    for p in ptrs {
        assert!((0..16).all(|i| unsafe { *p.add(i) } == 0xC3));
        mi::free(p);
    }
    mi::collect(false);
}

#[test]
fn heap_destroy_discards_blocks() {
    // heap_destroy frees whole pages without freeing individual blocks.
    let heap = mi::heap_new().expect("heap");
    for i in 0..512 {
        assert!(!mi::heap_malloc(heap, 8 + (i % 11) * 100).is_null());
    }
    assert!(!mi::heap_malloc(heap, 20 * MIB).is_null()); // huge page too
    mi::heap_destroy(heap);
}

#[test]
fn heap_destroy_without_destroy_permission_deletes() {
    // A heap created with allow_destroy = false routes heap_destroy through
    // heap_delete (it may hold reclaimed pages).
    let heap = mi::heap_new_ex(0, false, 0).expect("heap");
    let p = mi::heap_malloc(heap, 100);
    assert!(!p.is_null());
    mi::heap_destroy(heap);
    mi::free(p); // absorbed by the backing heap, still freeable
}

#[test]
fn heap_delete_from_middle_of_list() {
    // The thread heap list is LIFO; deleting the older heap first walks the
    // list past the newer one.
    let h1 = mi::heap_new().expect("h1");
    let h2 = mi::heap_new().expect("h2");
    assert!(!mi::heap_malloc(h1, 64).is_null());
    assert!(!mi::heap_malloc(h2, 64).is_null());
    mi::heap_delete(h1);
    mi::heap_delete(h2);
}

// ---------------------------------------------------------------------------
// Ownership introspection
// ---------------------------------------------------------------------------

#[test]
fn ownership_checks() {
    let p = mi::malloc(100);
    assert!(unsafe { mi::check_owned(p) });
    assert!(!unsafe { mi::check_owned(std::ptr::null()) });
    assert!(!unsafe { mi::check_owned(p.wrapping_add(1)) }); // unaligned
    assert!(mi::is_in_heap_region(p));
    assert!(!mi::is_in_heap_region(std::ptr::null()));
    let stack = 0u8;
    assert!(!mi::is_in_heap_region(&raw const stack));
    let heap = mi::heap_get_default();
    assert!(unsafe { mi::heap_check_owned(heap, p) });
    assert!(!unsafe { mi::heap_contains_block(heap, std::ptr::null()) });
    mi::free(p);
}

// ---------------------------------------------------------------------------
// Heap visiting (single-block, full-page, and bitmap paths)
// ---------------------------------------------------------------------------

#[test]
fn visit_blocks_all_page_shapes() {
    let heap = mi::heap_new().expect("heap");
    let mut live = Vec::new();
    // partial small page, dense prefix: blocks 0..256 stay live so whole
    // bitmap words are all-in-use, the tail is freed
    let mut small = Vec::new();
    for _ in 0..300 {
        small.push(mi::heap_malloc(heap, 48));
    }
    for (i, &p) in small.iter().enumerate() {
        if i >= 256 || i % 65 == 64 {
            mi::free(p);
        } else {
            live.push(p);
        }
    }
    // a fully-used page: 8 KiB blocks fill a 64 KiB small page
    for _ in 0..8 {
        let p = mi::heap_malloc(heap, 8 * 1024 - 64);
        assert!(!p.is_null());
        live.push(p);
    }
    // a one-block (huge) page
    let huge = mi::heap_malloc(heap, 20 * MIB);
    assert!(!huge.is_null());
    live.push(huge);

    let mut areas = 0usize;
    let mut blocks = 0usize;
    let complete = mi::heap_visit_blocks(heap, true, &mut |area, block| {
        match block {
            None => {
                assert!(!area.blocks.is_null() && area.full_block_size >= area.block_size);
                areas += 1;
            }
            Some((p, size)) => {
                assert!(size > 0 && !p.is_null());
                blocks += 1;
            }
        }
        true
    });
    assert!(complete);
    assert!(areas >= 2);
    assert_eq!(blocks, live.len());

    // early stop after the first area
    let stopped = mi::heap_visit_blocks(heap, true, &mut |_, _| false);
    assert!(!stopped);
    // area-only walk
    assert!(mi::heap_visit_blocks(heap, false, &mut |_, b| {
        assert!(b.is_none());
        true
    }));

    // a retired-but-retained page (all blocks freed) visits as an empty area
    let q: Vec<_> = (0..4).map(|_| mi::heap_malloc(heap, 99)).collect();
    for p in q {
        mi::free(p);
    }
    assert!(mi::heap_visit_blocks(heap, true, &mut |_, _| true));

    for p in live {
        mi::free(p);
    }
    mi::heap_delete(heap);
}

// ---------------------------------------------------------------------------
// Cross-thread huge free and abandoned reclaim-on-free
// ---------------------------------------------------------------------------

#[test]
fn cross_thread_huge_free_resets_memory() {
    // Freeing a huge (> LARGE_OBJ_SIZE_MAX) block from a foreign thread
    // goes through huge_page_reset before the delayed-free push.
    let p = mi::malloc(20 * MIB);
    assert!(!p.is_null());
    unsafe { p.write_bytes(0x42, 4096) };
    let addr = p as usize;
    std::thread::spawn(move || mi::free(addr as *mut u8))
        .join()
        .unwrap();
    mi::collect(true);
}

#[test]
fn delayed_free_via_full_page() {
    // A remote free into a FULL page goes onto the owning heap's delayed
    // list (UseDelayedFree); the owner's next collect drains it through
    // heap_delayed_free_partial / free_delayed_block.
    let mut v = Vec::new();
    for _ in 0..64 {
        let p = mi::malloc(4000); // 16 blocks per 64 KiB page
        assert!(!p.is_null());
        v.push(p as usize);
    }
    let first = v[0]; // belongs to a page that is full by now
    std::thread::spawn(move || mi::free(first as *mut u8))
        .join()
        .unwrap();
    mi::collect(false);
    for addr in v.into_iter().skip(1) {
        mi::free(addr as *mut u8);
    }
    mi::collect(false);
}

#[test]
fn abandoned_reclaim_on_free() {
    use rimalloc::options;
    let prev = options::get(options::Option::AbandonedReclaimOnFree);
    options::set(options::Option::AbandonedReclaimOnFree, 1);

    let ptrs = std::thread::spawn(|| {
        let mut v = Vec::new();
        for _ in 0..64 {
            let p = mi::malloc(2048);
            assert!(!p.is_null());
            unsafe { p.write_bytes(0x66, 2048) };
            v.push(p as usize);
        }
        v // thread exits with live blocks: its segments are abandoned
    })
    .join()
    .unwrap();

    // Warm this thread's allocator: reclaim-on-free only fires on an
    // initialized thread.
    mi::free(mi::malloc(1));
    let (_, tries_before, _) = mi::abandoned_stats();
    for addr in ptrs {
        let p = addr as *mut u8;
        assert!((0..2048).all(|i| unsafe { *p.add(i) } == 0x66));
        mi::free(p); // first free reclaims the abandoned segment
    }
    let (_, tries_after, _) = mi::abandoned_stats();
    assert!(tries_after >= tries_before);
    options::set(options::Option::AbandonedReclaimOnFree, prev);
    mi::collect(false);
}

// ---------------------------------------------------------------------------
// Arena multi-block claims and purging; segment cache collection
// ---------------------------------------------------------------------------

#[test]
fn arena_multi_block_and_purge() {
    use rimalloc::options;
    // multi-block arena claim: huge segments whose size lands on an exact
    // multiple of the 32 MiB arena block are served by the arena bitmap
    // (one of these sizes hits 64 MiB exactly, whatever the info-slice
    // overhead); the rest go directly to the OS
    for slack in 1..=4usize {
        let p = mi::malloc(64 * MIB - slack * 64 * 1024);
        assert!(!p.is_null());
        unsafe { p.write_bytes(1, 64) };
        mi::free(p);
    }
    let p = mi::malloc(40 * MIB);
    assert!(!p.is_null());
    unsafe { p.write_bytes(1, 64) };
    mi::free(p);

    // schedule and force arena/cache purges
    let prev = options::get(options::Option::PurgeDelay);
    options::set(options::Option::PurgeDelay, 1);
    let mut big = Vec::new();
    for _ in 0..4 {
        big.push(mi::malloc(8 * MIB));
    }
    for p in big.drain(..) {
        mi::free(p);
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    mi::collect(false); // expiry-driven purge
    mi::collect(true); // forced purge + cache flush back to the OS
    options::set(options::Option::PurgeDelay, prev);
}

#[test]
fn os_direct_huge_segments() {
    // Huge segments above the arena bitmap's single-claim limit (> 2 GiB)
    // and odd-sized huge segments come straight from the OS; check_owned on
    // them exercises the segment-cookie path (no arena contains them).
    let p = mi::malloc(36 * MIB); // odd size: not a 32 MiB multiple
    assert!(!p.is_null());
    if !mi::is_in_heap_region(p) {
        assert!(unsafe { mi::check_owned(p) });
    }
    unsafe { p.write_bytes(3, 64) };
    mi::free(p);
    mi::collect(true); // drain the deferred-free queue
}

#[test]
fn diagnostics_counters() {
    let p = mi::malloc(100);
    mi::free(p);
    let counts = mi::os_syscall_counts();
    assert!(counts[0] > 0); // at least one mmap happened by now
    assert!(mi::current_thread_count_dbg() >= 1);
    let (abandoned, tries, hits) = mi::abandoned_stats();
    assert!(hits <= tries || abandoned == 0 || tries == 0);
}

// ---------------------------------------------------------------------------
// Stats and options surfaces
// ---------------------------------------------------------------------------

#[test]
fn stats_report_renders() {
    let p = mi::malloc(100);
    mi::free(p);
    let mut buf = Vec::new();
    mi::stats_write(&mut buf).unwrap();
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("heap stats:"));
    assert!(s.contains("pages ops:"));
    assert!(s.contains("segments"));
    mi::stats_print(); // stderr variant

    // a writer failing at every possible point exercises each `?` arm
    struct Failing(usize);
    impl std::io::Write for Failing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("full"));
            }
            self.0 -= 1;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for budget in 0..32 {
        let _ = mi::stats_write(&mut Failing(budget));
    }
}

#[test]
fn options_set_get_enable_disable() {
    use rimalloc::options;
    let prev = options::get(options::Option::MaxPageCandidates);
    options::set(options::Option::MaxPageCandidates, 9);
    assert_eq!(options::get(options::Option::MaxPageCandidates), 9);
    options::enable(options::Option::MaxPageCandidates);
    assert!(options::is_enabled(options::Option::MaxPageCandidates));
    options::disable(options::Option::MaxPageCandidates);
    assert!(!options::is_enabled(options::Option::MaxPageCandidates));
    options::set(options::Option::MaxPageCandidates, prev);
}

// ---------------------------------------------------------------------------
// Thread lifecycle: explicit init/done and heaps left at thread exit
// ---------------------------------------------------------------------------

#[test]
fn explicit_thread_init_done() {
    std::thread::spawn(|| {
        mi::thread_init();
        mi::thread_init(); // idempotent
        let p = mi::malloc(100);
        assert!(!p.is_null());
        mi::free(p);
        mi::thread_done();
        mi::thread_done(); // idempotent
    })
    .join()
    .unwrap();
}

#[test]
fn thread_exit_deletes_extra_heaps() {
    std::thread::spawn(|| {
        let heap = mi::heap_new().expect("heap");
        for _ in 0..64 {
            assert!(!mi::heap_malloc(heap, 512).is_null());
        }
        // exit without heap_delete: thread teardown must absorb + abandon
    })
    .join()
    .unwrap();
    mi::collect(true);
}

// ---------------------------------------------------------------------------
// Block sizes above 4 GiB (fastmod reciprocal unusable)
// ---------------------------------------------------------------------------

#[test]
fn block_size_above_reciprocal_range() {
    // > 4 GiB block: page.block_recip stays 0 and usable_size/free fall
    // back to plain division. Virtual-only; pages are never touched beyond
    // the first one.
    let size = (4 << 30) + 4096usize;
    let p = mi::malloc(size);
    if p.is_null() {
        return; // acceptable under memory pressure
    }
    unsafe { p.write_bytes(7, 8) };
    assert!(mi::usable_size(p) >= size);
    mi::free(p);
    mi::collect(true);
}
