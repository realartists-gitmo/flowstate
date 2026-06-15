//! Tests for the `secure` feature (encoded free lists, double-free detection).
#![cfg(feature = "secure")]

use rimalloc as mi;

#[test]
fn alloc_free_roundtrip_encoded() {
    // basic correctness with encoding active across many size classes
    let mut ptrs = Vec::new();
    for i in 0..2000usize {
        let size = (i % 1500) + 1;
        let p = mi::malloc(size);
        assert!(!p.is_null());
        unsafe { std::ptr::write_bytes(p, 0x7e, size) };
        ptrs.push((p, size));
    }
    for (p, size) in ptrs {
        unsafe {
            for j in 0..size {
                assert_eq!(*p.add(j), 0x7e);
            }
        }
        mi::free(p);
    }
}

#[test]
fn double_free_detected() {
    // A double free must be detected (the block's first word decodes to a
    // valid free-list link) and ignored rather than corrupting the list.
    let p = mi::malloc(64);
    let q = mi::malloc(64); // keep the page alive and non-empty
    mi::free(p);
    mi::free(p); // double free: caught by the secure check
    // the page free list stays sound: we can keep allocating
    let r = mi::malloc(64);
    assert!(!r.is_null());
    let s = mi::malloc(64);
    assert!(!s.is_null());
    assert_ne!(r, s);
    mi::free(q);
    mi::free(r);
    mi::free(s);
}
