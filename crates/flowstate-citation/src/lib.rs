//! `flowstate-citation` — parse messy debate "fullcite" strings into structured citation JSON.
//!
//! An independent, callable crate: a fine-tuned Flan-T5 (base) does extraction, wrapped by a
//! pure-Rust deterministic layer that repairs the model's brace-free output, validates it with
//! failure-mode checks, and corrects flagged names against the source text — escalating through
//! a precision cascade only when the cheap deterministic repairs don't clear a citation.
//!
//! The deterministic layer ([`process_raw`]) has no model dependency and is usable on its own.
//! The model backend sits behind the [`Decoder`] trait; a native `CTranslate2` backend is provided
//! under the `ct2` feature ([`backend`]).

pub mod cascade;
pub mod checks;
pub mod normalize;
pub mod reconstruct;
pub mod snap;
pub mod text;

#[cfg(feature = "ct2")]
pub mod backend;

pub use cascade::{run, Decoder, Outcome, Tier};

use serde_json::Value;

/// Deterministic-only pipeline: parse + repair + normalize + sibling-consistency, and if the
/// checks fail, snap-to-source and re-check. Returns `(citation, passed_checks)`. Use this when
/// the caller already has the model's raw output (no re-decoding / escalation).
pub fn process_raw(raw: &str, source: &str) -> (Option<Value>, bool) {
    let mut obj = reconstruct::to_json(raw);
    if let Some(o) = obj.as_mut() {
        normalize::normalize_authors(o);
        normalize::sibling_consistency(o, source);
        // universal source-grounded repairs (fix things the checks never flag)
        snap::snap_families(o, source);
        snap::snap_givens(o, source);
        snap::snap_title(o, source);
        normalize::drop_phantom_authors(o);
        normalize::normalize_authors(o);
    }
    if checks::fails(source, obj.as_ref()).is_none() {
        return (obj, true);
    }
    if let Some(o) = obj.as_mut() {
        snap::snap_authors(o, source);
        normalize::normalize_authors(o);
        normalize::sibling_consistency(o, source);
    }
    let passed = checks::fails(source, obj.as_ref()).is_none();
    (obj, passed)
}

/// Like [`process_raw`] but also returns the failing check reason (`None` if it passes) — used
/// to measure per-detector precision during the divergence sweep.
pub fn process_raw_with_reason(raw: &str, source: &str) -> (Option<Value>, Option<&'static str>) {
    let (obj, _) = process_raw(raw, source);
    let reason = checks::fails(source, obj.as_ref());
    (obj, reason)
}
