//! Process and thread initialization (port of `init.c`): the thread-local
//! default heap, lazy thread setup, and thread-exit cleanup via a pthread
//! key destructor (Unix) or FLS callback (Windows).

use core::cell::Cell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::heap::{Heap, Tld};
use crate::os::OsMem;

/// Unique, cheap thread id. `pthread_self` rather than a thread-local's
/// address: macOS tears down and re-instantiates `#[thread_local]` storage
/// before pthread TSD destructors run, so a TLS address changes identity
/// mid-teardown and exit-time frees would take the cross-thread path
/// (leaking blocks onto the dying heap's delayed list).
///
/// On Apple arm64 this reads pthread TSD slot 0 (the pthread self pointer)
/// straight off the thread register, like C mimalloc's
/// `_mi_prim_thread_id`; it stays valid through TLS teardown.
#[cfg(all(target_vendor = "apple", target_arch = "aarch64", not(miri)))]
#[inline(always)]
pub fn thread_id() -> usize {
    let tcb: *const usize;
    // SAFETY: reads the read-only thread register; no memory effects.
    unsafe {
        core::arch::asm!(
            "mrs {tcb}, tpidrro_el0",
            "bic {tcb}, {tcb}, #7",
            tcb = out(reg) tcb,
            options(nomem, nostack, pure, preserves_flags)
        );
    }
    // SAFETY: slot 0 of the TSD block is `pthread_self`, always readable.
    unsafe { *tcb }
}

#[cfg(all(not(all(target_vendor = "apple", target_arch = "aarch64", not(miri))), not(windows)))]
#[inline(always)]
pub fn thread_id() -> usize {
    let t = tls::TID.get();
    if t != 0 {
        return t;
    }
    // SAFETY: always callable; on macOS this is a thread-register read.
    let t = unsafe { libc::pthread_self() as usize };
    tls::TID.set(t);
    t
}

#[cfg(windows)]
#[inline(always)]
pub fn thread_id() -> usize {
    let t = tls::TID.get();
    if t != 0 {
        return t;
    }
    // SAFETY: always callable.
    let t = unsafe { GetCurrentThreadId() as usize };
    tls::TID.set(t);
    t
}

// The static empty heap: the initial default for every thread, so the
// malloc fast path needs no init check (its pages have no free blocks, so
// allocation falls into `malloc_generic` which initializes the thread).
struct StaticHeap(Heap);
// SAFETY: read-only use across threads (its lists stay empty), except for
// the main-thread heap which is owned by the main thread.
unsafe impl Sync for StaticHeap {}

static EMPTY_HEAP: StaticHeap = StaticHeap(heap_empty_const(
    ptr::from_ref(&crate::heap::EMPTY_PAGE).cast_mut(),
));

const fn heap_empty_const(empty_page: *mut crate::page::Page) -> Heap {
    use crate::bins::BIN_WSIZE;
    use crate::constants::{BIN_FULL, PAGES_DIRECT, PTR_SIZE};
    use crate::heap::PageQueue;
    let mut pages = [const {
        PageQueue {
            first: Cell::new(ptr::null_mut()),
            last: Cell::new(ptr::null_mut()),
            block_size: 0,
        }
    }; BIN_FULL + 1];
    let mut i = 0;
    while i <= BIN_FULL {
        pages[i].block_size = BIN_WSIZE[i] * PTR_SIZE;
        i += 1;
    }
    Heap {
        tld: Cell::new(ptr::null_mut()),
        thread_delayed_free: core::sync::atomic::AtomicUsize::new(0),
        thread_id: Cell::new(0),
        cookie: Cell::new(0),
        keys: [Cell::new(0), Cell::new(0)],
        page_count: Cell::new(0),
        page_retired_min: Cell::new(BIN_FULL),
        page_retired_max: Cell::new(0),
        pages_full_size: Cell::new(0),
        generic_count: Cell::new(0),
        generic_collect_count: Cell::new(0),
        next: Cell::new(ptr::null_mut()),
        no_reclaim: Cell::new(false),
        tag: Cell::new(0),
        arena_id: Cell::new(0),
        // SAFETY: Cell<T> is repr(transparent) over T.
        pages_free_direct: unsafe {
            core::mem::transmute::<
                [*mut crate::page::Page; PAGES_DIRECT],
                [Cell<*mut crate::page::Page>; PAGES_DIRECT],
            >([empty_page; PAGES_DIRECT])
        },
        pages,
    }
}

#[inline(always)]
fn empty_heap() -> *mut Heap {
    ptr::from_ref(&EMPTY_HEAP.0).cast_mut()
}

