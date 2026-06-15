//! Loom model-check of the cross-thread delayed-free protocol.
//!
//! This drives the *actual implementation* — `rimalloc::protocol::{mt_push,
//! collect_take, try_use_delayed, delayed_partial}`, the same generic
//! functions production `free.rs`/`page_ops.rs` execute — instantiated
//! with loom atomics, so every CAS, ordering, and flag transition of the
//! real code is explored across all interleavings. Only block *storage*
//! (next links, the owner's counters) is test-side, mirroring how
//! production supplies raw page memory through the same closures.
//!
//! Checked: every remotely freed block lands on the owner's local list
//! exactly once (none lost, none duplicated), `used` reaches exactly zero,
//! and FREEING never leaks.
//!
//! Run: RUSTFLAGS="--cfg loom" cargo test --test loom_protocol --release
#![cfg(loom)]

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::Ordering;
use loom::thread;
use rimalloc::protocol::{
    AtomicWord, FREEING, NO_DELAYED, TF_COUNT_SHIFT, TF_PTR_MASK, USE_DELAYED, collect_take,
    delayed_partial, mt_push, try_use_delayed,
};

/// Loom-instrumented atomic word for the protocol's generic interface.
struct LoomWord(loom::sync::atomic::AtomicUsize);
impl AtomicWord for LoomWord {
    fn load(&self, o: core::sync::atomic::Ordering) -> usize {
        self.0.load(o)
    }
    fn store(&self, v: usize, o: core::sync::atomic::Ordering) {
        self.0.store(v, o)
    }
    fn swap(&self, v: usize, o: core::sync::atomic::Ordering) -> usize {
        self.0.swap(v, o)
    }
    fn compare_exchange_weak(
        &self,
        cur: usize,
        new: usize,
        ok: core::sync::atomic::Ordering,
        err: core::sync::atomic::Ordering,
    ) -> Result<usize, usize> {
        self.0.compare_exchange_weak(cur, new, ok, err)
    }
}

/// Test-side storage standing in for raw page memory: block addresses are
/// `4 * (index + 1)` so flag bits never collide and 0 stays null.
struct Storage {
    next: Vec<UnsafeCell<usize>>,
    xthread: LoomWord,
    delayed: LoomWord,
    used: UnsafeCell<usize>,
    local: UnsafeCell<Vec<usize>>,
}
unsafe impl Sync for Storage {}
unsafe impl Send for Storage {}

const fn addr(i: usize) -> usize {
    4 * (i + 1)
}
fn index(a: usize) -> usize {
    a / 4 - 1
}

impl Storage {
    fn new(n: usize, flag: usize) -> Storage {
        Storage {
            next: (0..n).map(|_| UnsafeCell::new(0)).collect(),
            xthread: LoomWord(loom::sync::atomic::AtomicUsize::new(flag)),
            delayed: LoomWord(loom::sync::atomic::AtomicUsize::new(0)),
            used: UnsafeCell::new(n),
            local: UnsafeCell::new(Vec::new()),
        }
    }

    /// `free_block_delayed_mt` = the real `mt_push` + raw storage closures.
    fn remote_free(&self, i: usize) {
        mt_push(
            &self.xthread,
            || Some(&self.delayed),
            addr(i),
            |next| self.next[i].with_mut(|p| unsafe { *p = next }),
        );
    }

    /// `page_thread_free_collect` = the real `collect_take` + list walk.
    /// The walk must agree with the count packed in the taken word.
    fn collect(&self) {
        let taken = collect_take(&self.xthread);
        let mut n = taken & TF_PTR_MASK;
        let mut count = 0usize;
        while n != 0 {
            count += 1;
            let next = self.next[index(n)].with(|p| unsafe { *p });
            self.local.with_mut(|l| unsafe { (*l).push(n) });
            n = next;
        }
        assert_eq!(taken >> TF_COUNT_SHIFT, count, "packed count wrong");
        self.used.with_mut(|u| unsafe {
            assert!(*u >= count, "used underflow in collect");
            *u -= count;
        });
    }

    /// `free_delayed_block` = the real `try_use_delayed` + collect +
    /// local bookkeeping.
    fn free_delayed_block(&self, a: usize) -> bool {
        if !try_use_delayed(&self.xthread, USE_DELAYED, false, thread::yield_now) {
            return false;
        }
        self.collect();
        self.local.with_mut(|l| unsafe { (*l).push(a) });
        self.used.with_mut(|u| unsafe {
            assert!(*u >= 1, "used underflow (delayed)");
            *u -= 1;
        });
        true
    }

    /// `heap_delayed_free_partial` = the real `delayed_partial`.
    fn drain_delayed(&self) -> bool {
        delayed_partial(
            &self.delayed,
            |a| self.next[index(a)].with(|p| unsafe { *p }),
            |a, next| self.next[index(a)].with_mut(|p| unsafe { *p = next }),
            |a| self.free_delayed_block(a),
        )
    }

    fn assert_quiescent(&self, n: usize) {
        for _ in 0..4 {
            self.drain_delayed();
            self.collect();
        }
        assert_eq!(
            self.delayed.load(Ordering::Relaxed),
            0,
            "delayed not drained"
        );
        let x = self.xthread.load(Ordering::Relaxed);
        assert_ne!(x & 3, FREEING, "FREEING leaked");
        assert_eq!(x & !3, 0, "page list not drained");
        self.used
            .with(|u| unsafe { assert_eq!(*u, 0, "used not zero") });
        self.local.with(|l| unsafe {
            let mut v = (*l).clone();
            v.sort();
            assert_eq!(
                v,
                (0..n).map(addr).collect::<Vec<_>>(),
                "blocks lost or duplicated"
            );
        });
    }
}

/// Full page (USE_DELAYED): the first remote free must take the FREEING
/// handshake and wake the owner via the delayed list; nothing may be lost
/// between the two lists.
#[test]
fn full_page_wakeup() {
    loom::model(|| {
        let s = Arc::new(Storage::new(2, USE_DELAYED));
        let r1 = {
            let s = s.clone();
            thread::spawn(move || s.remote_free(0))
        };
        let r2 = {
            let s = s.clone();
            thread::spawn(move || s.remote_free(1))
        };
        let owner = {
            let s = s.clone();
            thread::spawn(move || {
                s.drain_delayed();
            })
        };
        r1.join().unwrap();
        r2.join().unwrap();
        owner.join().unwrap();
        s.assert_quiescent(2);
    });
}

/// Normal page (NO_DELAYED): remote frees race the owner's collect on the
/// page-local thread-free list.
#[test]
fn page_list_race() {
    loom::model(|| {
        let s = Arc::new(Storage::new(2, NO_DELAYED));
        let r1 = {
            let s = s.clone();
            thread::spawn(move || s.remote_free(0))
        };
        let r2 = {
            let s = s.clone();
            thread::spawn(move || s.remote_free(1))
        };
        let owner = {
            let s = s.clone();
            thread::spawn(move || {
                s.collect();
                s.drain_delayed();
            })
        };
        r1.join().unwrap();
        r2.join().unwrap();
        owner.join().unwrap();
        s.assert_quiescent(2);
    });
}
