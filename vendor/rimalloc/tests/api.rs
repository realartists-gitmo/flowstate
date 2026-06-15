//! Faithful port of mimalloc `test/test-api.c` (C++-only STL-allocator and
//! `realpath` cases are exercised through Rust equivalents or skipped).

use rimalloc as mi;

const SMALL_SIZE_MAX: usize = 128 * size_of::<usize>();
const MAX_ALIGN_SIZE: usize = 16;
const BLOCK_ALIGNMENT_MAX: usize = 32 * 1024 * 1024 / 2; // MI_SEGMENT_SIZE/2
const MIB: usize = 1024 * 1024;

fn mem_is_zero(p: *const u8, size: usize) -> bool {
    if p.is_null() {
        return false;
    }
    (0..size).all(|i| unsafe { *p.add(i) } == 0)
}

#[test]
fn malloc_aligned9a() {
    // test large alignments
    let p = mi::zalloc_aligned(1024 * 1024, 2);
    mi::free(p);
    let p = mi::zalloc_aligned(1024 * 1024, 2);
    mi::free(p);
}

#[test]
fn malloc_zero() {
    let p = mi::malloc(0);
    assert!(!p.is_null());
    mi::free(p);
}

#[test]
fn malloc_nomem1() {
    assert!(mi::malloc(isize::MAX as usize + 1).is_null());
}

#[test]
fn malloc_null() {
    mi::free(std::ptr::null_mut());
}

#[test]
fn calloc_overflow() {
    assert!(mi::calloc(&raw const COOKIE as usize, usize::MAX / 1000).is_null());
}
static COOKIE: u8 = 0;

#[test]
fn calloc0() {
    let p = mi::calloc(0, 1000);
    assert!(mi::usable_size(p) <= 16);
    mi::free(p);
}

#[test]
fn malloc_large() {
    // see PR #544
    let p = mi::malloc(67108872);
    mi::free(p);
}

#[test]
fn posix_memalign1() {
    let mut p: *mut u8 = std::ptr::null_mut();
    let err = mi::posix_memalign(&mut p, size_of::<usize>(), 32);
    assert!(err == 0 && p.addr() % size_of::<usize>() == 0);
    mi::free(p);
}

#[test]
fn posix_memalign_no_align() {
    let mut p: *mut u8 = std::ptr::dangling_mut();
    let err = mi::posix_memalign(&mut p, 3, 32);
    assert!(err == mi::EINVAL && p == std::ptr::dangling_mut());
}

#[test]
fn posix_memalign_zero() {
    let mut p: *mut u8 = std::ptr::null_mut();
    let err = mi::posix_memalign(&mut p, size_of::<usize>(), 0);
    mi::free(p);
    assert_eq!(err, 0);
}

#[test]
fn posix_memalign_nopow2() {
    let mut p: *mut u8 = std::ptr::dangling_mut();
    let err = mi::posix_memalign(&mut p, 3 * size_of::<usize>(), 32);
    assert!(err == mi::EINVAL && p == std::ptr::dangling_mut());
}

#[test]
fn posix_memalign_nomem() {
    let mut p: *mut u8 = std::ptr::dangling_mut();
    let err = mi::posix_memalign(&mut p, size_of::<usize>(), usize::MAX);
    assert!(err == mi::ENOMEM && p == std::ptr::dangling_mut());
}

#[test]
fn malloc_aligned1() {
    let p = mi::malloc_aligned(32, 32);
    assert!(!p.is_null() && p.addr() % 32 == 0);
    mi::free(p);
}

#[test]
fn malloc_aligned2() {
    let p = mi::malloc_aligned(48, 32);
    assert!(!p.is_null() && p.addr() % 32 == 0);
    mi::free(p);
}

#[test]
fn malloc_aligned3() {
    let p1 = mi::malloc_aligned(48, 32);
    assert!(!p1.is_null() && p1.addr() % 32 == 0);
    let p2 = mi::malloc_aligned(48, 32);
    assert!(!p2.is_null() && p2.addr() % 32 == 0);
    mi::free(p2);
    mi::free(p1);
}

#[test]
fn malloc_aligned4() {
    for _ in 0..8 {
        let p = mi::malloc_aligned(8, 16);
        assert!(!p.is_null() && p.addr() % 16 == 0);
        mi::free(p);
    }
}

