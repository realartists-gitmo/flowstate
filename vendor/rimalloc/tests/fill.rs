//! Port of mimalloc `test/test-api-fill.c` (the zero-init half; the
//! `MI_DEBUG>=2` fill-pattern half tests debug fill values we do not
//! implement, matching a release-mode mimalloc build).

use rimalloc as mi;

const SMALL_SIZE_MAX: usize = 128 * size_of::<usize>();
const MAX_ALIGN_SIZE: usize = 16;

fn check_zero_init(p: *const u8, size: usize) -> bool {
    !p.is_null() && (0..size).all(|i| unsafe { *p.add(i) } == 0)
}

#[test]
fn zeroinit_zalloc_small() {
    let size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc(size);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_zalloc_large() {
    let size = SMALL_SIZE_MAX * 2;
    let p = mi::zalloc(size);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_zalloc_small_fn() {
    let size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc_small(size);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_calloc_small() {
    let size = SMALL_SIZE_MAX / 2;
    let p = mi::calloc(size, 1);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_calloc_large() {
    let size = SMALL_SIZE_MAX * 2;
    let p = mi::calloc(size, 1);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_rezalloc_small() {
    let mut size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc(size);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::rezalloc(p, size);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_rezalloc_large() {
    let mut size = SMALL_SIZE_MAX * 2;
    let p = mi::zalloc(size);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::rezalloc(p, size);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_recalloc_small() {
    let mut size = SMALL_SIZE_MAX / 2;
    let p = mi::calloc(size, 1);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::recalloc(p, size, 1);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_recalloc_large() {
    let mut size = SMALL_SIZE_MAX * 2;
    let p = mi::calloc(size, 1);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::recalloc(p, size, 1);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_zalloc_aligned_small() {
    let size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc_aligned(size, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_zalloc_aligned_large() {
    let size = SMALL_SIZE_MAX * 2;
    let p = mi::zalloc_aligned(size, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_calloc_aligned_small() {
    let size = SMALL_SIZE_MAX / 2;
    let p = mi::calloc_aligned(size, 1, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_calloc_aligned_large() {
    let size = SMALL_SIZE_MAX * 2;
    let p = mi::calloc_aligned(size, 1, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_rezalloc_aligned_small() {
    let mut size = SMALL_SIZE_MAX / 2;
    let p = mi::zalloc_aligned(size, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::rezalloc_aligned(p, size, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_rezalloc_aligned_large() {
    let mut size = SMALL_SIZE_MAX * 2;
    let p = mi::zalloc_aligned(size, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::rezalloc_aligned(p, size, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_recalloc_aligned_small() {
    let mut size = SMALL_SIZE_MAX / 2;
    let p = mi::calloc_aligned(size, 1, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::recalloc_aligned(p, size, 1, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}

#[test]
fn zeroinit_recalloc_aligned_large() {
    let mut size = SMALL_SIZE_MAX * 2;
    let p = mi::calloc_aligned(size, 1, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    size *= 3;
    let p = mi::recalloc_aligned(p, size, 1, MAX_ALIGN_SIZE * 2);
    assert!(check_zero_init(p, size));
    mi::free(p);
}
