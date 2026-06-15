//! The `committed` stat tracks committed bytes at whole-segment
//! granularity (see `SegmentsTld::track_size`): it must rise when segments
//! are created, fall when they are released, and never go negative — under
//! both reset-mode and decommit-mode purges.
//!
//! The whole-segment counter is balanced *by construction*: every increase
//! (segment enters) is paired with an equal decrease (segment leaves) at the
//! same granularity, so a churn cycle can never drive it negative and it
//! returns to its pre-churn baseline once everything is collected — in either
//! purge mode. (A globally *exact*, sub-segment counter is deliberately not
//! attempted; see the `track_size` doc comment for why it stays coarse.)
#![cfg(feature = "stats")]

use rimalloc::options::{self, Option};

#[test]
fn committed_rises_and_falls_without_underflow() {
    let base = rimalloc::stats_committed_current();
    assert!(base >= 0);

    let p = rimalloc::malloc(8 * 1024);
    assert!(!p.is_null());
    unsafe { p.write_bytes(7, 8 * 1024) };
    let after_alloc = rimalloc::stats_committed_current();
    assert!(after_alloc > base, "segment commit not counted");

    rimalloc::free(p);
    rimalloc::collect(true);
    let after_free = rimalloc::stats_committed_current();
    assert!(
        after_free >= 0,
        "committed stat went negative: {after_free}"
    );
    assert!(
        after_free <= after_alloc,
        "free/purge did not reduce committed: {after_alloc} -> {after_free}"
    );

    // Aligned (trim-path) mapping and a huge allocation must also balance.
    let a = rimalloc::malloc_aligned(1 << 20, 1 << 21);
    assert!(!a.is_null());
    rimalloc::free(a);
    let h = rimalloc::malloc(40 << 20);
    assert!(!h.is_null());
    rimalloc::free(h);
    rimalloc::collect(true);
    assert!(rimalloc::stats_committed_current() >= 0);
}

/// Drive a churn cycle that forces many segments (and arena blocks) through
/// alloc/free/purge, in *both* purge modes, asserting the committed counter
/// never underflows mid-churn and lands back at a bounded baseline after a
/// full collect. This is the exact OS<->arena cross-layer purge path the
/// routing change touches; under the old segment-level decommit of arena
/// memory this churn could drive committed negative.
#[test]
fn committed_never_underflows_across_churn_in_both_modes() {
    // 0 = reset/madvise mode, 1 = decommit mode.
    for &decommit in &[0_i64, 1_i64] {
        options::set(Option::PurgeDecommits, decommit);
        // Short delays so purges actually fire during the run.
        options::set(Option::PurgeDelay, 0);

        let baseline = rimalloc::stats_committed_current();
        assert!(
            baseline >= 0,
            "committed negative at baseline (mode decommit={decommit}): {baseline}"
        );

        // Several rounds of churn: enough live segments to span multiple
        // arena blocks, freed and re-collected each round.
        for round in 0..8 {
            let mut live = Vec::new();
            // ~24 MiB of 256 KiB blocks => multiple segments / arena blocks.
            for i in 0..96 {
                let p = rimalloc::malloc(256 * 1024);
                assert!(!p.is_null());
                // Touch the first and last page so the range is really faulted.
                unsafe {
                    p.write_bytes((i & 0xff) as u8, 64);
                    p.add(256 * 1024 - 64).write_bytes(0xab, 64);
                }
                live.push(p);
                let cur = rimalloc::stats_committed_current();
                assert!(
                    cur >= 0,
                    "committed negative during alloc (mode={decommit}, round={round}, i={i}): {cur}"
                );
            }
            for p in live.drain(..) {
                rimalloc::free(p);
                let cur = rimalloc::stats_committed_current();
                assert!(
                    cur >= 0,
                    "committed negative during free (mode={decommit}, round={round}): {cur}"
                );
            }
            rimalloc::collect(true);
            let cur = rimalloc::stats_committed_current();
            assert!(
                cur >= 0,
                "committed negative after collect (mode={decommit}, round={round}): {cur}"
            );
        }

        rimalloc::collect(true);
        let after = rimalloc::stats_committed_current();
        assert!(
            after >= 0,
            "committed negative after churn (mode decommit={decommit}): {after}"
        );
        // The coarse counter is balanced by construction: after a full collect
        // it must not have grown unboundedly relative to the round's working
        // set (a leak would show here). Allow some cached segments to remain.
        assert!(
            after <= baseline + (64 << 20),
            "committed leaked across churn (mode decommit={decommit}): \
             baseline={baseline} after={after}"
        );
    }
    // Restore defaults for any later test in this binary.
    options::set(Option::PurgeDecommits, 1);
    options::set(Option::PurgeDelay, 10);
}
