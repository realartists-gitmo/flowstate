//! Conformance against the Lean-verified model (`verify/`).
//!
//! The Lean project proves theorems about a reference model; these tests
//! pin the live Rust implementation to that model over the *entire* input
//! domain. Refactor the Rust freely: if it still passes here, every Lean
//! theorem (fit, <25% fragmentation, monotonicity, range, round-trip)
//! transfers to the new code. Regenerate the vectors (`lake exe
//! genvectors`) only when the spec itself changes.

use rimalloc::lean_model;

#[cfg(not(windows))]
#[test]
fn bins_match_lean_model() {
    let csv = include_str!("../../../verify/vectors/bins.csv");
    let mut checked = 0;
    for line in csv.lines() {
        let (w, bin) = line.split_once(',').expect("w,bin");
        let w: usize = w.parse().unwrap();
        let expect: usize = bin.parse().unwrap();
        assert_eq!(
            lean_model::bin_from_wsize(w),
            expect,
            "Bin::from_size diverged from the verified model at wsize {w}"
        );
        checked += 1;
    }
    assert_eq!(checked, 8194); // the full domain plus one past it
}

#[cfg(not(windows))]
#[test]
fn bin_sizes_match_lean_model() {
    let csv = include_str!("../../../verify/vectors/bin_sizes.csv");
    for line in csv.lines() {
        let (bin, wsize) = line.split_once(',').expect("bin,wsize");
        let bin: usize = bin.parse().unwrap();
        let expect: usize = wsize.parse().unwrap();
        assert_eq!(
            lean_model::bin_wsize(bin),
            expect,
            "BIN_WSIZE diverged from the verified model at bin {bin}"
        );
    }
}

#[test]
fn commit_mask_matches_closed_form() {
    // Lean proves the create loop equals ((1 << count) - 1) << idx
    // (`CommitMask.create_correct`); check the live Rust loop against the
    // same closed form for every valid (idx, count) pair. Under Miri the
    // ~67M-assert grid is sampled with a coprime stride (the full grid
    // still runs natively, and the property is Lean-proven).
    let step = if cfg!(miri) { 37 } else { 1 };
    for idx in (0..=512usize).step_by(step) {
        for count in (0..=(512 - idx)).step_by(step) {
            let mask = lean_model::commit_mask_create_words(idx, count);
            for bit in 0..512 {
                let expect = bit >= idx && bit < idx + count;
                let got = mask[bit / 64] & (1 << (bit % 64)) != 0;
                assert_eq!(got, expect, "create({idx},{count}) bit {bit}");
            }
        }
    }
}

#[test]
fn page_start_offset_matches_lean_contract() {
    // PageStart.lean proves three properties of the offset computation over
    // every reachable bin <= MAX_ALIGN_GUARANTEE and every 16-aligned
    // page-start residue; check the live function against the same
    // contract over the same (exhaustive) domain.
    for bin in 1..49usize {
        let bs = lean_model::bin_wsize(bin) * 8;
        let reachable = lean_model::bin_from_wsize(lean_model::bin_wsize(bin)) == bin;
        let psize = if bs <= 8192 { 65536 } else { 524288 };
        for k in 0..4096usize {
            let pstart = 16 * k;
            let off = lean_model::page_start_offset(bs, pstart, psize);
            // pageStart_align16
            assert_eq!(off % 16, 0, "16-alignment, bs={bs} pstart={pstart}");
            if reachable {
                // pageStart_blockAligned
                assert_eq!(
                    (pstart + off) % bs,
                    0,
                    "block alignment guarantee, bs={bs} pstart={pstart}"
                );
                // pageStart_fits
                assert!(off + bs <= psize, "fits, bs={bs} pstart={pstart}");
            }
        }
    }
}
