//! Meaning-preserving input perturbations for **decorrelated decoding**.
//!
//! A same-model second opinion (a higher precision tier, greedy) shares the model's *systematic*
//! errors: if the learned function drops the third author of a byline, int8 and float32 both drop
//! it and "agree" on the wrong answer. The omission is a property of the input's surface form, so
//! perturbing that surface — without changing which authors/status are present — breaks the
//! correlation: the perturbed decode re-attends and keeps the dropped author, and the disagreement
//! becomes a flag. This is the cheap alternative to the precision-consensus cascade.
//!
//! Each variant preserves author/status semantics but changes the token sequence the encoder sees.

use crate::text::fold;
use serde_json::Value;

/// The task prefix the model was fine-tuned with; perturbations act on the payload after it and
/// re-attach it so the input stays in-distribution.
const PREFIX: &str = "parse citation: ";

/// A labelled perturbation of a cite input (prefix included), ready to decode.
pub struct Variant {
    pub label: &'static str,
    pub input: String,
}

fn split_prefix(input: &str) -> (&str, &str) {
    match input.strip_prefix(PREFIX) {
        Some(body) => (PREFIX, body),
        None => ("", input),
    }
}

/// Keep the leading `frac` of `body` (by char), trimmed back to a word boundary. Author bylines
/// sit near the front of a fullcite, so a head slice preserves them while radically changing the
/// tail the encoder sees. Returns `None` when `body` is too short to slice safely.
fn head_slice(body: &str, frac: f64) -> Option<String> {
    let chars: Vec<char> = body.chars().collect();
    if chars.len() < 200 {
        return None; // short cites: slicing risks dropping the byline itself
    }
    let mut cut = (chars.len() as f64 * frac) as usize;
    while cut > 0 && !chars[cut - 1].is_whitespace() {
        cut -= 1; // back up to a word boundary so we don't split a token
    }
    if cut < 40 {
        return None;
    }
    Some(chars[..cut].iter().collect::<String>().trim_end().to_string())
}

/// Collapse whitespace runs and canonicalize smart quotes / dashes. The mildest perturbation:
/// same words, different byte/token sequence.
fn canonicalize(body: &str) -> String {
    let normalized: String = body
        .chars()
        .map(|c| match c {
            '\u{201C}' | '\u{201D}' | '\u{2033}' => '"',
            '\u{2018}' | '\u{2019}' | '\u{2032}' => '\'',
            '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            '\u{00A0}' => ' ',
            other => other,
        })
        .collect();
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop a trailing URL and everything after it (access dates, retrieval boilerplate). Authors,
/// title and publisher precede the link, so this preserves the scored fields while cutting the
/// tail. Returns `None` when there is no URL to drop.
fn drop_url_tail(body: &str) -> Option<String> {
    let pos = body.find("http").or_else(|| body.find("www."))?;
    let head = body[..pos].trim_end().trim_end_matches([',', ';', '-']);
    if head.chars().count() < 40 {
        return None;
    }
    Some(head.to_string())
}

/// Build every applicable perturbation of `input`. Callers decode each, then flag when the
/// author identity set (or status) disagrees with the original decode. Fewer than the full set
/// may be returned when a transform does not apply (e.g. no URL, or too short to slice).
pub fn variants(input: &str) -> Vec<Variant> {
    let (prefix, body) = split_prefix(input);
    let mut out = Vec::new();
    let mut push = |label: &'static str, payload: String| {
        // a transform that changed nothing is not a perturbation — skip it
        if payload != body && !payload.trim().is_empty() {
            out.push(Variant { label, input: format!("{prefix}{payload}") });
        }
    };
    if let Some(s) = head_slice(body, 0.55) {
        push("head55", s);
    }
    push("canon", canonicalize(body));
    if let Some(s) = drop_url_tail(body) {
        push("dropurl", s);
    }
    if let Some(v) = redelimit(input) {
        out.push(v);
    }
    if let Some(v) = merged(input) {
        out.push(v);
    }
    out
}

