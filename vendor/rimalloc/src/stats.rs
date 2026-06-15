//! Statistics (port of `stats.c`, condensed). Segment/page-level events are
//! always counted (they are rare); per-allocation counting is gated behind
//! the `stats` cargo feature, mirroring C's `MI_STAT` levels.

use core::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

/// `mi_stat_count_t`: current/peak/total.
#[derive(Default)]
pub struct StatCount {
    pub current: AtomicI64,
    pub peak: AtomicI64,
    pub total: AtomicI64,
}

impl StatCount {
    pub const fn new() -> StatCount {
        StatCount {
            current: AtomicI64::new(0),
            peak: AtomicI64::new(0),
            total: AtomicI64::new(0),
        }
    }
    pub fn increase(&self, amount: usize) {
        let cur = self.current.fetch_add(amount as i64, Ordering::Relaxed) + amount as i64;
        self.peak.fetch_max(cur, Ordering::Relaxed);
        self.total.fetch_add(amount as i64, Ordering::Relaxed);
    }
    pub fn decrease(&self, amount: usize) {
        self.current.fetch_sub(amount as i64, Ordering::Relaxed);
    }
    fn get(&self) -> (i64, i64, i64) {
        (
            self.current.load(Ordering::Relaxed),
            self.peak.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }
}

/// Process-wide statistics (`_mi_stats_main`).
pub struct Stats {
    pub segments: StatCount,
    pub pages: StatCount,
    pub committed: StatCount,
    pub reserved: StatCount,
    pub threads: StatCount,
    pub malloc_normal: StatCount,
    pub malloc_huge: StatCount,
    #[allow(dead_code)] // read by stats-feature builds / external consumers
    pub malloc_normal_count: AtomicUsize,
    #[allow(dead_code)]
    pub malloc_huge_count: AtomicUsize,
    pub pages_extended: AtomicUsize,
    pub pages_retire: AtomicUsize,
    pub segments_abandoned: AtomicUsize,
    pub segments_reclaimed: AtomicUsize,
}

pub static STATS: Stats = Stats {
    segments: StatCount::new(),
    pages: StatCount::new(),
    committed: StatCount::new(),
    reserved: StatCount::new(),
    threads: StatCount::new(),
    malloc_normal: StatCount::new(),
    malloc_huge: StatCount::new(),
    malloc_normal_count: AtomicUsize::new(0),
    malloc_huge_count: AtomicUsize::new(0),
    pages_extended: AtomicUsize::new(0),
    pages_retire: AtomicUsize::new(0),
    segments_abandoned: AtomicUsize::new(0),
    segments_reclaimed: AtomicUsize::new(0),
};

/// Per-allocation accounting; compiled out unless the `stats` feature is on
/// (like release-mode C with `MI_STAT=0`).
#[inline(always)]
pub fn stat_malloc(size: usize, huge: bool) {
    #[cfg(feature = "stats")]
    {
        if huge {
            STATS.malloc_huge.increase(size);
            STATS.malloc_huge_count.fetch_add(1, Ordering::Relaxed);
        } else {
            STATS.malloc_normal.increase(size);
            STATS.malloc_normal_count.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[cfg(not(feature = "stats"))]
    {
        let _ = (size, huge);
    }
}

fn fmt_bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    if n >= 1e9 {
        format!("{:.1} GiB", n / (1u64 << 30) as f64)
    } else if n >= 1e6 {
        format!("{:.1} MiB", n / (1u64 << 20) as f64)
    } else if n >= 1e3 {
        format!("{:.1} KiB", n / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// `mi_stats_print`: write a summary to stderr (or any writer).
pub fn print(out: &mut dyn std::io::Write) -> std::io::Result<()> {
    let rows: &[(&str, &StatCount, bool)] = &[
        ("reserved", &STATS.reserved, true),
        ("committed", &STATS.committed, true),
        ("segments", &STATS.segments, false),
        ("pages", &STATS.pages, false),
        ("threads", &STATS.threads, false),
        ("malloc normal", &STATS.malloc_normal, true),
        ("malloc huge", &STATS.malloc_huge, true),
    ];
    writeln!(
        out,
        "{:<14} {:>12} {:>12} {:>12}",
        "heap stats:", "current", "peak", "total"
    )?;
    for (name, stat, bytes) in rows {
        let (cur, peak, total) = stat.get();
        if *bytes {
            writeln!(
                out,
                "{:<14} {:>12} {:>12} {:>12}",
                name,
                fmt_bytes(cur),
                fmt_bytes(peak),
                fmt_bytes(total)
            )?;
        } else {
            writeln!(out, "{name:<14} {cur:>12} {peak:>12} {total:>12}")?;
        }
    }
    writeln!(
        out,
        "{:<14} extended: {}, retired: {}, abandoned: {}, reclaimed: {}",
        "pages ops:",
        STATS.pages_extended.load(Ordering::Relaxed),
        STATS.pages_retire.load(Ordering::Relaxed),
        STATS.segments_abandoned.load(Ordering::Relaxed),
        STATS.segments_reclaimed.load(Ordering::Relaxed),
    )?;
    Ok(())
}
