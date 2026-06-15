//! `MIMALLOC_TARGET_SEGMENTS_PER_THREAD` makes a thread shed segments past
//! its target so other threads can reclaim them (C's
//! `mi_segments_try_abandon` / `_mi_page_force_abandon`). Own-process test:
//! the option latches on first allocator use.

use std::sync::mpsc;

#[test]
fn target_segments_sheds_and_reuses() {
    // SAFETY: single test in this binary; no concurrent env access.
    unsafe {
        std::env::set_var("MIMALLOC_TARGET_SEGMENTS_PER_THREAD", "2");
        std::env::set_var("MIMALLOC_PURGE_DELAY", "0");
    }

    // Build a working set large enough to need several segments, keeping
    // pages full (so they land in the full bin, the abandon candidate set),
    // then keep allocating: the target trigger must abandon excess segments
    // rather than growing unboundedly. We assert it stays alive and the
    // freed memory round-trips through abandonment.
    // Miri interprets every allocation, so scale the working set down there
    // (the abandon/reclaim code paths still run, just over fewer blocks).
    let (n, keep, threads_n) = if cfg!(miri) {
        (200usize, 180usize, 400usize)
    } else {
        (4000, 3900, 8000)
    };
    let mut live: Vec<*mut u8> = Vec::new();
    for round in 0..8 {
        for _ in 0..n {
            let p = rimalloc::malloc(4096);
            assert!(!p.is_null());
            unsafe { p.write_bytes(round as u8, 64) };
            live.push(p);
        }
        // Free most, retain a few per round to keep churn realistic.
        for p in live.drain(..keep) {
            rimalloc::free(p);
        }
        rimalloc::collect(false);
    }
    for p in live.drain(..) {
        rimalloc::free(p);
    }

    // A second thread must be able to reclaim what the first abandoned.
    let (tx, rx) = mpsc::channel::<usize>();
    let h = std::thread::spawn(move || {
        let mut ps: Vec<*mut u8> = (0..threads_n).map(|_| rimalloc::malloc(4096)).collect();
        for &p in &ps {
            assert!(!p.is_null());
        }
        tx.send(ps.len()).unwrap();
        for p in ps.drain(..) {
            rimalloc::free(p);
        }
    });
    assert_eq!(rx.recv().unwrap(), threads_n);
    h.join().unwrap();
    rimalloc::collect(true);
}