/// Direct pthread TSD slot for the default heap on Apple arm64: the same
/// unused slot C mimalloc claims (`MI_TLS_SLOT` 89, `__PTK_FRAMEWORK_OLDGC_KEY9`).
/// Mach-O `thread_local!`/`#[thread_local]` accesses go through a
/// `_tlv_get_addr` call on every malloc/free; this is a 3-instruction load
/// off the read-only thread register instead, and (unlike TLV storage) it
/// stays valid through TLS teardown at thread exit.
#[cfg(all(target_vendor = "apple", target_arch = "aarch64", not(miri)))]
mod tsd {
    use super::*;

    const HEAP_SLOT: usize = 89;

    #[inline(always)]
    fn tsd_base() -> *mut usize {
        let tcb: *mut usize;
        // SAFETY: reads the read-only thread register; no memory effects.
        unsafe {
            core::arch::asm!(
                "mrs {tcb}, tpidrro_el0",
                "bic {tcb}, {tcb}, #7",
                tcb = out(reg) tcb,
                options(nomem, nostack, pure, preserves_flags)
            );
        }
        tcb
    }

    /// Relaxed atomics rather than plain loads/stores: the slot is
    /// logically thread-private, but it lives inside the pthread struct,
    /// which libpthread recycles across threads at the same address (reuse
    /// is ordered inside libpthread, invisibly to TSan). The dying thread's
    /// final store (`thread_done` via the TSD destructor) would otherwise
    /// appear to race the first read of a later thread occupying the
    /// recycled struct. Same single ldr/str on arm64; zero cost.
    pub struct HeapSlot;
    impl HeapSlot {
        #[inline(always)]
        pub fn get(&self) -> *mut Heap {
            // SAFETY: slot 89 of this thread's TSD block is reserved for the
            // allocator (as in C mimalloc on Apple); it is zero on fresh
            // threads, which maps to the static empty heap.
            let slot = unsafe { AtomicUsize::from_ptr(tsd_base().add(HEAP_SLOT)) };
            let p = slot.load(Ordering::Relaxed) as *mut Heap;
            if p.is_null() { empty_heap() } else { p }
        }
        #[inline(always)]
        pub fn set(&self, v: *mut Heap) {
            // SAFETY: as in `get`; only this thread writes its own slot.
            let slot = unsafe { AtomicUsize::from_ptr(tsd_base().add(HEAP_SLOT)) };
            slot.store(v as usize, Ordering::Relaxed);
        }
    }
    pub static DEFAULT_HEAP: HeapSlot = HeapSlot;
}

/// The allocator's three thread-local slots. Stable Rust uses
/// const-initialized `thread_local!` (correct everywhere, ~1ns per access
/// through `LocalKey`); the opt-in `nightly` feature uses raw
/// `#[thread_local]` statics for direct TLS loads on the malloc/free fast
/// paths. On Apple arm64 the default-heap slot instead lives in a pthread
/// TSD slot (see `tsd` above).
#[cfg(not(feature = "nightly"))]
mod tls {
    use super::*;

    pub struct Slot<T: 'static>(&'static std::thread::LocalKey<Cell<T>>);
    impl<T: Copy> Slot<T> {
        #[inline(always)]
        pub fn get(&self) -> T {
            self.0.with(Cell::get)
        }
        #[inline(always)]
        pub fn set(&self, v: T) {
            self.0.with(|c| c.set(v))
        }
    }

    thread_local! {
        static TID_KEY: Cell<usize> = const { Cell::new(0) };
        static RNG_KEY: Cell<u64> = const { Cell::new(0) };
    }
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64", not(miri))))]
    thread_local! {
        static HEAP_KEY: Cell<*mut Heap> =
            const { Cell::new((&raw const EMPTY_HEAP.0).cast_mut()) };
    }
    #[cfg_attr(
        all(target_vendor = "apple", target_arch = "aarch64", not(miri)),
        allow(dead_code)
    )]
    pub static TID: Slot<usize> = Slot(&TID_KEY);
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64", not(miri))))]
    pub static DEFAULT_HEAP: Slot<*mut Heap> = Slot(&HEAP_KEY);
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64", not(miri)))]
    pub use super::tsd::DEFAULT_HEAP;
    pub static RNG: Slot<u64> = Slot(&RNG_KEY);
}

#[cfg(feature = "nightly")]
mod tls {
    use super::*;

