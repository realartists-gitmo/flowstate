//! Tiny spinlock used by the global caches and lists. Contention is rare
//! (segment-level events only) and critical sections are a few pointer
//! writes, so spinning beats an OS mutex here.

use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};

#[inline]
pub(crate) fn spin_acquire(lock: &AtomicBool) {
    while lock
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        spin_loop();
    }
}

#[inline]
pub(crate) fn spin_release(lock: &AtomicBool) {
    lock.store(false, Ordering::Release);
}

/// Run `f` under `lock`.
#[inline]
pub(crate) fn spin_locked<R>(lock: &AtomicBool, f: impl FnOnce() -> R) -> R {
    spin_acquire(lock);
    let r = f();
    spin_release(lock);
    r
}