/// The distinct perturbation labels [`variants`] can emit, for per-transform reporting.
pub const LABELS: &[&str] = &["head55", "canon", "dropurl", "redelimit", "merged"];

/// The three non-destructive surface perturbations composed into one input — canonicalize, then
/// drop the URL tail, then re-delimit the byline. None of them delete an author (unlike ablation),
/// so they stack safely; decoding this once instead of three times is the per-cite latency win on a
/// thread-saturated CPU. `None` if the composition changed nothing.
pub fn merged(input: &str) -> Option<Variant> {
    let (prefix, body) = split_prefix(input);
    let mut b = canonicalize(body);
    if let Some(d) = drop_url_tail(&b) {
        b = d;
    }
    let recombined = format!("{prefix}{b}");
    let out = redelimit(&recombined).map_or(recombined, |v| v.input);
    if out == input {
        return None;
    }
    Some(Variant { label: "merged", input: out })
}

/// Re-delimit an under-segmented byline: insert a comma before a `First [M.] Last` name run that
/// begins right after an affiliation boundary — a closing `)` or an org word — restoring the missing
/// delimiter that made the model read a second author as part of the first's affiliation
/// (`… (IFANS) Won K. Paik …` → `… (IFANS), Won K. Paik …`). A second decode then re-segments the
/// byline, exposing the swallowed coauthor. `None` if no boundary is found.
pub fn redelimit(input: &str) -> Option<Variant> {
    const ORG: &[&str] = &[
        "institute", "university", "college", "school", "center", "centre", "department",
        "corporation", "foundation", "association", "committee", "council", "agency", "bank",
        "ministry", "bureau", "society", "faculty", "security", "affairs",
    ];
    let (prefix, body) = split_prefix(input);
    let toks: Vec<&str> = body.split_whitespace().collect();
    if toks.len() < 3 {
        return None;
    }
    fn clean(t: &str) -> &str {
        t.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '\'')
    }
    let is_cap = |t: &str| {
        let c = clean(t);
        c.chars().next().is_some_and(char::is_uppercase) && c.chars().filter(|x| x.is_alphabetic()).count() >= 2
    };
    let is_initial = |t: &str| {
        let c = clean(t);
        c.chars().next().is_some_and(char::is_uppercase) && c.trim_end_matches('.').chars().count() == 1
    };
    let is_org = |t: &str| ORG.contains(&fold(clean(t)).as_str());
    let mut s = String::from(toks[0]);
    let mut inserted = false;
    for i in 1..toks.len() {
        let prev = toks[i - 1];
        let already_delimited = prev.ends_with([',', ';', '|']);
        // (A) a `First [M.] Last` run begins at i, right after an affiliation boundary — separate the
        // new author from the preceding affiliation: `(IFANS) Won …` → `(IFANS), Won …`
        let starts_name = is_cap(toks[i]) && toks.get(i + 1).is_some_and(|n| is_initial(n) || is_cap(n));
        let boundary = prev.ends_with(')') || is_org(prev);
        let type_a = starts_name && boundary;
        // (B) an org-affiliation run begins at i, right after a `Initial Surname` — separate the
        // author from their own affiliation: `K. Paik Central Michigan University` →
        // `K. Paik, Central Michigan University`. The middle-initial guard keeps it off place bigrams.
        let org_ahead = (i..(i + 3).min(toks.len())).any(|k| is_org(toks[k]));
        let prev_is_surname = is_cap(prev) && i >= 2 && is_initial(toks[i - 2]);
        let type_b = is_cap(toks[i]) && org_ahead && prev_is_surname;
        if !already_delimited && (type_a || type_b) {
            s.push_str(", ");
            inserted = true;
        } else {
            s.push(' ');
        }
        s.push_str(toks[i]);
    }
    if !inserted {
        return None;
    }
    Some(Variant { label: "redelimit", input: format!("{prefix}{s}") })
}