    #[thread_local]
    static TID_RAW: Cell<usize> = Cell::new(0);
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64", not(miri))))]
    #[thread_local]
    static HEAP_RAW: Cell<*mut Heap> = Cell::new((&raw const EMPTY_HEAP.0).cast_mut());
    #[thread_local]
    static RNG_RAW: Cell<u64> = Cell::new(0);

    macro_rules! slot {
        ($name:ident, $ty:ty, $raw:ident, $slot:ident) => {
            pub struct $name;
            impl $name {
                #[inline(always)]
                pub fn get(&self) -> $ty {
                    $raw.get()
                }
                #[inline(always)]
                pub fn set(&self, v: $ty) {
                    $raw.set(v)
                }
            }
            pub static $slot: $name = $name;
        };
    }
    #[cfg_attr(
        all(target_vendor = "apple", target_arch = "aarch64", not(miri)),
        allow(dead_code)
    )]
    mod tid {
        use super::*;
        slot!(TidSlot, usize, TID_RAW, TID);
    }
    #[cfg_attr(
        all(target_vendor = "apple", target_arch = "aarch64", not(miri)),
        allow(unused_imports)
    )]
    pub use tid::TID;
    #[cfg(not(all(target_vendor = "apple", target_arch = "aarch64", not(miri))))]
    slot!(HeapSlot, *mut Heap, HEAP_RAW, DEFAULT_HEAP);
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64", not(miri)))]
    pub use super::tsd::DEFAULT_HEAP;
    slot!(RngSlot, u64, RNG_RAW, RNG);
}

#[inline(always)]
pub fn heap_default_unchecked() -> &'static Heap {
    // SAFETY: always the static empty heap or this thread's live heap.
    unsafe { &*tls::DEFAULT_HEAP.get() }
}

#[inline(always)]
pub fn thread_is_initialized() -> bool {
    tls::DEFAULT_HEAP.get() != empty_heap()
}

/// `mi_heap_get_default`: initializes the thread if needed.
#[inline]
pub fn heap_default() -> &'static Heap {
    if !thread_is_initialized() {
        thread_init();
    }
    heap_default_unchecked()
}

pub fn heap_set_default(heap: &Heap) -> &'static Heap {
    let old = heap_default();
    tls::DEFAULT_HEAP.set(ptr::from_ref(heap).cast_mut());
    old
}

// ---------------------------------------------------------------------------
// Thread metadata allocation (OS-backed so it does not recurse into malloc)
// ---------------------------------------------------------------------------

#[repr(C)]
struct ThreadData {
    heap: Heap, // must be first (cast in thread_done)
    tld: Tld,
    mem: OsMem,
}

static THREAD_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn current_thread_count() -> usize {
    THREAD_COUNT.load(Ordering::Relaxed).max(1)
}

// Simple wyrand-style RNG for cookies/keys (free lists are not encoded).
pub fn random_next() -> usize {
    let mut s = tls::RNG.get();
    if s == 0 {
        s = thread_id() as u64 ^ 0x9e37_79b9_7f4a_7c15 ^ (options_entropy() << 1) | 1;
    }
    s = s.wrapping_add(0xa076_1d64_78bd_642f);
    tls::RNG.set(s);
    let t = (s as u128).wrapping_mul((s ^ 0xe703_7ed1_a0b4_28db) as u128);
    ((t >> 64) ^ t) as usize
}

#[cfg(not(windows))]
fn options_entropy() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_nsec as u64
}

#[cfg(windows)]
fn options_entropy() -> u64 {
    let mut counter: i64 = 0;
    // SAFETY: valid out-pointer.
    unsafe { QueryPerformanceCounter(&mut counter) };
    counter as u64
}

// Cache of dead ThreadData regions (`mi_thread_data_cache`): thread churn
// must not mmap/munmap per thread. Nodes are written into the dead regions.
struct TdNode {
    next: *mut TdNode,
    mem: OsMem,
}
struct TdCache {
    lock: core::sync::atomic::AtomicBool,
    first: Cell<*mut TdNode>,
    count: AtomicUsize,
}
// SAFETY: `first` only accessed under `lock`.
unsafe impl Sync for TdCache {}
static TD_CACHE: TdCache = TdCache {
    lock: core::sync::atomic::AtomicBool::new(false),
    first: Cell::new(ptr::null_mut()),
    count: AtomicUsize::new(0),
};
const TD_CACHE_MAX: usize = 32;

fn td_locked<R>(f: impl FnOnce() -> R) -> R {
    crate::sync::spin_locked(&TD_CACHE.lock, f)
}

