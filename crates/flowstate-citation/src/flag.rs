//! Review flagger — decides whether a repaired citation should go to a human instead of shipping.
//!
//! Replaces the old failure-mode/schema checker (26% flag precision). It reasons about *authors vs
//! the source* rather than output shape, on the premise that the residual silent errors are all
//! author fabrications or byline omissions. Two signals, both grounded in the source text:
//!
//! 1. **Ungrounded author** — a surname the source never mentions is a fabrication.
//! 2. **`et al.` undercount** — the source signals "and others" but the output found ≤1 author, so
//!    coauthors were dropped. (A raw byline name-scan over-flags wildly — every editor/quoted
//!    person in a bio is a clean `First Last` — so omission is flagged only from this hard signal.)
//!
//! Primary goal is zero silent author errors; over-flagging is the accepted, lesser cost.

use crate::cascade::{Decode, DecodeIssue, Decoder, Outcome, Tier};
use crate::gazetteer::{GIVEN_NAMES, SURNAMES};
use crate::snap;
use crate::text::{fold, sim};
use crate::{FlagMode, perturb, process_with_mode};
use serde_json::Value;
use std::collections::BTreeSet;

/// Return the review reason, or `None` to ship. Operates on the *repaired* citation.
pub fn review_reason(obj: Option<&Value>, source: &str) -> Option<&'static str> {
    let obj = obj?;
    // a citation the model marked unparseable is an explicit non-answer, not a silent error
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return None;
    }
    let words = snap::words(source);
    let src: Vec<String> = words.iter().map(|w| fold(w)).collect();
    let grounded = |s: &str| -> bool {
        let f = fold(s);
        f.chars().count() < 3 || src.iter().any(|x| *x == f || sim(&f, x) >= 0.88)
    };
    let model_surs: Vec<String> = obj
        .get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("surname").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // 1. fabrication: an extracted surname the source never contains
    if model_surs.iter().any(|s| !grounded(s)) {
        return Some("author_ungrounded");
    }

    // 1b. all-caps institutional header: a parsed cite whose source has essentially no lowercase
    // letters is a publication/org name (`CHAZEN GLOBAL INSIGHTS 2-16-17`), not an authored work —
    // any extracted author is the institution mis-read as a person. (A real byline always carries
    // lowercase: a title, a particle, "by", an affiliation.) Prefix already stripped for the count.
    if !model_surs.is_empty() {
        let body = source.trim_start_matches("parse citation:").trim_start();
        if body.chars().filter(|c| c.is_lowercase()).count() <= 2 {
            return Some("author_from_allcaps_source");
        }
    }

    // 2. role-person with no extracted author: a byline like `Judge Prost of the …` names a person
    // by title, which the model dropped to zero authors. Gated on a role word directly preceding a
    // capitalized name so a purely institutional no-author cite isn't flagged.
    if model_surs.is_empty() {
        const ROLE: &[&str] = &[
            "judge", "justice", "rep", "representative", "sen", "senator", "gov", "governor",
            "president", "professor", "prof", "dr", "ambassador", "secretary", "chairman", "mayor",
            "congressman", "congresswoman", "gen", "general", "col", "colonel", "admiral",
            "commissioner", "attorney", "chancellor", "director",
        ];
        let role_person = words.windows(2).any(|w| {
            ROLE.contains(&fold(&w[0]).as_str())
                && w[1].chars().next().is_some_and(char::is_uppercase)
                && w[1].chars().filter(|c| c.is_alphabetic()).count() >= 3
        });
        if role_person {
            return Some("role_person_no_author");
        }
    }

    // 3. named-chair over-extraction: an endowed professorship (`… is Peter F. Krogh Professor …`)
    // is not an author. Signature: an extracted surname is immediately followed by a title word and
    // an `is`/`the` sits just before it — distinguishing the endowment from a real professor's title
    // (`Gregory Klass Professor of Law`, which has no such marker).
    const TITLE: &[&str] = &["professor", "professorship", "chair", "chaired", "fellowship"];
    for s in &model_surs {
        for i in 0..src.len() {
            if &src[i] == s && src.get(i + 1).is_some_and(|nx| TITLE.contains(&nx.as_str())) {
                let lo = i.saturating_sub(4);
                if src[lo..i].iter().any(|t| t == "is" || t == "the") {
                    return Some("named_chair_over_extraction");
                }
            }
        }
    }

    // 2. omission: `et al.` in the *cite key* (before the year) promises coauthors beyond the named
    // one(s); ≤1 extracted means the list was dropped. Restricting to the key avoids an `et al.` in
    // a title/quotation, and the low-count bound avoids flagging a genuine multi-author extraction.
    let key = source.trim_start_matches("parse citation:");
    let year_at = key
        .char_indices()
        .find(|(i, c)| c.is_ascii_digit() && !key[i + c.len_utf8()..].chars().next().is_some_and(char::is_alphabetic))
        .map_or(key.len(), |(i, _)| i);
    let head = key[..year_at].to_lowercase();
    if (head.contains("et al") || head.contains("et. al") || head.contains("et.al")) && model_surs.len() <= 1 {
        // A single credited author whose source contains no *other* person-name is a genuine
        // single-author "et al" (the coauthors are uncredited / never enumerated — `McKeown et al
        // 2002`, `Bunn et al 02 [bio of Bunn]`), so gold credits only the lead: don't flag. The name
        // scan is high-recall by design — a false positive only *keeps* the flag, never turns a real
        // omission silent. `model_surs` empty (missed even the lead) always flags.
        let single_credited = model_surs.len() == 1 && !has_other_author_name(source, &model_surs);
        if !single_credited {
            return Some("et_al_undercount");
        }
    }

    // 2b. structural undercount for a multi-author extraction: an `et al.` byline that enumerates its
    // authors with a strong per-author marker — `First [M.] Last, PhD` or `First Last is a/the <role>`
    // — but yields FEWER extracted authors than distinct markers dropped one. These markers only
    // appear in an explicit author roster (a single author's bio has one such clause, not several), so
    // requiring ≥2 markers strictly exceeding the extracted count keeps this off ordinary bios.
    if (head.contains("et al") || head.contains("et. al") || head.contains("et.al")) && model_surs.len() >= 2 {
        let signals = byline_author_markers(&words);
        if signals >= 2 && signals > model_surs.len() {
            return Some("et_al_undercount");
        }
    }

    None
}

