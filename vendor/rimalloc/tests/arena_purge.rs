//! A freed span inside a *live* arena-backed segment must be returned to the
//! OS in-life (C purges every segment's freed spans via mi_segment_span_free,
//! regardless of backing — only whole-segment teardown is left to the arena).
//! Skipping this for arena-backed segments leaks resident memory until the
//! whole segment is freed (a ~2x RSS regression in decommit mode).
//!
//! Deterministic signal: in decommit mode an in-life purge calls os::decommit,
//! which remaps PROT_NONE via mmap (syscall counter index 0). A pin keeps the
//! segment alive so the free cannot trigger whole-segment teardown instead.

use rimalloc::options::{self, Option};

#[test]
fn arena_segment_frees_span_in_life() {
    // Decommit mode, immediate purge (no delay), so the free itself purges.
    options::set(Option::PurgeDecommits, 1);
    options::set(Option::PurgeDelay, 0);

    // Pin a (32MiB, arena-backed) segment so it stays live across the free.
    let pin = rimalloc::malloc(1024);
    assert!(!pin.is_null());

    // A large span (< LARGE_OBJ_SIZE_MAX = 16MiB) lands as a large page in the
    // same segment. Touch it so the pages are resident, then free it.
    let span = 12 << 20;
    let big = rimalloc::malloc(span);
    assert!(!big.is_null());
    unsafe { big.write_bytes(0xA5, span) };

    let decommits_before = rimalloc::os_syscall_counts()[0];
    rimalloc::free(big);
    let decommits_after = rimalloc::os_syscall_counts()[0];

    assert!(
        decommits_after > decommits_before,
        "freed span of a live arena-backed segment was not decommitted in-life \
         (RSS regression): decommit syscalls {decommits_before} -> {decommits_after}"
    );

    rimalloc::free(pin);
}