fn td_cache_pop() -> Option<(*mut u8, OsMem)> {
    if TD_CACHE.count.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let mem = td_locked(|| {
        // SAFETY: nodes are owned by the cache while linked (lock held).
        unsafe {
            let node = TD_CACHE.first.get().as_ref()?;
            TD_CACHE.first.set(node.next);
            Some(node.mem)
        }
    })?;
    TD_CACHE.count.fetch_sub(1, Ordering::Relaxed);
    Some((mem.base, mem))
}

fn td_cache_push(mem: OsMem) -> bool {
    if TD_CACHE.count.load(Ordering::Relaxed) >= TD_CACHE_MAX {
        return false;
    }
    // SAFETY: dead, fully-committed region; node lives at the tail, away
    // from the Heap header which protected references may still cover.
    let node = unsafe { crate::os::tail_node::<TdNode>(&mem) };
    td_locked(|| {
        // SAFETY: dead region owned by the cache.
        unsafe {
            node.write(TdNode {
                next: TD_CACHE.first.get(),
                mem,
            })
        };
        TD_CACHE.first.set(node);
    });
    TD_CACHE.count.fetch_add(1, Ordering::Relaxed);
    true
}

// ---------------------------------------------------------------------------
// Thread init / done
// ---------------------------------------------------------------------------

static THREAD_KEY: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(not(windows))]
pub(crate) fn td_cache_lock() -> &'static core::sync::atomic::AtomicBool {
    &TD_CACHE.lock
}

// ---------------------------------------------------------------------------
// Unix: pthread TSD thread-exit callback
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
/// Register the `pthread_atfork` handlers that hold every allocator lock
/// across `fork()`. This MUST run before any thread or fork exists, so it
/// lives in a load-time constructor (`register_fork_handlers`); doing it
/// lazily on first allocation lets a fork race a half-registered or
/// orphaned-lock state into the child. Split out so the lazy fallback (for
/// builds without a usable load-time constructor) can reuse it.
fn register_fork_handlers() {
    // SAFETY: a plain libc call with three valid handler fns; the handlers
    // only touch allocator spinlocks and never allocate.
    unsafe {
        libc::pthread_atfork(
            Some(crate::os::fork_prepare),
            Some(crate::os::fork_release),
            Some(crate::os::fork_release),
        );
    }
}

#[cfg(not(windows))]
unsafe extern "C" fn thread_done_callback(value: *mut libc::c_void) {
    // On macOS the dynamic loader tears down `#[thread_local]` storage
    // before pthread TSD destructors run, so the heap arrives as the key
    // value (mimalloc does the same in `prim/unix`).
    if !value.is_null() {
        // SAFETY: the value is this thread's backing heap, still live.
        thread_done_with(unsafe { &*value.cast::<Heap>() });
    }
}

// ---------------------------------------------------------------------------
// Windows: FLS thread-exit callback
// ---------------------------------------------------------------------------

#[cfg(windows)]
unsafe extern "system" {
    fn FlsAlloc(callback: Option<unsafe extern "system" fn(*mut core::ffi::c_void)>) -> u32;
    fn FlsSetValue(fls_handle: u32, value: *mut core::ffi::c_void) -> i32;
    fn GetCurrentThreadId() -> u32;
    fn QueryPerformanceCounter(lpPerformanceCount: *mut i64) -> i32;
}

#[cfg(windows)]
fn register_fork_handlers() {
    // No fork on Windows.
}

#[cfg(windows)]
unsafe extern "system" fn thread_done_callback(value: *mut core::ffi::c_void) {
    if !value.is_null() {
        // SAFETY: the value is this thread's backing heap, still live.
        thread_done_with(unsafe { &*value.cast::<Heap>() });
    }
}

// ---------------------------------------------------------------------------

/// Runs at image load, before `main` and before any thread or fork exists,
/// so the atfork handlers are always registered ahead of any `fork()`. The
/// body is a single libc call and never allocates, so it is safe even when
/// `rimalloc` is the `#[global_allocator]` (the allocator is not used yet).
///
/// Excluded under `fuzzing`: cargo-fuzz's sancov/ASan linker rejects the
/// constructor's `__init_offsets` entry (`initializer pointer has no
/// target`); the fuzz harness is single-process and never forks, so the
/// lazy fallback below suffices there.
#[cfg(all(not(miri), not(fuzzing)))]
#[ctor::ctor(unsafe)]
fn fork_handler_ctor() {
    register_fork_handlers();
}

