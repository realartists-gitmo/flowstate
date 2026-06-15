//! Regression tests for GlobalAlloc alignment and realloc edge cases found
//! by adversarial differential testing against C mimalloc.

use std::alloc::{GlobalAlloc, Layout};

/// The 8-byte size class is only 8-aligned, so a `{size <= 8, align: 16}`
/// layout must route through the aligned allocator (C's
/// `mi_malloc_is_naturally_aligned`).
#[test]
fn global_alloc_honors_align16_for_tiny_sizes() {
    let a = rimalloc::Rimalloc;
    for size in 1..=16usize {
        for &align in &[8usize, 16, 32, 64] {
            let layout = Layout::from_size_align(size, align).unwrap();
            // Miri interprets every allocation; a few iterations exercise the
            // same routing the native run hammers 256x.
            let iters = if cfg!(miri) { 4 } else { 256 };
            for _ in 0..iters {
                let p = unsafe { a.alloc(layout) };
                assert!(!p.is_null());
                assert_eq!(p.addr() % align, 0, "size {size} align {align}: {:p}", p);
                let z = unsafe { a.alloc_zeroed(layout) };
                assert_eq!(z.addr() % align, 0);
                assert_eq!(unsafe { *z }, 0);
                unsafe {
                    a.dealloc(p, layout);
                    a.dealloc(z, layout);
                }
            }
        }
    }
}

/// `realloc` preserving a large alignment must still honor it, and a big
/// shrink must release the oversized block rather than retain it forever.
#[test]
fn realloc_aligned_shrinks_and_keeps_alignment() {
    let p = rimalloc::malloc_aligned(8 << 20, 256);
    assert_eq!(p.addr() % 256, 0);
    let q = rimalloc::realloc_aligned(p, 64, 256);
    assert_eq!(q.addr() % 256, 0);
    assert!(
        rimalloc::usable_size(q) < 4096,
        "aligned shrink retained the oversized block: {}",
        rimalloc::usable_size(q)
    );
    rimalloc::free(q);
}

/// `realloc(p, 0)` returns a non-null minimal block whose first byte is
/// zeroed (C issue #725), and frees the original.
#[test]
fn realloc_to_zero_is_zeroed() {
    let p = rimalloc::malloc(128);
    unsafe { p.write_bytes(0xAB, 128) };
    let q = rimalloc::realloc(p, 0);
    assert!(!q.is_null());
    assert_eq!(unsafe { *q }, 0);
    rimalloc::free(q);
}
