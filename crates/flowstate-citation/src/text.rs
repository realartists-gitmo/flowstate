//! Text utilities: diacritic-folding and name-oriented fuzzy similarity.
//!
//! Ports the Python reference (`snap.py`, `bench.py`). Similarity combines a
//! Ratcliff–Obershelp ratio (difflib parity) with Jaro–Winkler (purpose-built for
//! name typos: `Javad`/`Jaded` ≈ 0.86, `Tomo`/`Toto` ≈ 0.93).

use strsim::jaro_winkler;
use unicode_normalization::UnicodeNormalization;

/// NFKD-fold to lowercase ASCII alphanumerics. Diacritics decompose into combining
/// marks that are then dropped, so `Šarić` → `saric`, `O'Neil` → `oneil`.
pub fn fold(s: &str) -> String {
    s.nfkd()
        .flat_map(char::to_lowercase)
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// Fold every whitespace-delimited word of `s`, dropping empties.
pub fn tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(fold)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Total matched characters under Ratcliff–Obershelp (recursive longest common
/// contiguous block). Mirrors difflib's `SequenceMatcher` match count for junk-free input.
fn ro_matches(a: &[char], b: &[char]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (mut bi, mut bj, mut blen) = (0usize, 0usize, 0usize);
    let mut prev = vec![0usize; b.len() + 1];
    for (i, &ai) in a.iter().enumerate() {
        let mut cur = vec![0usize; b.len() + 1];
        for j in 0..b.len() {
            if ai == b[j] {
                let v = prev[j] + 1;
                cur[j + 1] = v;
                if v > blen {
                    blen = v;
                    bi = i + 1 - v;
                    bj = j + 1 - v;
                }
            }
        }
        prev = cur;
    }
    if blen == 0 {
        return 0;
    }
    ro_matches(&a[..bi], &b[..bj]) + blen + ro_matches(&a[bi + blen..], &b[bj + blen..])
}

/// difflib-equivalent ratio in `[0, 1]`.
pub fn ratio(a: &str, b: &str) -> f64 {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let t = ca.len() + cb.len();
    if t == 0 {
        return 1.0;
    }
    2.0 * ro_matches(&ca, &cb) as f64 / t as f64
}

/// Edit distance ≤ 1 (single substitution or single insertion/deletion), `a != b`.
#[allow(clippy::many_single_char_names, reason = "two-pointer edit-distance scan")]
pub fn ed1(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a == b {
        return false;
    }
    let (la, lb) = (a.len(), b.len());
    if la.abs_diff(lb) > 1 {
        return false;
    }
    if la == lb {
        return a.iter().zip(&b).filter(|(x, y)| x != y).count() == 1;
    }
    let (s, l) = if la < lb { (&a, &b) } else { (&b, &a) };
    let (mut i, mut j, mut diff) = (0usize, 0usize, 0usize);
    while i < s.len() && j < l.len() {
        if s[i] != l[j] {
            diff += 1;
            j += 1;
            if diff > 1 {
                return false;
            }
        } else {
            i += 1;
            j += 1;
        }
    }
    true
}

/// Is `a` a (not-necessarily-contiguous) subsequence of `b`?
pub fn subseq(a: &str, b: &str) -> bool {
    let mut it = b.chars();
    a.chars().all(|c| it.any(|x| x == c))
}

/// Name-oriented similarity in `[0, 1]`: max(RO ratio, Jaro–Winkler), boosted to 0.9 for
/// an edit-distance-1 typo and 0.88 for a short containment. Inputs should be pre-folded.
pub fn sim(nv: &str, ng: &str) -> f64 {
    if nv.is_empty() || ng.is_empty() {
        return 0.0;
    }
    let mut r = ratio(nv, ng).max(jaro_winkler(nv, ng));
    if ed1(nv, ng) {
        r = r.max(0.9);
    }
    let (ln, lg) = (nv.chars().count(), ng.chars().count());
    if ln >= 4 && (ln as isize - lg as isize).unsigned_abs() <= 2 && (ng.contains(nv) || nv.contains(ng)) {
        r = r.max(0.88);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_strips_diacritics() {
        assert_eq!(fold("Šarić"), "saric");
        assert_eq!(fold("O'Neil"), "oneil");
    }

    #[test]
    fn jaro_winkler_catches_name_typos() {
        assert!(sim(&fold("Javad"), &fold("Jaded")) >= 0.72);
        assert!(sim(&fold("Tomo"), &fold("Toto")) >= 0.85);
        assert!(ed1("wink", "wick"));
    }
}