fn ensure_process_init() {
    static ONCE: AtomicUsize = AtomicUsize::new(0);
    if ONCE.swap(1, Ordering::AcqRel) == 0 {
        #[cfg(not(windows))]
        // SAFETY: valid out-pointer and destructor fn.
        unsafe {
            let mut key: libc::pthread_key_t = 0;
            libc::pthread_key_create(&mut key, Some(thread_done_callback));
            THREAD_KEY.store(key as usize, Ordering::Release);
        }
        #[cfg(windows)]
        // SAFETY: registers a thread-exit callback via FLS.
        unsafe {
            let key = FlsAlloc(Some(thread_done_callback));
            // FLS_OUT_OF_INDEXES = 0xFFFFFFFF
            if key != u32::MAX {
                THREAD_KEY.store(key as usize, Ordering::Release);
            }
        }
        // Builds without the load-time constructor (Miri's interpreter,
        // fuzzing's sancov link) register the handlers lazily here. Neither
        // forks, so the registration-window race the constructor closes does
        // not apply.
        #[cfg(any(miri, fuzzing))]
        register_fork_handlers();
    }
}

/// `mi_thread_init`: set up this thread's tld + backing heap.
pub fn thread_init() {
    if thread_is_initialized() {
        return;
    }
    ensure_process_init();

    let size = crate::constants::align_up(size_of::<ThreadData>(), crate::os::page_size());
    let Some((p, mem)) =
        td_cache_pop().or_else(|| OsMem::alloc_aligned(size, crate::os::page_size(), true))
    else {
        return;
    };
    let td = p.cast::<ThreadData>();
    // SAFETY: fresh OS memory, sized for ThreadData; we initialize all fields.
    unsafe {
        td.write(ThreadData {
            heap: Heap::new_empty(),
            tld: Tld::new(),
            mem,
        });
        let heap = &raw mut (*td).heap;
        let tld = &raw mut (*td).tld;
        (*tld).heap_backing.set(heap);
        crate::heap_ops::heap_init(&*heap, tld, false, 0, 0);
        tls::DEFAULT_HEAP.set(heap);
    }
    THREAD_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::stats::STATS.threads.increase(1);

    let key = THREAD_KEY.load(Ordering::Acquire);
    if key != usize::MAX {
        #[cfg(not(windows))]
        // SAFETY: key was created in ensure_process_init; the value is the
        // backing heap pointer, handed to the destructor at thread exit.
        unsafe {
            libc::pthread_setspecific(key as libc::pthread_key_t, tls::DEFAULT_HEAP.get().cast())
        };
        #[cfg(windows)]
        // SAFETY: key was created in ensure_process_init via FlsAlloc.
        unsafe {
            FlsSetValue(key as u32, tls::DEFAULT_HEAP.get().cast());
        }
    }
}

/// `_mi_thread_done`
pub fn thread_done() {
    if !thread_is_initialized() {
        return;
    }
    thread_done_with(heap_default_unchecked());
}

/// `_mi_thread_heap_done`: tear down a thread given its (backing) heap.
/// `heap` must belong to the current thread.
fn thread_done_with(heap: &Heap) {
    if !heap.is_initialized() {
        return; // already done
    }
    tls::DEFAULT_HEAP.set(empty_heap());
    // Clear the FLS/TSD value so the destructor does not fire again on a heap
    // we are about to free.
    let key = THREAD_KEY.load(Ordering::Acquire);
    if key != usize::MAX {
        #[cfg(not(windows))]
        // SAFETY: valid key.
        unsafe { libc::pthread_setspecific(key as libc::pthread_key_t, ptr::null()) };
        #[cfg(windows)]
        // SAFETY: valid FLS key.
        unsafe { FlsSetValue(key as u32, ptr::null_mut()) };
    }

    // SAFETY: this thread's tld/heaps are live until we free them below.
    unsafe {
        let tld = &*heap.tld.get();
        // The raw backing pointer carries provenance over the whole
        // ThreadData (set in thread_init); keep it for the final free.
        let backing_ptr = tld.heap_backing.get();
        let backing = &*backing_ptr;

        // delete all non-backing heaps
        let mut curr = tld.heaps.get();
        while let Some(h) = curr.as_ref() {
            let next = h.next.get();
            if !ptr::eq(h, backing) {
                crate::heap_ops::heap_delete(h);
            }
            curr = next;
        }

        // abandon what is still live
        crate::heap_ops::heap_collect_abandon(backing);

        // free the thread metadata (cache it for the next thread)
        let td = backing_ptr.cast::<ThreadData>();
        let mem = (*td).mem;
        if !td_cache_push(mem) {
            crate::os::defer_free(mem);
        }
    }
    THREAD_COUNT.fetch_sub(1, Ordering::Relaxed);
    crate::stats::STATS.threads.decrease(1);
    crate::os::drain_deferred();
}
