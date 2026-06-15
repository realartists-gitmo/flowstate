//! User-registered callbacks (`mi_register_deferred_free`,
//! `mi_register_output`, `mi_register_error`).

use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};

/// `mi_deferred_free_fun`: called when the allocator runs low on free
/// blocks, so the program can free cached objects. `force` is set on
/// forced collection; `heartbeat` is a deterministic monotonic count.
pub type DeferredFreeFun = extern "C" fn(force: bool, heartbeat: u64, arg: *mut c_void);
/// `mi_output_fun`: receives diagnostic output (stats, warnings).
pub type OutputFun = extern "C" fn(msg: *const u8, len: usize, arg: *mut c_void);
/// `mi_error_fun`: receives error codes (EFAULT, ENOMEM, EOVERFLOW, EINVAL).
pub type ErrorFun = extern "C" fn(code: i32, arg: *mut c_void);

// Function pointers stored as data pointers (provenance preserved).
static DEFERRED_FREE: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static DEFERRED_ARG: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static OUTPUT: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static OUTPUT_ARG: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static ERROR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static ERROR_ARG: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// `mi_register_deferred_free` (pass `None` to unregister).
pub fn register_deferred_free(f: Option<DeferredFreeFun>, arg: *mut c_void) {
    DEFERRED_ARG.store(arg, Ordering::Release);
    DEFERRED_FREE.store(
        f.map_or(core::ptr::null_mut(), |f| f as *mut ()),
        Ordering::Release,
    );
}

/// `mi_register_output`
pub fn register_output(f: Option<OutputFun>, arg: *mut c_void) {
    OUTPUT_ARG.store(arg, Ordering::Release);
    OUTPUT.store(
        f.map_or(core::ptr::null_mut(), |f| f as *mut ()),
        Ordering::Release,
    );
}

/// `mi_register_error`
pub fn register_error(f: Option<ErrorFun>, arg: *mut c_void) {
    ERROR_ARG.store(arg, Ordering::Release);
    ERROR.store(
        f.map_or(core::ptr::null_mut(), |f| f as *mut ()),
        Ordering::Release,
    );
}

/// `_mi_deferred_free`: bump the heartbeat and call the user hook (guarded
/// against recursion through `tld.recurse`).
pub fn deferred_free(heap: &crate::heap::Heap, force: bool) {
    // SAFETY: tld outlives its heaps; owner-thread access.
    let tld = unsafe { &*heap.tld.get() };
    tld.heartbeat.set(tld.heartbeat.get() + 1);
    let f = DEFERRED_FREE.load(Ordering::Acquire);
    if !f.is_null() && !tld.recurse.get() {
        tld.recurse.set(true);
        // SAFETY: registered via register_deferred_free with this signature.
        let f: DeferredFreeFun = unsafe { core::mem::transmute::<*mut (), DeferredFreeFun>(f) };
        f(
            force,
            tld.heartbeat.get(),
            DEFERRED_ARG.load(Ordering::Acquire),
        );
        tld.recurse.set(false);
    }
}

/// Route a diagnostic message to the registered output hook (or stderr).
pub fn output(msg: &str) {
    let f = OUTPUT.load(Ordering::Acquire);
    if !f.is_null() {
        // SAFETY: registered via register_output with this signature.
        let f: OutputFun = unsafe { core::mem::transmute::<*mut (), OutputFun>(f) };
        f(msg.as_ptr(), msg.len(), OUTPUT_ARG.load(Ordering::Acquire));
    } else {
        #[cfg(not(windows))]
        // SAFETY: plain write(2) to stderr.
        unsafe { libc::write(2, msg.as_ptr().cast(), msg.len()) };
        #[cfg(windows)]
        // SAFETY: WriteFile to stderr handle.
        unsafe {
            const STD_ERROR_HANDLE: u32 = u32::MAX - 12; // -12
            let handle = GetStdHandle(STD_ERROR_HANDLE);
            if !handle.is_null() && handle != winapi_invalid_handle() {
                let mut written: u32 = 0;
                WriteFile(
                    handle,
                    msg.as_ptr().cast(),
                    msg.len().try_into().unwrap_or(u32::MAX),
                    &mut written,
                    core::ptr::null_mut(),
                );
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
    fn WriteFile(
        hFile: *mut core::ffi::c_void,
        lpBuffer: *const core::ffi::c_void,
        nNumberOfBytesToWrite: u32,
        lpNumberOfBytesWritten: *mut u32,
        lpOverlapped: *mut core::ffi::c_void,
    ) -> i32;
}

#[cfg(windows)]
fn winapi_invalid_handle() -> *mut core::ffi::c_void {
    core::ptr::with_exposed_provenance_mut(!0isize as usize) // INVALID_HANDLE_VALUE
}

/// Report an error code to the registered error hook (after the message
/// went to the output hook).
pub fn error(code: i32) {
    let f = ERROR.load(Ordering::Acquire);
    if !f.is_null() {
        // SAFETY: registered via register_error with this signature.
        let f: ErrorFun = unsafe { core::mem::transmute::<*mut (), ErrorFun>(f) };
        f(code, ERROR_ARG.load(Ordering::Acquire));
    }
}
