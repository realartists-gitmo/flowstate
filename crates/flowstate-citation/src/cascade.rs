//! Precision cascade and deterministic-repair controller.
//!
//! A syntactically valid, grounded subset of an author list can still be an omission. The
//! controller therefore requires two independently configured, schema-valid decodes to agree on
//! status and author identities before it reports success. Any valid disagreement is a loud
//! failure with the best-grounded candidate retained for review.

use crate::checks::{fails, grounded};
use crate::normalize::{drop_phantom_authors, normalize_authors};
use crate::reconstruct::to_json;
use crate::snap::snap_authors;
use crate::text::{fold, tokens};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Int8,
    Int16,
    Float32,
    Beam,
    BestOf,
}

/// A bounded decoder condition that makes its text incomplete even if deterministic bracket
/// repair can turn that text into parseable JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeIssue {
    InputLengthLimit,
    OutputLengthLimit,
}

impl DecodeIssue {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::InputLengthLimit => "decode_input_length_limit",
            Self::OutputLengthLimit => "decode_output_length_limit",
        }
    }
}

/// Raw decoder text plus any completeness issue observed by the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decode {
    pub raw: String,
    pub issue: Option<DecodeIssue>,
}

impl Decode {
    /// Construct a decode known to have terminated normally (usually by emitting EOS).
    pub fn complete(raw: impl Into<String>) -> Self {
        Self { raw: raw.into(), issue: None }
    }

    /// Construct a decode that reached a model boundary and must never be accepted as complete.
    pub fn incomplete(raw: impl Into<String>, issue: DecodeIssue) -> Self {
        Self { raw: raw.into(), issue: Some(issue) }
    }
}

/// A model backend. `decode` returns raw (brace-free) model output for one input at a tier.
pub trait Decoder {
    fn decode(&mut self, input: &str, tier: Tier) -> anyhow::Result<String>;

    /// Decode with termination metadata. Bounded backends should override this so a length-capped
    /// prefix cannot be repaired into apparently valid subset JSON. The compatibility default is
    /// for custom decoders that are known to return complete text.
    fn decode_with_metadata(&mut self, input: &str, tier: Tier) -> anyhow::Result<Decode> {
        self.decode(input, tier).map(Decode::complete)
    }

    /// Decode several inputs at one tier, results aligned with `inputs`. Backends should override
    /// this to run the inputs as a single engine batch (the review path decodes a cite's
    /// perturbations together, so this collapses their wall-clock from sum → one batched decode).
    /// `max_output` caps the decode length (0 = the model default); the review path caps its
    /// perturbations short since it only reads the early author fields. The default is sequential.
    fn decode_batch(&mut self, inputs: &[String], tier: Tier, _max_output: usize) -> anyhow::Result<Vec<Decode>> {
        inputs.iter().map(|i| self.decode_with_metadata(i, tier)).collect()
    }
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

/// Deterministic post-processing of one raw decode. Returns `(candidate, passed, snapped)`.
/// On failure the snapped candidate is returned because it is the better-grounded review aid.
pub fn resolve(raw: &str, source: &str) -> (Option<Value>, bool, bool) {
    let mut obj = to_json(raw);
    if let Some(o) = obj.as_mut() {
        normalize_authors(o, source);
        crate::snap::snap_surnames(o, source);
        crate::snap::snap_surname_span(o, source);
        crate::snap::snap_cite_tag(o, source);
        crate::snap::snap_names(o, source);
        crate::snap::ground_names(o, source);
        crate::snap::recover_empty_author(o, source);
        crate::snap::recover_key_coauthors(o, source);
        crate::snap::recover_byline_coauthors(o, source);
        crate::snap::drop_fabricated_near_dups(o, source);
        crate::snap::snap_title(o, source);
        crate::normalize::strip_superscript_markers(o, source);
        drop_phantom_authors(o);
        normalize_authors(o, source);
    }
    if fails(source, obj.as_ref()).is_none() {
        return (obj, true, false);
    }
    if let Some(o) = obj.as_ref() {
        let mut snapped = o.clone();
        snap_authors(&mut snapped, source);
        normalize_authors(&mut snapped, source);
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
        for author in authors {
            let ok = ["surname", "name"].iter().all(|key| {
                author
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|text| {
                        !text.trim().is_empty()
                            && text.split_whitespace().all(|word| grounded(word, &toks))
                    })
            });
            if ok {
                score += 1;
            }
        }
    }
    score
}

