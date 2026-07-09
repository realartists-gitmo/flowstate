//! The precision cascade + deterministic-repair controller.
//!
//! Each tier: decode → parse+repair → normalize → sibling-consistency → checks; on failure,
//! snap-to-source and re-check. Order is cheapest-first (int8 → int16 → f32 → beam →
//! best-of-N sampling). If the ladder exhausts, the human gets the *best-grounded* candidate
//! seen across all tiers (escalation can degrade text, so the last decode is often not best).

use crate::checks::{fails, grounded};
use crate::normalize::{drop_phantom_authors, normalize_authors, sibling_consistency};
use crate::reconstruct::to_json;
use crate::snap::snap_authors;
use crate::text::tokens;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Int8,
    Int16,
    Float32,
    Beam,
    BestOf,
}

/// A model backend. `decode` returns the raw (brace-free) model output for one input at a tier.
pub trait Decoder {
    fn decode(&mut self, input: &str, tier: Tier) -> anyhow::Result<String>;
}

/// The result of running the cascade for one citation.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub value: Option<Value>,
    pub passed: bool,
    pub tier: Option<Tier>,
    pub snapped: bool,
    pub reason: Option<&'static str>,
}

/// Deterministic post-processing of one raw decode. Returns (candidate, passed, snapped).
/// On failure the *snapped* candidate is returned — it is the better-grounded one to keep.
pub fn resolve(raw: &str, source: &str) -> (Option<Value>, bool, bool) {
    let mut obj = to_json(raw);
    if let Some(o) = obj.as_mut() {
        normalize_authors(o);
        sibling_consistency(o, source);
        crate::snap::snap_families(o, source);
        crate::snap::snap_givens(o, source);
        crate::snap::snap_title(o, source);
        drop_phantom_authors(o);
        normalize_authors(o);
    }
    if fails(source, obj.as_ref()).is_none() {
        return (obj, true, false);
    }
    if let Some(o) = obj.as_ref() {
        let mut snapped = o.clone();
        snap_authors(&mut snapped, source);
        normalize_authors(&mut snapped);
        sibling_consistency(&mut snapped, source);
        let passed = fails(source, Some(&snapped)).is_none();
        return (Some(snapped), passed, true);
    }
    (obj, false, false)
}

/// Number of authors whose every name word is grounded in the source (best-candidate metric).
fn grounded_score(obj: &Value, source: &str) -> i64 {
    let toks = tokens(source);
    let mut score = 0;
    if let Some(authors) = obj.get("authors").and_then(Value::as_array) {
        for a in authors {
            let ok = ["family", "given"].iter().all(|k| {
                a.get(*k)
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.split_whitespace().all(|w| grounded(w, &toks)))
            });
            if ok {
                score += 1;
            }
        }
    }
    score
}

/// Track the best-grounded failing candidate seen so far.
fn consider(best: &mut Option<(Value, i64, Tier)>, obj: Option<&Value>, tier: Tier, source: &str) {
    if let Some(o) = obj {
        let sc = grounded_score(o, source);
        if best.as_ref().is_none_or(|(_, bs, _)| sc > *bs) {
            *best = Some((o.clone(), sc, tier));
        }
    }
}

/// Run the full cascade for one citation.
pub fn run<D: Decoder>(dec: &mut D, input: &str) -> anyhow::Result<Outcome> {
    let mut best: Option<(Value, i64, Tier)> = None;
    // int16 is omitted: the ruy CPU backend doesn't support it, and int8 ≈ int16 ≈ f32 on
    // quality anyway (int16 recovered ~2/250 on gold, all now handled by snap).
    for tier in [Tier::Int8, Tier::Float32, Tier::Beam] {
        let raw = dec.decode(input, tier)?;
        let (obj, passed, snapped) = resolve(&raw, input);
        if passed {
            return Ok(Outcome { value: obj, passed: true, tier: Some(tier), snapped, reason: None });
        }
        consider(&mut best, obj.as_ref(), tier, input);
    }
    for _ in 0..5 {
        let raw = dec.decode(input, Tier::BestOf)?;
        let (obj, passed, snapped) = resolve(&raw, input);
        if passed {
            return Ok(Outcome {
                value: obj,
                passed: true,
                tier: Some(Tier::BestOf),
                snapped,
                reason: None,
            });
        }
        consider(&mut best, obj.as_ref(), Tier::BestOf, input);
    }
    match best {
        Some((o, _, tier)) => {
            let reason = fails(input, Some(&o));
            Ok(Outcome { value: Some(o), passed: false, tier: Some(tier), snapped: true, reason })
        }
        None => Ok(Outcome {
            value: None,
            passed: false,
            tier: None,
            snapped: false,
            reason: Some("invalid_json"),
        }),
    }
}