#[test]
fn malloc_aligned5() {
    let p = mi::malloc_aligned(4097, 4096);
    let usable = mi::usable_size(p);
    assert!(usable >= 4097 && usable < 16000, "usable: {usable}");
    mi::free(p);
}

#[test]
fn malloc_aligned7() {
    let p = mi::malloc_aligned(1024, BLOCK_ALIGNMENT_MAX);
    assert_eq!(p.addr() % BLOCK_ALIGNMENT_MAX, 0);
    mi::free(p);
}

#[test]
fn malloc_aligned8() {
    for i in 0..5 {
        let n = 1usize << i;
        let p = mi::malloc_aligned(1024, n * BLOCK_ALIGNMENT_MAX);
        assert_eq!(p.addr() % (n * BLOCK_ALIGNMENT_MAX), 0, "n={n}");
        mi::free(p);
    }
}

#[test]
fn malloc_aligned9() {
    // test large alignments
    let sizes: [usize; 8] = [
        8,
        512,
        1024 * 1024,
        BLOCK_ALIGNMENT_MAX,
        BLOCK_ALIGNMENT_MAX + 1,
        2 * BLOCK_ALIGNMENT_MAX,
        8 * BLOCK_ALIGNMENT_MAX,
        0,
    ];
    let step = if cfg!(miri) { 5 } else { 1 };
    for i in (0..28).step_by(step) {
        let align = 1usize << i;
        let mut p = [std::ptr::null_mut(); 8];
        for (j, &size) in sizes.iter().enumerate() {
            p[j] = mi::zalloc_aligned(size, align);
            assert_eq!(p[j].addr() % align, 0, "align={align} size={size}");
        }
        for q in p {
            mi::free(q);
        }
    }
}

#[test]
fn malloc_aligned10() {
    let mut p = [std::ptr::null_mut(); 11];
    let mut align = 1;
    for j in 0..=10 {
        p[j] = mi::malloc_aligned(43 + align, align);
        assert_eq!(p[j].addr() % align, 0, "align={align}");
        align *= 2;
    }
    for q in p.into_iter().rev() {
        mi::free(q);
    }
}

#[test]
fn malloc_aligned11() {
    let heap = mi::heap_new().unwrap();
    let p = mi::heap_malloc_aligned(heap, 33554426, 8);
    assert!(unsafe { mi::heap_contains_block(heap, p) });
    mi::heap_destroy(heap);
}

#[test]
fn malloc_aligned12() {
    let p = mi::malloc_aligned(0x100, 0x100);
    assert_eq!(p.addr() % 0x100, 0); // #602
    mi::free(p);
}

#[test]
fn malloc_aligned13() {
    // Under Miri sample the size domain (the interpreter is ~1000x slower);
    // natively the full domain is covered.
    let step = if cfg!(miri) { 41 } else { 1 };
    let mut size = 1usize;
    while size <= SMALL_SIZE_MAX * 2 {
        let mut align = 1usize;
        while align <= size {
            let mut p = [std::ptr::null_mut(); 10];
            for slot in &mut p {
                *slot = mi::malloc_aligned(size, align);
                assert!(
                    !slot.is_null() && slot.addr() % align == 0,
                    "size={size} align={align}"
                );
            }
            for q in p {
                mi::free(q);
            }
            align *= 2;
        }
        size += step;
    }
}

#[test]
fn malloc_aligned_at1() {
    let p = mi::malloc_aligned_at(48, 32, 0);
    assert!(!p.is_null() && p.addr() % 32 == 0);
    mi::free(p);
}

#[test]
fn malloc_aligned_at2() {
    let p = mi::malloc_aligned_at(50, 32, 8);
    assert!(!p.is_null() && (p.addr() + 8) % 32 == 0);
    mi::free(p);
}

#[test]
fn memalign1() {
    for _ in 0..8 {
        let p = mi::memalign(16, 8);
        assert!(!p.is_null() && p.addr() % 16 == 0);
        mi::free(p);
    }
}

#[test]
fn zalloc_aligned_small1() {
    let zalloc_size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc_aligned(zalloc_size, MAX_ALIGN_SIZE * 2);
    assert!(mem_is_zero(p, zalloc_size));
    mi::free(p);
}

