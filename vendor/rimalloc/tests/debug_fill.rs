//! Port of the `MI_DEBUG>=2` half of mimalloc `test/test-api-fill.c`:
//! fresh blocks carry the 0xD0 uninit pattern, freed blocks 0xDF.
#![cfg(feature = "debug-fill")]

use rimalloc as mi;

fn check_fill(p: *const u8, size: usize, byte: u8) -> bool {
    (0..size).all(|i| unsafe { *p.add(i) } == byte)
}

#[test]
fn uninit_malloc_small_and_large() {
    for size in [8usize, 128, 1024, 16 * 1024] {
        let p = mi::malloc(size);
        assert!(check_fill(p, size, 0xD0), "uninit fill, size {size}");
        mi::free(p);
    }
}

#[test]
fn fill_freed_small() {
    let size = 256usize;
    let p = mi::malloc(size);
    unsafe { std::ptr::write_bytes(p, 0xab, size) };
    mi::free(p);
    // skip the first word: it now holds the free-list link
    assert!(check_fill(unsafe { p.add(8) }, size - 8, 0xDF));
}

#[test]
fn zalloc_still_zero() {
    let p = mi::zalloc(512);
    assert!(check_fill(p, 512, 0));
    mi::free(p);
}
