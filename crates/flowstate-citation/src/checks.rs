//! Deterministic failure-mode checks. A `None` return means the output passes; a `Some`
//! carries the reason it must be repaired/escalated (or, if unrecoverable, sent to a human).
//!
//! `under_enumeration` and `title_ungrounded` are deliberately absent: on gold they scored
//! 1/7 and 0/1 precision with zero recoveries, so they only manufactured false human-review.

use crate::text::{fold, ratio, tokens};
use serde_json::Value;

/// Bare function words that are never a family name (their presence means the real name was lost).
const CONNECTORS: &[&str] = &["and", "a", "the", "of", "for", "in", "an", "or", "to", "at", "on", "by"];

/// Is a name word present (fuzzily, diacritic-folded) in the source tokens?
pub fn grounded(word: &str, toks: &[String]) -> bool {
    let x = fold(word);
    if x.chars().count() < 3 {
        return true; // initials / short particles never flag
    }
    for t in toks {
        if t.contains(x.as_str()) || x.contains(t.as_str()) {
            return true;
        }
        if ratio(&x, t) >= 0.85 {
            return true;
        }
    }
    false
}

/// Return the first failing check, or `None` if the citation passes.
pub fn fails(source: &str, obj: Option<&Value>) -> Option<&'static str> {
    let obj = match obj {
        Some(o) => o,
        None => return Some("invalid_json"),
    };
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return None; // rejects/bare cites are allowed through
    }
    let toks = tokens(source);
    let authors = obj.get("authors").and_then(Value::as_array)?;

    // loud-fail: a title that (post title-snap) still isn't grounded in the source is fabricated.
    if let Some(t) = obj.get("title").and_then(Value::as_str) {
        let nt = fold(t);
        if nt.chars().count() >= 12 {
            let src = fold(source);
            let head: String = nt.chars().take(20).collect();
            if !src.contains(&head) {
                return Some("title_ungrounded");
            }
        }
    }
    // NB: two detectors were tried against the labels and dropped for low precision:
    //  - byline author-count (under-enumeration): 31% — institutions read as authors.
    //  - sparse-output (fragment the model should reject): 20% — legitimate bare cites look
    //    identical to fragments. The parse-vs-reject boundary can't be detected deterministically
    //    without breaking real bare cites, so it's left as a documented residual (model-side fix).
    for a in authors {
        let fam = a.get("family").and_then(Value::as_str);
        let giv = a.get("given").and_then(Value::as_str);
        // loud-fail: a URL or a bare connector word is never a real family name — the model
        // mis-extracted and the real name is lost (`http://…` as author, `and`/`in`/`a`).
        if let Some(f) = fam {
            let lf = f.to_lowercase();
            if lf.contains("http") || f.contains("//") || lf.contains(".pdf") || lf.contains(".htm") {
                return Some("url_in_name");
            }
            if CONNECTORS.contains(&fold(f).as_str()) {
                return Some("connector_as_name");
            }
        }
        if let (Some(f), Some(g)) = (fam, giv)
            && fold(f) == fold(g)
        {
            return Some("family_eq_given");
        }
        if let Some(f) = fam
            && !f.split_whitespace().all(|w| grounded(w, &toks))
        {
            return Some("name_ungrounded");
        }
        if let Some(g) = giv {
            let gw: Vec<&str> = g
                .split_whitespace()
                .filter(|w| fold(w).chars().count() >= 4)
                .collect();
            if !gw.is_empty() && !gw.iter().any(|w| grounded(w, &toks)) {
                return Some("given_ungrounded");
            }
        }
    }
    None
}
