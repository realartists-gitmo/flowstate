//! Deterministic failure-mode checks. A `None` return means the output passes; a `Some`
//! carries the reason it must be repaired/escalated (or, if unrecoverable, sent to a human).
//!
//! A heuristic `under_enumeration` name-counter is deliberately absent: it scored 31% precision
//! because institutions look like people. Omission uncertainty is handled by the model controller's
//! cross-tier author consensus instead of by guessing an author count from capitalization.

use crate::text::{fold, ratio, tokens};
use serde_json::Value;

/// Bare function words that are never a family name (their presence means the real name was lost).
const CONNECTORS: &[&str] = &["and", "a", "the", "of", "for", "in", "an", "or", "to", "at", "on", "by"];

/// Spacing diacritics that only appear when the decoder mangled a precomposed accented letter
/// (`Clémençon` → `Cle´menc¸on`). A cleanly-extracted name never carries these standalone marks.
fn is_garbled(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c, '\u{00B4}' | '\u{00B8}' | '\u{02DC}' | '\u{00A8}' | '\u{02C6}' | '\u{00AF}')
    })
}

/// An immediately-repeated multi-char token is a greedy-decode artifact (`Smith Smith`,
/// `… menc on menc on`) — a real name never repeats a whole token.
fn has_repeated_token(s: &str) -> bool {
    let toks: Vec<String> =
        s.split_whitespace().map(fold).filter(|t| t.chars().count() >= 3).collect();
    toks.windows(2).any(|w| w[0] == w[1])
}

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
    if let Some(reason) = crate::schema::fails(obj) {
        return Some(reason);
    }
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return None; // schema-valid rejects are allowed through
    }
    let toks = tokens(source);
    let Some(authors) = obj.get("authors").and_then(Value::as_array) else {
        return None; // URL-only and other authorless parsed citations are valid sparse output
    };

    // loud-fail: a title that (post title-snap) still isn't grounded in the source is fabricated.
    if let Some(t) = obj.get("title").and_then(Value::as_str) {
        // a filename / URL / single long token is never a real title — the true title was lost.
        let lt = t.to_lowercase();
        if lt.contains("http") || lt.contains(".pdf") || lt.contains(".htm") || lt.contains(".doc")
            || t.contains("%20")
            || (t.split_whitespace().count() == 1 && t.chars().count() >= 20)
        {
            return Some("title_is_filename");
        }
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
        let surname = a.get("surname").and_then(Value::as_str);
        let name = a.get("name").and_then(Value::as_str);
        // loud-fail: a decode-mangled name (stray spacing diacritic, or a repeated token) is
        // corrupted beyond deterministic repair — send it to review rather than emit garbage.
        if surname.is_some_and(is_garbled)
            || name.is_some_and(|n| is_garbled(n) || has_repeated_token(n))
        {
            return Some("name_corrupted");
        }
        // loud-fail: a URL or a bare connector word is never a real surname — the model
        // mis-extracted and the real name is lost (`http://…` as author, `and`/`in`/`a`).
        if let Some(s) = surname {
            let ls = s.to_lowercase();
            if ls.contains("http") || s.contains("//") || ls.contains(".pdf") || ls.contains(".htm") {
                return Some("url_in_surname");
            }
            if CONNECTORS.contains(&fold(s).as_str()) {
                return Some("connector_as_surname");
            }
        }
        // NB: no surname==name check — for a mononym the full name IS the surname (valid).
        if let Some(s) = surname
            && !s.split_whitespace().all(|w| grounded(w, &toks))
        {
            return Some("surname_ungrounded");
        }
        if let Some(n) = name {
            let nw: Vec<&str> = n
                .split_whitespace()
                .filter(|w| fold(w).chars().count() >= 4)
                .collect();
            if !nw.is_empty() && !nw.iter().any(|w| grounded(w, &toks)) {
                return Some("name_ungrounded");
            }
        }
    }
    None
}
