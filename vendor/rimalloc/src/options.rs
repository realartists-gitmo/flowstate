//! Runtime options (port of `options.c`): each option reads its
//! `MIMALLOC_*` environment variable once and is settable at runtime
//! (`mi_option_get`/`mi_option_set`).

use core::sync::atomic::{AtomicI64, Ordering};

macro_rules! options {
    ($($variant:ident => $name:ident: $default:expr, $env:literal;)*) => {
        /// `mi_option_t`
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(usize)]
        pub enum Option {
            $($variant),*
        }

        const COUNT: usize = [$($env),*].len();
        const DEFAULTS: [(i64, &str); COUNT] = [$(($default, $env)),*];
        // i64::MIN marks "not yet initialized from the environment".
        static VALUES: [AtomicI64; COUNT] = [const { AtomicI64::new(i64::MIN) }; COUNT];

        $(
            /// Internal shorthand for this option's value.
            #[allow(dead_code)]
            #[inline]
            pub fn $name() -> i64 {
                get(Option::$variant)
            }
        )*
    };
}

options! {
    EagerCommit => eager_commit: 1, "MIMALLOC_EAGER_COMMIT";
    EagerCommitDelay => eager_commit_delay: 0, "MIMALLOC_EAGER_COMMIT_DELAY";
    PurgeDelay => purge_delay: 10, "MIMALLOC_PURGE_DELAY";
    PurgeExtendDelay => purge_extend_delay: 1, "MIMALLOC_PURGE_EXTEND_DELAY";
    AbandonedPagePurge => abandoned_page_purge: 0, "MIMALLOC_ABANDONED_PAGE_PURGE";
    FullPageRetain => full_page_retain: 2, "MIMALLOC_FULL_PAGE_RETAIN";
    MaxPageCandidates => max_page_candidates: 4, "MIMALLOC_MAX_PAGE_CANDIDATES";
    MaxSegmentReclaim => max_segment_reclaim: 10, "MIMALLOC_MAX_SEGMENT_RECLAIM";
    AbandonedReclaimOnFree => abandoned_reclaim_on_free: 0, "MIMALLOC_ABANDONED_RECLAIM_ON_FREE";
    TargetSegmentsPerThread => target_segments_per_thread: 0, "MIMALLOC_TARGET_SEGMENTS_PER_THREAD";
    PurgeDecommits => purge_decommits: 1, "MIMALLOC_PURGE_DECOMMITS";
    ArenaReserve => arena_reserve: 1024 * 1024, "MIMALLOC_ARENA_RESERVE"; // KiB
    ShowErrors => show_errors: 0, "MIMALLOC_SHOW_ERRORS";
    ShowStats => show_stats: 0, "MIMALLOC_SHOW_STATS";
    Verbose => verbose: 0, "MIMALLOC_VERBOSE";
}

