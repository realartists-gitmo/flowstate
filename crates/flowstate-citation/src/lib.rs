//! `flowstate-citation` — parse messy debate "fullcite" strings into structured citation JSON.
//!
//! An independent, callable crate: a fine-tuned Flan-T5 (base) does extraction, wrapped by a
//! pure-Rust deterministic layer that repairs the model's brace-free output, validates it with
//! failure-mode checks, and corrects flagged names against the source text. The model controller
//! cross-checks independent precision tiers before success so a clean-looking author subset is
//! not accepted when another decode sees additional authors.
//!
//! The deterministic layer ([`process_raw`]) has no model dependency and is usable on its own.
//! The model backend sits behind the [`Decoder`] trait; a native `CTranslate2` backend is provided
//! under the `ct2` feature ([`backend`]).

pub mod cascade;
pub mod checks;
pub mod flag;
pub mod gazetteer;
pub mod limits;
pub mod normalize;
pub mod perturb;
pub mod reconstruct;
pub mod schema;
pub mod snap;
pub mod text;

#[cfg(feature = "ct2")]
pub mod backend;

pub use cascade::{Decode, DecodeIssue, Decoder, Outcome, Tier, run};

use serde_json::Value;

/// Preserve the model's original CSL author evidence before aliasing duplicate `given`/`literal`
/// keys.  Splitting on `<unk>` isolates the model's brace-free author objects; when `literal` is
/// present it is the authoritative full name, otherwise fall back to canonical `name` or `given`.
fn decoded_author_evidence(raw: &str, reconstructed: Option<&Value>) -> Vec<(String, String)> {
    let mut evidence = Vec::new();
    for segment in raw.split("<unk>") {
        if !segment.contains("\"family\"") && !segment.contains("\"surname\"") {
            continue;
        }
        let candidate = format!("{{{}}}", segment.trim().trim_matches(','));
        let Ok(author) = serde_json::from_str::<Value>(&candidate) else {
            continue;
        };
        let surname = author
            .get("family")
            .or_else(|| author.get("surname"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = author
            .get("literal")
            .or_else(|| author.get("name"))
            .or_else(|| author.get("given"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !surname.is_empty() {
            evidence.push((surname.to_string(), name.to_string()));
        }
    }
    if !evidence.is_empty() {
        return evidence;
    }
    reconstructed
        .and_then(|value| value.get("authors"))
        .and_then(Value::as_array)
        .map(|authors| {
            authors
                .iter()
                .map(|author| {
                    (
                        author.get("surname").and_then(Value::as_str).unwrap_or("").to_string(),
                        author.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Which flag architecture the deterministic pipeline applies.
///
/// The failure-mode checks + strict schema ([`FlagMode::Checks`]) and the two-tier precision
/// consensus ([`run`], reachable via the `consensus` bin) are both *pinned*: kept intact and
/// selectable, but no longer the default. [`FlagMode::Passthrough`] is the clean baseline — pure
/// deterministic repair, never flags — against which a cheaper decorrelation flagger is measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlagMode {
    /// Deterministic repair only; best-effort output, no review flag. The baseline.
    #[default]
    Passthrough,
    /// Repair, then gate on [`checks::fails`] (failure-mode detectors + strict schema). The pinned
    /// single-tier deterministic flagger (26% flag precision — superseded by `Review`).
    Checks,
    /// Repair, then gate on [`flag::review_reason`] (ungrounded author / byline-omission). The
    /// grounding-based flagger aimed at zero silent author errors.
    Review,
}

/// Universal source-grounded deterministic repairs applied to a raw decode. These *correct*
/// output (they never reject it), so they run in every mode. Flagging is layered on top.
fn repair(raw: &str, source: &str) -> Option<Value> {
    let mut obj = reconstruct::to_json(raw);
    let decoded_authors = decoded_author_evidence(raw, obj.as_ref());
    if let Some(o) = obj.as_mut() {
        normalize::normalize_authors(o, source);
        snap::snap_surnames(o, source);
        snap::snap_surname_span(o, source);
        snap::snap_cite_tag(o, source);
        snap::snap_names(o, source);
        snap::ground_names(o, source);
        snap::recover_empty_author(o, source);
        snap::recover_key_coauthors(o, source);
        snap::recover_byline_coauthors(o, source);
        snap::drop_fabricated_near_dups(o, source);
        snap::snap_title(o, source);
        normalize::strip_superscript_markers(o, source);
        normalize::strip_defined_affiliation_markers(o, source);
        normalize::drop_phantom_authors(o);
        normalize::normalize_authors(o, source);
        snap::restore_omitted_family_from_model_tag(o, source, &decoded_authors);
        snap::restore_omitted_families_from_model_key(o, source, &decoded_authors);
        snap::recover_semicolon_record_authors(o, source);
        snap::recover_bibliographic_roster_authors(o, source);
        snap::recover_conjunction_chain_authors(o, source);
        snap::recover_contribution_statement_authors(o, source);
        snap::recover_about_authors_section(o, source);
        snap::recover_semicolon_bio_authors(o, source);
        snap::recover_degree_delimited_authors(o, source);
        snap::repair_mixed_inverted_byline(o, source);
        snap::recover_ranked_roster_authors(o, source);
        snap::recover_juxtaposed_bio_author(o, source);
        snap::recover_starred_footnote_authors(o, source);
        snap::recover_allcaps_bio_authors(o, source);
        snap::recover_post_affiliation_author(o, source);
        snap::recover_marked_bracket_authors(o, source);
        snap::recover_numbered_inline_authors(o, source);
        snap::recover_strong_empty_author(o, source);
        snap::recover_ranked_key_author(o, source);
        snap::recover_bare_author_shorthand(o, source);
        snap::recover_role_prefixed_authors(o, source);
        snap::reconcile_explicit_key_authors(o, source);
        snap::recover_page_header_coauthor(o, source);
        snap::repair_repeated_single_initial_surname(o, source);
        snap::repair_concatenated_role_byline(o, source);
        snap::restore_source_confirmed_decoded_families(o, source, &decoded_authors);
        snap::repair_speech_marker_family(o, source);
        snap::repair_starred_surname_first_byline(o, source);
        snap::repair_conjunction_byline_spelling(o, source);
        snap::repair_pipe_volume_author_tail(o, source);
        snap::repair_leading_inverted_author(o, source);
        snap::repair_sole_author_byline_spelling(o, source);
        snap::retain_primary_credit_group(o, source);
        snap::drop_fabricated_near_dups(o, source);
        snap::drop_structural_nonperson_authors(o, source);
        snap::drop_obvious_publication_author(o, source);
        normalize::drop_non_name_authors(o);
    }
    obj
}

/// Deterministic-only pipeline: parse + repair + normalize, and if the checks fail,
/// snap-to-source and re-check. Returns `(citation, passed_checks)`. Use this when the caller
/// already has a raw output whose normal EOS completion was verified separately. The full
/// [`run`] controller additionally rejects length-capped output and requires decode consensus.
pub fn process_raw(raw: &str, source: &str) -> (Option<Value>, bool) {
    let mut obj = repair(raw, source);
    if checks::fails(source, obj.as_ref()).is_none() {
        return (obj, true);
    }
    if let Some(o) = obj.as_mut() {
        snap::snap_authors(o, source);
        normalize::normalize_authors(o, source);
    }
    let passed = checks::fails(source, obj.as_ref()).is_none();
    (obj, passed)
}

/// Deterministic pipeline with a selectable [`FlagMode`]. Returns `(citation, flag_reason)` where
/// `flag_reason` is `None` when the output is accepted. [`FlagMode::Passthrough`] never flags.
pub fn process_with_mode(raw: &str, source: &str, mode: FlagMode) -> (Option<Value>, Option<&'static str>) {
    match mode {
        FlagMode::Passthrough => (repair(raw, source), None),
        FlagMode::Checks => process_raw_with_reason(raw, source),
        FlagMode::Review => {
            let obj = repair(raw, source);
            let reason = flag::review_reason(obj.as_ref(), source);
            (obj, reason)
        }
    }
}

/// Like [`process_raw`] but also returns the failing check reason (`None` if it passes) — used
/// to measure per-detector precision during the divergence sweep.
pub fn process_raw_with_reason(raw: &str, source: &str) -> (Option<Value>, Option<&'static str>) {
    let (obj, _) = process_raw(raw, source);
    let reason = checks::fails(source, obj.as_ref());
    (obj, reason)
}
