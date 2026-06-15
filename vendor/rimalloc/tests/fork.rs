//! fork() safety: the child of a multithreaded allocating process must be
//! able to keep allocating (locks are held across fork and released in the
//! child; another thread's mid-critical-section state cannot leak in).
//! Miri cannot emulate `fork`, so this target is skipped under Miri.
#![cfg(all(unix, not(miri)))]

use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn fork_while_allocating() {
    // hammer the allocator (and its locks) from background threads
    static STOP: AtomicBool = AtomicBool::new(false);
    let workers: Vec<_> = (0..4)
        .map(|t| {
            std::thread::spawn(move || {
                let mut keep = Vec::new();
                while !STOP.load(Ordering::Relaxed) {
                    let p = rimalloc::malloc((t + 1) * 137);
                    keep.push(p);
                    if keep.len() > 64 {
                        for q in keep.drain(..) {
                            rimalloc::free(q);
                        }
                    }
                }
                for q in keep {
                    rimalloc::free(q);
                }
            })
        })
        .collect();

    for _ in 0..20 {
        // SAFETY: child only uses the allocator and _exit (async-signal
        // considerations handled by our atfork hooks).
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // child: allocate/free across many size classes, then exit
            let mut ptrs = Vec::new();
            for i in 0..2000usize {
                let p = rimalloc::malloc((i % 3000) + 1);
                if p.is_null() {
                    unsafe { libc::_exit(2) };
                }
                unsafe { p.write_bytes(0x5c, (i % 3000) + 1) };
                ptrs.push(p);
            }
            for p in ptrs {
                rimalloc::free(p);
            }
            let big = rimalloc::malloc(50 << 20); // force fresh segment work
            if big.is_null() {
                unsafe { libc::_exit(3) };
            }
            rimalloc::free(big);
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        // SAFETY: valid pid from fork above.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child failed: status={status:#x}"
        );
    }

    STOP.store(true, Ordering::Relaxed);
    for w in workers {
        w.join().unwrap();
    }
}