/// Case-insensitive comparison of `s` against a short literal, without
/// allocating (avoids `to_ascii_lowercase` → String).
fn eq_ignore_case(s: &str, lit: &str) -> bool {
    s.len() == lit.len() && s.as_bytes().iter().zip(lit.as_bytes())
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn parse(s: &str) -> std::option::Option<i64> {
    if s.is_empty() || eq_ignore_case(s, "on") || eq_ignore_case(s, "yes") || eq_ignore_case(s, "true")
    {
        Some(1)
    } else if eq_ignore_case(s, "off") || eq_ignore_case(s, "no") || eq_ignore_case(s, "false") {
        Some(0)
    } else {
        s.parse().ok()
    }
}

/// Read an environment variable using raw OS calls that do **not** go
/// through `#[global_allocator]`.  `std::env::var()` allocates a `String`,
/// which would recurse into `Rimalloc::alloc()` when called from the
/// allocation slow path.
fn read_env(env: &str) -> std::option::Option<i64> {
    let mut buf = [0u8; 512];

    #[cfg(unix)]
    let val = {
        let mut cstr = [0i8; 128];
        let b = env.as_bytes();
        if b.len() + 1 > cstr.len() {
            return None;
        }
        let mut i = 0;
        while i < b.len() {
            cstr[i] = b[i] as i8;
            i += 1;
        }
        cstr[i] = 0;
        let p = unsafe { libc::getenv(cstr.as_ptr()) };
        if p.is_null() {
            return None;
        }
        let s = unsafe { core::ffi::CStr::from_ptr(p) };
        s.to_str().ok()?
    };

    #[cfg(windows)]
    let val = {
        // env → UTF‑16 name
        let mut name_wide: [u16; 128] = [0; 128];
        for (name_len, c) in env.encode_utf16().enumerate() {
            if name_len >= name_wide.len() - 1 {
                return None;
            }
            name_wide[name_len] = c;
        }

        // Get value length
        let required =
            unsafe { GetEnvironmentVariableW(name_wide.as_ptr(), core::ptr::null_mut(), 0) };
        if required == 0 {
            return None;
        }

        // Read value wide
        let mut val_wide: [u16; 256] = [0; 256];
        if (required as usize) > val_wide.len() {
            return None;
        }
        let n = unsafe {
            GetEnvironmentVariableW(
                name_wide.as_ptr(),
                val_wide.as_mut_ptr(),
                val_wide.len() as u32,
            )
        };
        if n == 0 {
            return None;
        }

        // Wide → UTF‑8 via stack buffer
        let utf8_len = unsafe {
            WideCharToMultiByte(
                65001, 0, val_wide.as_ptr(), n as i32,
                core::ptr::null_mut(), 0,
                core::ptr::null(), core::ptr::null_mut(),
            )
        };
        if utf8_len <= 0 || (utf8_len as usize) >= buf.len() {
            return None;
        }
        let written = unsafe {
            WideCharToMultiByte(
                65001, 0, val_wide.as_ptr(), n as i32,
                buf.as_mut_ptr(), buf.len() as i32,
                core::ptr::null(), core::ptr::null_mut(),
            )
        };
        if written <= 0 {
            return None;
        }
        core::str::from_utf8(&buf[..written as usize]).ok()?
    };

    parse(val)
}

/// `mi_option_get`
pub fn get(option: Option) -> i64 {
    let idx = option as usize;
    match VALUES[idx].load(Ordering::Relaxed) {
        i64::MIN => {
            let (default, env) = DEFAULTS[idx];
            let v = read_env(env).unwrap_or(default);
            VALUES[idx].store(v, Ordering::Relaxed);
            v
        }
        v => v,
    }
}

/// `mi_option_set`
pub fn set(option: Option, value: i64) {
    VALUES[option as usize].store(value, Ordering::Relaxed);
}

/// `mi_option_is_enabled`
pub fn is_enabled(option: Option) -> bool {
    get(option) != 0
}

/// `mi_option_enable` / `mi_option_disable`
pub fn enable(option: Option) {
    set(option, 1)
}
pub fn disable(option: Option) {
    set(option, 0)
}

/// `_mi_clock_now`: monotonic milliseconds. Only used for relative purge
/// deadlines (>= 10ms granularity), so on Apple the cheap commpage-cached
/// approximate clock is used; `CLOCK_MONOTONIC` there is a gettimeofday +
/// boottime computation that shows up in thread-churn profiles.
#[cfg(all(target_vendor = "apple", not(miri)))]
pub fn clock_now() -> i64 {
    unsafe extern "C" {
        fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
    }
    // SAFETY: no preconditions.
    (unsafe { clock_gettime_nsec_np(libc::CLOCK_MONOTONIC_RAW_APPROX) } / 1_000_000) as i64
}

/// `_mi_clock_now`: monotonic milliseconds.
#[cfg(all(not(all(target_vendor = "apple", not(miri))), not(windows)))]
#[allow(clippy::unnecessary_cast)] // tv_sec/tv_nsec are narrower off-macOS
pub fn clock_now() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as i64 * 1000 + ts.tv_nsec as i64 / 1_000_000
}

/// `_mi_clock_now`: monotonic milliseconds (Windows via QueryPerformanceCounter).
#[cfg(all(not(all(target_vendor = "apple", not(miri))), windows))]
pub fn clock_now() -> i64 {
    // SAFETY: QPF and QPC are safe to call at any time.
    unsafe {
        let mut freq: i64 = 0;
        let mut count: i64 = 0;
        QueryPerformanceFrequency(&mut freq);
        QueryPerformanceCounter(&mut count);
        if freq == 0 {
            return 0;
        }
        (count / freq * 1000) + ((count % freq) * 1000 / freq)
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn QueryPerformanceCounter(lpPerformanceCount: *mut i64) -> i32;
    fn QueryPerformanceFrequency(lpFrequency: *mut i64) -> i32;
    fn GetEnvironmentVariableW(lpName: *const u16, lpBuffer: *mut u16, nSize: u32) -> u32;
    fn WideCharToMultiByte(
        CodePage: u32,
        dwFlags: u32,
        lpWideCharStr: *const u16,
        cchWideChar: i32,
        lpMultiByteStr: *mut u8,
        cbMultiByte: i32,
        lpDefaultChar: *const u8,
        lpUsedDefaultChar: *mut i32,
    ) -> i32;
}