#[derive(Debug)]
struct BestCandidate {
    value: Value,
    score: i64,
    tier: Tier,
    snapped: bool,
}

/// Track the best-grounded candidate seen so far. Ties retain the earlier, cheaper tier.
fn consider(
    best: &mut Option<BestCandidate>,
    obj: Option<&Value>,
    tier: Tier,
    snapped: bool,
    source: &str,
) {
    if let Some(value) = obj {
        let score = grounded_score(value, source);
        if best.as_ref().is_none_or(|candidate| score > candidate.score) {
            *best = Some(BestCandidate { value: value.clone(), score, tier, snapped });
        }
    }
}

fn identity_token(text: &str) -> String {
    let ascii_folded = fold(text);
    if ascii_folded.is_empty() {
        text.chars()
            .flat_map(char::to_lowercase)
            .filter(|character| character.is_alphanumeric())
            .collect()
    } else {
        ascii_folded
    }
}

fn canonical_name(name: &str) -> String {
    const HONORIFICS: &[&str] = &[
        "dr", "prof", "professor", "mr", "mrs", "ms", "mx", "sir", "maj", "lt", "col",
        "gen", "capt", "rev", "hon", "the",
    ];
    let words: Vec<&str> = name
        .split_whitespace()
        .filter(|word| !HONORIFICS.contains(&fold(word).as_str()))
        .collect();
    match words.as_slice() {
        [] => String::new(),
        [only] => identity_token(only),
        [first, .., last] => format!("{}:{}", identity_token(first), identity_token(last)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecodeIdentity {
    status: String,
    authors: Vec<String>,
}

impl DecodeIdentity {
    fn of(value: &Value) -> Self {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut authors: Vec<String> = value
            .get("authors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|author| {
                let surname = author
                    .get("surname")
                    .and_then(Value::as_str)
                    .map(identity_token)
                    .unwrap_or_default();
                let name = author
                    .get("name")
                    .and_then(Value::as_str)
                    .map(canonical_name)
                    .unwrap_or_default();
                format!("{surname}|{name}")
            })
            .collect();
        authors.sort_unstable();
        Self { status, authors }
    }

    fn disagreement_reason(&self, other: &Self) -> &'static str {
        if self.status != other.status {
            "decode_status_disagreement"
        } else {
            "decode_author_disagreement"
        }
    }
}

#[derive(Debug)]
struct ValidCandidate {
    identity: DecodeIdentity,
    value: Value,
    tier: Tier,
    snapped: bool,
}

#[derive(Debug)]
struct Attempt {
    value: Option<Value>,
    passed: bool,
    snapped: bool,
    reason: Option<&'static str>,
}

fn attempt<D: Decoder>(decoder: &mut D, input: &str, tier: Tier) -> anyhow::Result<Attempt> {
    let decoded = decoder.decode_with_metadata(input, tier)?;
    let (value, checks_passed, snapped) = resolve(&decoded.raw, input);
    let reason = decoded
        .issue
        .map(DecodeIssue::reason)
        .or_else(|| fails(input, value.as_ref()));
    Ok(Attempt {
        value,
        passed: decoded.issue.is_none() && checks_passed,
        snapped,
        reason,
    })
}

fn failed(best: Option<BestCandidate>, reason: &'static str) -> Outcome {
    match best {
        Some(candidate) => Outcome {
            value: Some(candidate.value),
            passed: false,
            tier: Some(candidate.tier),
            snapped: candidate.snapped,
            reason: Some(reason),
        },
        None => Outcome {
            value: None,
            passed: false,
            tier: None,
            snapped: false,
            reason: Some(reason),
        },
    }
}

/// Run the full cascade for one citation.
///
/// Success requires two schema-valid decodes with identical status and author identities. The
/// common case therefore performs int8 and float32 greedy decoding; beam/sampling are only reached
/// when one of those candidates fails ordinary checks. A valid disagreement is never majority-
/// voted away: it is the uncertainty signal that turns otherwise silent omission into review.
pub fn run<D: Decoder>(decoder: &mut D, input: &str) -> anyhow::Result<Outcome> {
    let mut best: Option<BestCandidate> = None;
    let mut first_valid: Option<ValidCandidate> = None;
    let mut last_reason = Some("invalid_json");
    let mut saw_output_limit = false;

    // int16 is omitted: the ruy CPU backend does not support it. Repeated BestOf entries are
    // independent samples from the same tier.
    let tiers = [
        Tier::Int8,
        Tier::Float32,
        Tier::Beam,
        Tier::BestOf,
        Tier::BestOf,
        Tier::BestOf,
        Tier::BestOf,
        Tier::BestOf,
    ];

    for tier in tiers {
        let current = attempt(decoder, input, tier)?;
        if current.reason == Some(DecodeIssue::InputLengthLimit.reason()) {
            return Ok(failed(best, DecodeIssue::InputLengthLimit.reason()));
        }
        saw_output_limit |= current.reason == Some(DecodeIssue::OutputLengthLimit.reason());
        if current.reason.is_some() {
            last_reason = current.reason;
        }
        consider(&mut best, current.value.as_ref(), tier, current.snapped, input);

        if !current.passed {
            continue;
        }
        let Some(value) = current.value else {
            continue;
        };
        let identity = DecodeIdentity::of(&value);
        if let Some(first) = first_valid.as_ref() {
            if first.identity != identity {
                let reason = first.identity.disagreement_reason(&identity);
                return Ok(failed(best, reason));
            }
            // The later candidate verifies the earlier one; retain the earlier candidate's
            // secondary fields because escalation can degrade otherwise-correct text.
            return Ok(Outcome {
                value: Some(first.value.clone()),
                passed: true,
                tier: Some(first.tier),
                snapped: first.snapped,
                reason: None,
            });
        }
        first_valid = Some(ValidCandidate {
            identity,
            value,
            tier,
            snapped: current.snapped,
        });
    }

    if first_valid.is_some() {
        return Ok(failed(best, "decode_no_consensus"));
    }
    let reason = if saw_output_limit {
        DecodeIssue::OutputLengthLimit.reason()
    } else {
        best.as_ref()
            .and_then(|candidate| fails(input, Some(&candidate.value)))
            .or(last_reason)
            .unwrap_or("invalid_json")
    };
    Ok(failed(best, reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedDecoder {
        decodes: VecDeque<Decode>,
        calls: usize,
    }

    impl ScriptedDecoder {
        fn new(decodes: impl IntoIterator<Item = Decode>) -> Self {
            Self { decodes: decodes.into_iter().collect(), calls: 0 }
        }
    }

    impl Decoder for ScriptedDecoder {
        fn decode(&mut self, _input: &str, _tier: Tier) -> anyhow::Result<String> {
            unreachable!("the cascade must request decode metadata")
        }

        fn decode_with_metadata(&mut self, _input: &str, _tier: Tier) -> anyhow::Result<Decode> {
            self.calls += 1;
            Ok(self.decodes.pop_front().expect("scripted decode available"))
        }
    }

    fn parsed(authors: &[(&str, &str)]) -> String {
        let authors = authors
            .iter()
            .map(|(surname, name)| format!(r#""surname":"{surname}","name":"{name}""#))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#""status":"parsed","authors":[{authors}],"year":2021,"source_type":"report""#
        )
    }

    #[test]
    fn accepts_only_after_two_valid_decodes_agree() {
        let raw = parsed(&[("Hargraves", "Drew Hargraves")]);
        let mut decoder = ScriptedDecoder::new([
            Decode::complete(raw.clone()),
            Decode::complete(raw),
        ]);
        let outcome = run(
            &mut decoder,
            "parse citation: Hargraves 2021 [Drew Hargraves]",
        )
        .expect("cascade succeeds");

        assert!(outcome.passed);
        assert_eq!(outcome.tier, Some(Tier::Int8));
        assert_eq!(decoder.calls, 2);
    }

    #[test]
    fn author_subset_disagreement_fails_loudly() {
        let mut decoder = ScriptedDecoder::new([
            Decode::complete(parsed(&[("Hargraves", "Drew Hargraves")])),
            Decode::complete(parsed(&[
                ("Hargraves", "Drew Hargraves"),
                ("Chess", "Timothy Chess"),
            ])),
        ]);
        let outcome = run(
            &mut decoder,
            "parse citation: Hargraves et al. 2021 [Drew Hargraves, Timothy Chess]",
        )
        .expect("cascade returns review outcome");

        assert!(!outcome.passed);
        assert_eq!(outcome.reason, Some("decode_author_disagreement"));
        assert_eq!(outcome.value.as_ref().and_then(|value| value["authors"].as_array()).map(Vec::len), Some(2));
        assert_eq!(decoder.calls, 2);
    }

    #[test]
    fn duplicate_surnames_still_preserve_author_count_in_consensus() {
        let mut decoder = ScriptedDecoder::new([
            Decode::complete(parsed(&[("Idso", "Craig Idso")])),
            Decode::complete(parsed(&[
                ("Idso", "Craig Idso"),
                ("Idso", "Sherwood Idso"),
            ])),
        ]);
        let outcome = run(
            &mut decoder,
            "parse citation: Idso et al. 2014 [Craig Idso and Sherwood Idso]",
        )
        .expect("cascade returns review outcome");

        assert!(!outcome.passed);
        assert_eq!(outcome.reason, Some("decode_author_disagreement"));
    }

    #[test]
    fn middle_initial_formatting_does_not_create_false_disagreement() {
        let mut decoder = ScriptedDecoder::new([
            Decode::complete(parsed(&[("Tobey", "William Tobey")])),
            Decode::complete(parsed(&[("Tobey", "William H. Tobey")])),
        ]);
        let outcome = run(
            &mut decoder,
            "parse citation: Tobey 2019 [William H. Tobey]",
        )
        .expect("cascade succeeds");

        assert!(outcome.passed);
        assert_eq!(decoder.calls, 2);
    }

    #[test]
    fn output_limit_never_passes_after_json_repair() {
        let capped = Decode::incomplete(
            parsed(&[("Hargraves", "Drew Hargraves")]),
            DecodeIssue::OutputLengthLimit,
        );
        let mut decoder = ScriptedDecoder::new(std::iter::repeat_n(capped, 8));
        let outcome = run(
            &mut decoder,
            "parse citation: Hargraves et al. 2021 [Drew Hargraves, Timothy Chess]",
        )
        .expect("cascade returns review outcome");

        assert!(!outcome.passed);
        assert_eq!(outcome.reason, Some("decode_output_length_limit"));
        assert_eq!(decoder.calls, 8);
    }

    #[test]
    fn input_limit_stops_before_wasting_escalation_decodes() {
        let mut decoder = ScriptedDecoder::new([Decode::incomplete(
            "",
            DecodeIssue::InputLengthLimit,
        )]);
        let outcome = run(&mut decoder, "parse citation: oversized")
            .expect("cascade returns review outcome");

        assert!(!outcome.passed);
        assert_eq!(outcome.reason, Some("decode_input_length_limit"));
        assert_eq!(decoder.calls, 1);
    }
}