/// Count strong per-author roster markers in a byline: a name immediately followed by an academic
/// degree (`Hecht, PhD`) or a `First Last is a/the <role>` bio clause. These mark an *enumerated*
/// author list; an ordinary single-author bio carries at most one. Used only to detect an `et al.`
/// undercount, so it errs toward missing rather than over-counting.
fn byline_author_markers(words: &[String]) -> usize {
    const DEG: &[&str] = &["phd", "ma", "jd", "md", "edd", "ms", "mba", "llm", "dphil", "mph", "mpa", "msc", "bsc", "mbbs"];
    const ROLE: &[&str] = &[
        "senior", "resident", "assistant", "associate", "adjunct", "distinguished", "research",
        "visiting", "professor", "fellow", "director", "president", "chair", "scholar", "lecturer",
        "scientist", "partner", "correspondent", "reporter", "chairman", "commissioner",
    ];
    let is_name = |t: &str| t.chars().next().is_some_and(char::is_uppercase) && t.chars().filter(|c| c.is_alphabetic()).count() >= 2;
    let mut n = 0;
    for i in 0..words.len() {
        // `Name, PhD` (the comma is stripped by `words`, so the degree simply follows the surname).
        if is_name(&words[i]) && words.get(i + 1).is_some_and(|w| DEG.contains(&fold(w).as_str())) {
            n += 1;
            continue;
        }
        // `First Last is [a|the|an] <role>` — a bio clause introducing an author.
        if i + 3 < words.len() && is_name(&words[i]) && is_name(&words[i + 1]) && fold(&words[i + 2]) == "is" {
            let after: Vec<String> = words[i + 3..(i + 6).min(words.len())].iter().map(|w| fold(w)).collect();
            if after.iter().any(|w| ROLE.contains(&w.as_str())) {
                n += 1;
            }
        }
    }
    n
}

/// Scan for a `First [M.] Last` person-name in the source whose given name is a known forename and
/// whose surname is a known (non-credited) surname — i.e. *another* author beyond the credited lead.
/// Used to cancel the `et al.` flag only when no such name exists (a genuine single-author cite).
/// The frequency-filtered gazetteers reject Title-Case citation text (`Precision Medicines`,
/// `Harvard Business`) that heuristics mistook for names. A cancelled et-al flag still falls through
/// to the perturbation decode, which backstops any coauthor this misses — so it errs safe.
fn has_other_author_name(source: &str, credited: &[String]) -> bool {
    let toks: Vec<&str> = source
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '\''))
        .filter(|t| !t.is_empty())
        .collect();
    let is_name = |t: &str| t.chars().next().is_some_and(char::is_uppercase) && t.chars().filter(|c| c.is_alphabetic()).count() >= 2;
    let is_initial = |t: &str| t.chars().next().is_some_and(char::is_uppercase) && t.trim_end_matches('.').chars().count() == 1;
    for i in 0..toks.len() {
        if !is_name(toks[i]) || !GIVEN_NAMES.contains(fold(toks[i]).as_str()) {
            continue;
        }
        let mut j = i + 1;
        while j < toks.len() && is_initial(toks[j]) {
            j += 1;
        }
        if j >= toks.len() || !is_name(toks[j]) {
            continue;
        }
        let surname = fold(toks[j]);
        if surname.chars().count() >= 2 && !credited.contains(&surname) && SURNAMES.contains(surname.as_str()) {
            return true; // known forename + known non-credited surname → another author
        }
    }
    false
}

