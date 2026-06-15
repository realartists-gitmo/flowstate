//! Exercises the OS segment cache, which is only reachable when arena
//! reservation is disabled (`MIMALLOC_ARENA_RESERVE=0`). Lives in its own
//! test binary: options latch on first read, so the env must be set
//! before the first allocator call in this process.

#[test]
fn segment_cache_serves_reuse_without_arenas() {
    // SAFETY: single test in this binary; no concurrent env access.
    unsafe {
        std::env::set_var("MIMALLOC_ARENA_RESERVE", "0");
        std::env::set_var("MIMALLOC_PURGE_DELAY", "0");
    }

    // Wave 1: force several segments into existence, then free everything
    // so the segments retire into the OS cache.
    let mut ptrs: Vec<*mut u8> = (0..64)
        .map(|i| rimalloc::malloc(64 * 1024 + i * 64))
        .collect();
    for &p in &ptrs {
        assert!(!p.is_null());
        unsafe { p.write_bytes(0xAB, 64) };
    }
    for p in ptrs.drain(..) {
        rimalloc::free(p);
    }
    rimalloc::collect(true);
    let [mmap1, ..] = rimalloc::os_syscall_counts();

    // Wave 2: the same demand must be served from the cache, not mmap.
    let ptrs: Vec<*mut u8> = (0..64)
        .map(|i| rimalloc::malloc(64 * 1024 + i * 64))
        .collect();
    for &p in &ptrs {
        assert!(!p.is_null());
    }
    let [mmap2, ..] = rimalloc::os_syscall_counts();
    assert!(
        mmap2 <= mmap1 + 2,
        "cache miss storm: {mmap1} mmaps before wave 2, {mmap2} after"
    );
    for p in ptrs {
        rimalloc::free(p);
    }

    // Expire and purge the cached segments (purge_delay=0 → immediate).
    rimalloc::collect(true);

    // Huge allocations bypass the cache (non-standard size) but must still
    // work without arenas.
    let huge = rimalloc::malloc(40 << 20);
    assert!(!huge.is_null());
    unsafe { huge.write_bytes(1, 4096) };
    rimalloc::free(huge);
}
