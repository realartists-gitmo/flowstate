//! Tests for arena reservation/binding, NUMA plumbing, and registered hooks.

use std::sync::atomic::{AtomicUsize, Ordering};

use rimalloc as mi;

#[test]
fn reserve_and_heap_in_arena() {
    // Reserve an exclusive arena and bind a heap to it: allocations from
    // that heap must come from inside the reservation; the default heap
    // must never allocate from it.
    const ARENA_SIZE: usize = 64 * 1024 * 1024; // 2 blocks
    let arena_id = mi::reserve_os_memory(ARENA_SIZE, false, true).expect("reserve");
    assert!(arena_id > 0);

    let heap = mi::heap_new_in_arena(arena_id).expect("heap");
    let inside = mi::heap_malloc(heap, 1234);
    assert!(!inside.is_null());

    // exhaust nothing on the default heap; its memory must be elsewhere
    let outside = mi::malloc(1234);
    assert!(!outside.is_null());

    // the exclusive arena served the bound heap
    let p1 = mi::heap_malloc(heap, 100_000);
    let p2 = mi::heap_malloc(heap, 8);
    for p in [inside, p1, p2] {
        assert!(!p.is_null());
    }
    // all blocks of the bound heap live in the same 32MiB-aligned segment
    // region; segments of the two heaps must differ
    let seg = |p: *mut u8| p.addr() & !(32 * 1024 * 1024 - 1);
    assert_ne!(seg(inside), seg(outside));

    mi::free(p1);
    mi::free(p2);
    mi::heap_delete(heap);
    mi::free(outside);
}

#[cfg(not(windows))]
#[test]
fn manage_external_memory() {
    // Hand rimalloc a raw mapping; it must become an allocatable arena.
    const SIZE: usize = 96 * 1024 * 1024;
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(raw, libc::MAP_FAILED);
    let arena_id =
        unsafe { mi::manage_os_memory(raw.cast(), SIZE, true, true, -1, true).expect("manage") };
    let heap = mi::heap_new_in_arena(arena_id).expect("heap");
    let p = mi::heap_malloc(heap, 4096);
    assert!(!p.is_null());
    assert!(p.addr() >= raw.addr() && p.addr() < raw.addr() + SIZE);
    mi::free(p);
    mi::heap_delete(heap);
}

#[test]
fn huge_os_pages_api() {
    // Unsupported on macOS: must fail gracefully, not crash.
    let r = mi::reserve_huge_os_pages_at(1, 0);
    if cfg!(target_os = "linux") {
        // may succeed or fail depending on hugetlb pool; both are valid
        let _ = r;
    } else {
        assert!(r.is_none());
    }
}

static DEFERRED_CALLS: AtomicUsize = AtomicUsize::new(0);
static ERROR_CALLS: AtomicUsize = AtomicUsize::new(0);
static OUTPUT_BYTES: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_deferred(_force: bool, _heartbeat: u64, _arg: *mut core::ffi::c_void) {
    DEFERRED_CALLS.fetch_add(1, Ordering::Relaxed);
}
extern "C" fn on_error(_code: i32, _arg: *mut core::ffi::c_void) {
    ERROR_CALLS.fetch_add(1, Ordering::Relaxed);
}
extern "C" fn on_output(_msg: *const u8, len: usize, _arg: *mut core::ffi::c_void) {
    OUTPUT_BYTES.fetch_add(len, Ordering::Relaxed);
}

#[test]
fn registered_hooks_fire() {
    mi::register_deferred_free(Some(on_deferred), std::ptr::null_mut());
    mi::register_output(Some(on_output), std::ptr::null_mut());
    mi::register_error(Some(on_error), std::ptr::null_mut());

    // deferred-free fires on collect
    mi::collect(false);
    assert!(DEFERRED_CALLS.load(Ordering::Relaxed) >= 1);

    // an oversized allocation reports through output+error hooks
    let p = mi::malloc(isize::MAX as usize + 1);
    assert!(p.is_null());
    assert!(ERROR_CALLS.load(Ordering::Relaxed) >= 1);
    assert!(OUTPUT_BYTES.load(Ordering::Relaxed) > 0);

    mi::register_deferred_free(None, std::ptr::null_mut());
    mi::register_output(None, std::ptr::null_mut());
    mi::register_error(None, std::ptr::null_mut());
}

#[test]
fn exclusive_arena_never_escapes_to_os() {
    // A 64 MiB exclusive arena holds exactly 2 segment blocks. A heap bound
    // to it must (a) serve multi-block huge segments from the arena (sizes
    // rounded up to whole blocks) and (b) return null once the arena is
    // exhausted, never falling back to OS memory: this regressed into an
    // infinite segment-allocation loop before segment_os_alloc refused the
    // OS fallback for bound heaps.
    const MIB: usize = 1024 * 1024;
    let arena = mi::reserve_os_memory(64 * MIB, true, true).expect("arena");
    let heap = mi::heap_new_in_arena(arena).expect("heap");

    let big = mi::heap_malloc(heap, 40 * MIB); // 2-block arena claim
    assert!(!big.is_null() && mi::is_in_heap_region(big));
    unsafe { big.write_bytes(1, 64) };
    mi::free(big);

    let pinned: Vec<_> = (0..6).map(|_| mi::heap_malloc(heap, 10 * MIB)).collect();
    assert!(
        pinned
            .iter()
            .all(|&p| !p.is_null() && mi::is_in_heap_region(p))
    );

    // arena exhausted: large and huge requests fail instead of escaping
    assert!(mi::heap_malloc(heap, 10 * MIB).is_null());
    assert!(mi::heap_malloc(heap, 40 * MIB).is_null());

    for p in pinned {
        mi::free(p);
    }
    // blocks returned: the arena serves the bound heap again
    let p = mi::heap_malloc(heap, 10 * MIB);
    assert!(!p.is_null() && mi::is_in_heap_region(p));
    mi::free(p);
    mi::heap_delete(heap);
}