/// Folded author-surname set of a repaired citation — the identity a decorrelation check compares.
fn author_surnames(obj: Option<&Value>) -> BTreeSet<String> {
    obj.and_then(|o| o.get("authors"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("surname").and_then(Value::as_str))
                .map(fold)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn outcome(value: Option<Value>, reason: Option<&'static str>) -> Outcome {
    Outcome { value, passed: reason.is_none(), tier: Some(Tier::Int8), snapped: false, reason }
}

/// The definitive review path: decode, repair, and decide review. Layers three signals, cheapest
/// first: truncation (output hit the ceiling), the deterministic [`review_reason`] (ungrounded
/// author / `et al.` undercount), and — the model-coupled part — **input-perturbation
/// decorrelation**: decode meaning-preserving perturbations (`canon`, `dropurl`) and flag if any
/// disagrees with the original on the author-surname set. The perturbations re-attend to the byline,
/// so they expose shaky omissions and over-extractions a single greedy decode hides. `head55`
/// (truncating perturbation) is excluded — it manufactures false disagreements.
pub fn review_run<D: Decoder>(decoder: &mut D, input: &str) -> anyhow::Result<Outcome> {
    let decoded = decoder.decode_with_metadata(input, Tier::Int8)?;
    let (obj, _) = process_with_mode(&decoded.raw, input, FlagMode::Passthrough);
    if let Some(issue) = decoded.issue {
        return Ok(outcome(obj, Some(issue.reason())));
    }
    if let Some(reason) = review_reason(obj.as_ref(), input) {
        return Ok(outcome(obj, Some(reason)));
    }
    let base = author_surnames(obj.as_ref());
    // One decorrelation probe: the merged non-destructive surface perturbation (canon + dropurl +
    // redelimit composed into a single decode). Field-ablation was dropped — it uniquely caught only
    // the all-caps institution case (now handled deterministically above) at 9% precision and the
    // cost of a second decode per cite.
    let Some(merged) = perturb::merged(input) else {
        return Ok(outcome(obj, None));
    };
    let variants = [merged];
    // decode every perturbation in one batch — with model replicas the engine runs them
    // concurrently. We only read the author fields (early in the JSON), so cap the decode short
    // (the vast majority finish well under this; it trims the trailing title/qualifications tokens
    // and bounds runaway decodes).
    let inputs: Vec<String> = variants.iter().map(|v| v.input.clone()).collect();
    for pd in decoder.decode_batch(&inputs, Tier::Int8, PERTURB_MAX_TOKENS)? {
        if matches!(pd.issue, Some(DecodeIssue::InputLengthLimit)) {
            continue; // oversized input — the decode is unreliable
        }
        let (pobj, _) = process_with_mode(&pd.raw, input, FlagMode::Passthrough);
        let pset = author_surnames(pobj.as_ref());
        // A cap-truncated decode (OutputLengthLimit) may have lost trailing authors, so a shortfall
        // is ambiguous — trust only the "found an author the original missed" direction. A complete
        // decode is compared in full.
        let disagree = if pd.issue.is_some() {
            pset.difference(&base).next().is_some()
        } else {
            pset != base
        };
        if disagree {
            return Ok(outcome(obj, Some("decode_perturbation_disagreement")));
        }
    }
    Ok(outcome(obj, None))
}

/// The perturbation output cap shared by [`review_run`] and [`review_batch`].
const PERTURB_MAX_TOKENS: usize = 512;

/// Bulk staged-batch review — the exact same three-signal path as [`review_run`], but batched across
/// many *unrelated* cites. For a document import that surfaces hundreds of cites at once, this
/// collapses 2N sequential decodes into two batched passes: one over the originals, one over the
/// survivors' perturbations. Per-cite results are identical to [`review_run`] (greedy int8 decoding
/// is batch-invariant), so it is a throughput swap, not a semantics change.
///
/// Stage 1 batch-decodes every original. Stage 2 repairs + runs the deterministic [`review_reason`]
/// (free — no model). Stage 3 batch-decodes the merged perturbation for every cite that survived
/// stage 2. Stage 4 compares author-surname sets and emits. Both decode passes are chunked by a
/// token budget so a handful of very long cites can't blow the batch's padded footprint.
pub fn review_batch<D: Decoder>(decoder: &mut D, inputs: &[String]) -> anyhow::Result<Vec<Outcome>> {
    // Stage 1: decode all originals in full (no output cap, as review_run's single decode).
    let raws = batched_decode(decoder, inputs, 0)?;

    // Stage 2: repair + deterministic flag; queue perturbation work for the survivors.
    let mut outcomes: Vec<Outcome> = Vec::with_capacity(inputs.len());
    let mut bases: Vec<BTreeSet<String>> = Vec::with_capacity(inputs.len());
    let mut pending: Vec<usize> = Vec::new(); // survivor indices needing a perturbation decode
    let mut pinputs: Vec<String> = Vec::new(); // aligned perturbation inputs
    for (i, dec) in raws.iter().enumerate() {
        let (obj, _) = process_with_mode(&dec.raw, &inputs[i], FlagMode::Passthrough);
        bases.push(author_surnames(obj.as_ref()));
        if let Some(issue) = dec.issue {
            outcomes.push(outcome(obj, Some(issue.reason())));
            continue;
        }
        if let Some(reason) = review_reason(obj.as_ref(), &inputs[i]) {
            outcomes.push(outcome(obj, Some(reason)));
            continue;
        }
        // survivor: ship unless its perturbation later disagrees. No possible perturbation → ship.
        if let Some(m) = perturb::merged(&inputs[i]) {
            pending.push(i);
            pinputs.push(m.input);
        }
        outcomes.push(outcome(obj, None));
    }

    // Stage 3: batch-decode the survivors' perturbations, capped short (author fields are early).
    let praws = batched_decode(decoder, &pinputs, PERTURB_MAX_TOKENS)?;

    // Stage 4: compare each survivor's perturbation to its original author-surname set.
    for (k, &i) in pending.iter().enumerate() {
        let pd = &praws[k];
        if matches!(pd.issue, Some(DecodeIssue::InputLengthLimit)) {
            continue; // oversized input — the perturbation decode is unreliable
        }
        let (pobj, _) = process_with_mode(&pd.raw, &inputs[i], FlagMode::Passthrough);
        let pset = author_surnames(pobj.as_ref());
        // Same directionality as review_run: a cap-truncated decode is trusted only when it *adds*
        // an author the original missed; a complete decode is compared in full.
        let disagree = if pd.issue.is_some() {
            pset.difference(&bases[i]).next().is_some()
        } else {
            pset != bases[i]
        };
        if disagree {
            let obj = outcomes[i].value.take();
            outcomes[i] = outcome(obj, Some("decode_perturbation_disagreement"));
        }
    }
    Ok(outcomes)
}

/// Decode `inputs` at int8, chunked so each batch's padded footprint (member count × longest member)
/// stays under a budget — a few very long cites get small batches, short cites pack densely. Inputs
/// are grouped by length to minimise padding waste; results are returned in `inputs` order.
/// `max_output` forwards to each batch (0 = model default).
fn batched_decode<D: Decoder>(decoder: &mut D, inputs: &[String], max_output: usize) -> anyhow::Result<Vec<Decode>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    const MAX_BATCH: usize = 48;
    const CHAR_BUDGET: usize = 24_576; // ceiling on batch_size × longest-member chars
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by_key(|&i| inputs[i].len());

    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut longest = 0usize;
    for &i in &order {
        let would_longest = longest.max(inputs[i].len().max(1));
        if !cur.is_empty() && (cur.len() >= MAX_BATCH || (cur.len() + 1) * would_longest > CHAR_BUDGET) {
            chunks.push(std::mem::take(&mut cur));
            longest = 0;
        }
        longest = longest.max(inputs[i].len().max(1));
        cur.push(i);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }

    let mut results: Vec<Option<Decode>> = (0..inputs.len()).map(|_| None).collect();
    for chunk in chunks {
        let batch: Vec<String> = chunk.iter().map(|&i| inputs[i].clone()).collect();
        for (&i, d) in chunk.iter().zip(decoder.decode_batch(&batch, Tier::Int8, max_output)?) {
            results[i] = Some(d);
        }
    }
    Ok(results.into_iter().map(Option::unwrap).collect())
}