#[test]
fn rezalloc_aligned_small1() {
    let mut zalloc_size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc_aligned(zalloc_size, MAX_ALIGN_SIZE * 2);
    assert!(mem_is_zero(p, zalloc_size));
    zalloc_size *= 3;
    let p = mi::rezalloc_aligned(p, zalloc_size, MAX_ALIGN_SIZE * 2);
    assert!(mem_is_zero(p, zalloc_size));
    mi::free(p);
}

#[test]
fn realloc_null() {
    let p = mi::realloc(std::ptr::null_mut(), 4);
    assert!(!p.is_null());
    mi::free(p);
}

#[test]
fn realloc_null_sizezero() {
    // "If ptr is NULL, the behavior is the same as calling malloc(new_size)."
    let p = mi::realloc(std::ptr::null_mut(), 0);
    assert!(!p.is_null());
    mi::free(p);
}

#[test]
fn realloc_sizezero() {
    let p = mi::malloc(4);
    let q = mi::realloc(p, 0);
    assert!(!q.is_null());
    mi::free(q);
}

#[test]
fn reallocarray_null_sizezero() {
    let p = mi::reallocarray(std::ptr::null_mut(), 0, 16); // issue #574
    assert!(!p.is_null());
    mi::free(p);
}

#[test]
fn umalloc1() {
    let limit = if cfg!(miri) { MIB } else { 32 * MIB };
    let mut size = 1usize;
    while size <= limit {
        let mut bsize = 0;
        let p = mi::umalloc(size, &mut bsize);
        assert!(bsize >= size);
        let (mut pre, mut post) = (0, 0);
        let p = mi::urealloc(p, size + 1024, &mut pre, &mut post);
        assert_eq!(pre, bsize);
        assert!(post >= size + 1024);
        let mut fsize = 0;
        mi::ufree(p, &mut fsize);
        assert_eq!(fsize, post);
        size *= 2;
    }
}

#[test]
fn heap_destroy_test() {
    // test_heap1
    let heap = mi::heap_new().unwrap();
    let p1 = mi::heap_malloc(heap, size_of::<i32>()).cast::<i32>();
    let p2 = mi::heap_malloc(heap, size_of::<i32>()).cast::<i32>();
    unsafe {
        *p1 = 43;
        *p2 = 43;
    }
    mi::heap_destroy(heap);
}

#[test]
fn heap_delete_test() {
    // test_heap2
    let heap = mi::heap_new().unwrap();
    let p1 = mi::heap_malloc(heap, size_of::<i32>()).cast::<i32>();
    let p2 = mi::heap_malloc(heap, size_of::<i32>()).cast::<i32>();
    mi::heap_delete(heap);
    unsafe { *p1 = 42 };
    mi::free(p1.cast());
    mi::free(p2.cast());
}

#[test]
fn rust_allocator_vec() {
    // stand-in for the C++ STL allocator tests
    let mut sizes = vec![];
    for i in 0..64usize {
        let p = mi::malloc(i * 17 + 1);
        sizes.push((p, i * 17 + 1));
    }
    for (p, _) in sizes {
        mi::free(p);
    }
}

#[test]
fn heap_visit_blocks_walk() {
    // mirror of test-stress's HEAP_WALK: sum visited block sizes and count
    // blocks; all live allocations must be visited exactly once.
    let heap = mi::heap_new().unwrap();
    let sizes = [1usize, 8, 16, 100, 700, 8000, 70_000, 200_000];
    let ptrs: Vec<*mut u8> = sizes.iter().map(|&s| mi::heap_malloc(heap, s)).collect();
    // free one so a partially-used page is walked via the bitmap path
    mi::free(ptrs[1]);

    let mut count = 0usize;
    let mut total = 0usize;
    let complete = mi::heap_visit_blocks(heap, true, &mut |area, block| {
        assert!(area.committed <= area.reserved);
        if let Some((p, size)) = block {
            assert!(!p.is_null() && size > 0);
            count += 1;
            total += size;
        }
        true
    });
    assert!(complete);
    assert_eq!(count, sizes.len() - 1);
    assert!(total >= sizes.iter().sum::<usize>() - sizes[1]);

    // early stop works
    let mut seen = 0;
    let complete = mi::heap_visit_blocks(heap, true, &mut |_, block| {
        if block.is_some() {
            seen += 1;
            return seen < 2;
        }
        true
    });
    assert!(!complete && seen == 2);

    for (i, p) in ptrs.iter().enumerate() {
        if i != 1 {
            mi::free(*p);
        }
    }
    mi::heap_destroy(heap);
}