/// Remove affiliation/org spans the model did *not* extract into a field — an `<Org>` phrase glued
/// to an un-extracted coauthor (`Won K. Paik Central Michigan University`). An org word pulls in up
/// to two capitalized words to its left (`Central Michigan University`), capped so it cannot swallow
/// the preceding author name. Complements field-ablation for interleaved bylines.
fn strip_org_spans(text: &str) -> String {
    const ORG: &[&str] = &[
        "institute", "university", "college", "school", "center", "centre", "department",
        "corporation", "foundation", "association", "committee", "council", "agency", "bank",
        "ministry", "bureau", "program", "society", "faculty",
    ];
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut keep = vec![true; toks.len()];
    let is_cap = |t: &str| t.chars().next().is_some_and(char::is_uppercase) && t.chars().filter(|c| c.is_alphabetic()).count() >= 2;
    for i in 0..toks.len() {
        if ORG.contains(&fold(toks[i]).as_str()) {
            keep[i] = false;
            let mut j = i;
            while j > 0 && i - j < 2 && is_cap(toks[j - 1]) {
                j -= 1;
                keep[j] = false;
            }
        }
    }
    toks.iter().zip(keep).filter_map(|(t, k)| k.then_some(*t)).collect::<Vec<_>>().join(" ")
}

/// Case-insensitive byte offset of `needle` in `hay`. Uses lowercased search only when it preserves
/// byte length (ASCII-ish, so offsets stay valid); otherwise falls back to an exact match.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (hl, nl) = (hay.to_lowercase(), needle.to_lowercase());
    if hl.len() == hay.len() && nl.len() == needle.len() {
        hl.find(&nl)
    } else {
        hay.find(needle)
    }
}

/// **Field-ablation** perturbation, driven by the first decode's own JSON: remove the input spans it
/// filed as non-author fields — title, publisher, url, date, and every author's qualifications /
/// affiliation — leaving a de-cluttered residue that is mostly cite-tag + byline. A second decode of
/// this focused text re-attends to the names, exposing interior omissions (an affiliation wedged
/// between two authors) and buried over-extractions (a name that occurred only inside a removed
/// qualification) that surface-only perturbations miss. `None` when there is nothing to strip.
pub fn ablated(input: &str, obj: &Value) -> Option<Variant> {
    let (prefix, body) = split_prefix(input);
    let mut strip: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        if s.chars().count() >= 5 {
            strip.push(s.to_string());
        }
    };
    for key in ["title", "publisher", "publication", "container_title", "database", "url", "published_date"] {
        if let Some(s) = obj.get(key).and_then(Value::as_str) {
            push(s);
        }
    }
    if let Some(authors) = obj.get("authors").and_then(Value::as_array) {
        for a in authors {
            if let Some(q) = a.get("qualifications").and_then(Value::as_array) {
                for x in q.iter().filter_map(Value::as_str) {
                    push(x);
                }
            }
        }
    }
    if strip.is_empty() {
        return None;
    }
    // remove the longest spans first so a short value can't consume part of a longer one
    strip.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut residue = body.to_string();
    let mut removed = false;
    for s in &strip {
        if let Some(pos) = find_ci(&residue, s) {
            let end = pos + s.len();
            if residue.is_char_boundary(pos) && residue.is_char_boundary(end) {
                residue.replace_range(pos..end, " ");
                removed = true;
            }
        }
    }
    if !removed {
        return None;
    }
    // also drop un-extracted affiliation phrases wedged against a dropped coauthor
    let residue = strip_org_spans(&residue);
    let residue = residue.split_whitespace().collect::<Vec<_>>().join(" ");
    if residue == body || residue.split_whitespace().count() < 2 {
        return None;
    }
    Some(Variant { label: "ablate", input: format!("{prefix}{residue}") })
}
