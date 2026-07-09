//! Deterministic author-structure cleanups applied to every parsed output.

use crate::text::{fold, sim};
use serde_json::Value;

const SUFFIX: [&str; 6] = ["jr", "sr", "ii", "iii", "iv", "v"];

/// Junk tokens that leak into `given`: connectors, credentials, and debate speech tags.
/// A real given is never one of these, so they are dropped.
const JUNK_GIVEN: &[&str] = &[
    "and", "a", "the", "of", "for", "in", "an", "unknown", "emeritus", "professor", "prof", "dr",
    "mr", "mrs", "ms", "lecturer", "fellow", "senior", "associate", "assistant", "director",
    "1ac", "2ac", "1nc", "2nc", "1ar", "2ar", "1nr", "2nr", "cx", "parse",
];

/// Clean junk the model appended to a family: a credential/affiliation after `,`/`(`/`&`
/// (`Oberman,PhD`→`Oberman`, `Edelman(English`→`Edelman`, `Bernstein&DWOSKIN`→`Bernstein`);
/// a trailing year (`Schaefer13`→`Schaefer`); a generational suffix (`Moore IV`→`Moore`); and a
/// hyphenated affiliation whose tail is lowercase or a short acronym (`Boskin-prof`→`Boskin`,
/// `Oladipo-BBC`→`Oladipo`) — while keeping real hyphenated surnames (`Moore-Gilbert`).
fn clean_family(fam: &str) -> String {
    let mut f = fam;
    if let Some((head, _)) = f.split_once([',', '(', '&', '\u{2014}', '\u{2013}'])
        && head.trim().chars().count() >= 2
    {
        f = head.trim();
    }
    let mut out = f.to_string();
    // hyphen affiliation: drop a trailing "-tail" that is lowercase or a ≤4-char acronym
    if let Some((head, tail)) = out.clone().rsplit_once('-') {
        let tail_lower = tail.chars().all(|c| c.is_lowercase() || !c.is_alphabetic());
        let tail_acronym = tail.chars().all(|c| c.is_uppercase() || !c.is_alphabetic()) && tail.chars().count() <= 4;
        if head.chars().count() >= 2 && (tail_lower || tail_acronym) {
            out = head.to_string();
        }
    }
    let stripped = out.trim_end_matches(|c: char| c.is_ascii_digit() || c == '\'' || c == '\u{2019}');
    if stripped.chars().count() >= 2 {
        out = stripped.to_string();
    }
    let parts: Vec<&str> = out.split_whitespace().collect();
    if parts.len() > 1 && SUFFIX.contains(&fold(parts[parts.len() - 1]).as_str()) {
        out = parts[..parts.len() - 1].join(" ");
    }
    out
}

/// Drop phantom authors: a lone-family author (no given) whose family *is* a container string
/// (publisher / publication / `container_title` / database) — e.g. `Stanford` extracted as an
/// author when it is the institution. Conservative: only lone-family authors, family length ≥ 4,
/// and an exact/prefix/high-similarity match to a container field.
pub fn drop_phantom_authors(obj: &mut Value) {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return;
    }
    let containers: Vec<String> = ["publisher", "publication", "container_title", "database"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(fold)
        .filter(|s| !s.is_empty())
        .collect();
    if containers.is_empty() {
        return;
    }
    if let Some(authors) = obj.get_mut("authors").and_then(Value::as_array_mut) {
        authors.retain(|a| {
            let fam = a.get("family").and_then(Value::as_str).map(fold).unwrap_or_default();
            let has_given = a.get("given").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
            if fam.chars().count() < 4 || has_given {
                return true; // only target lone-family authors
            }
            !containers.iter().any(|c| c == &fam || c.starts_with(&fam) || sim(&fam, c) >= 0.9)
        });
    }
}

/// (1) Strip a trailing generational suffix from family (`Moore IV` → `Moore`);
/// (2) `family == given` is pure duplication (a mononym the model couldn't split) → drop given.
pub fn normalize_authors(obj: &mut Value) {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return;
    }
    let Some(authors) = obj.get_mut("authors").and_then(Value::as_array_mut) else {
        return;
    };
    for a in authors.iter_mut() {
        let Some(map) = a.as_object_mut() else { continue };
        if let Some(fam) = map.get("family").and_then(Value::as_str) {
            let cleaned = clean_family(fam);
            if cleaned != fam {
                map.insert("family".into(), Value::String(cleaned));
            }
        }
        // drop a junk given (connector / credential / debate tag)
        let junk_given = map
            .get("given")
            .and_then(Value::as_str)
            .is_some_and(|g| JUNK_GIVEN.contains(&fold(g).as_str()));
        if junk_given {
            map.remove("given");
        }
        let f = map.get("family").and_then(Value::as_str).map(fold);
        let g = map.get("given").and_then(Value::as_str).map(fold);
        if let (Some(f), Some(g)) = (f, g)
            && !f.is_empty()
            && f == g
        {
            map.remove("given");
        }
    }
    // drop exact-duplicate authors (model repetition), keeping the first occurrence
    let mut seen: Vec<(String, String)> = Vec::new();
    authors.retain(|a| {
        let key = (
            a.get("family").and_then(Value::as_str).map(fold).unwrap_or_default(),
            a.get("given").and_then(Value::as_str).map(fold).unwrap_or_default(),
        );
        if key.0.is_empty() && key.1.is_empty() {
            return true; // keep empty entries untouched
        }
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// Sibling-consistency: in an author list that is *mostly* bare surnames, a lone `given` is
/// probably the first half of a two-word surname the model wrongly split. If the concatenation
/// `"<given> <family>"` appears verbatim in the source, merge it into `family` (e.g. a
/// 3-judge list where `Scheffler Blaeser` was split into given `Scheffler` + family `Blaeser`).
/// Conservative: needs ≥3 authors, ≥2 bare surnames, a strict majority bare, and source grounding.
pub fn sibling_consistency(obj: &mut Value, source: &str) {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return;
    }
    let src = fold(source);
    let Some(authors) = obj.get_mut("authors").and_then(Value::as_array_mut) else {
        return;
    };
    if authors.len() < 3 {
        return;
    }
    let bare = authors
        .iter()
        .filter(|a| {
            a.get("given")
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty())
        })
        .count();
    if bare < 2 || bare * 2 <= authors.len() {
        return; // need a strong bare-surname majority
    }
    for a in authors.iter_mut() {
        let Some(map) = a.as_object_mut() else { continue };
        let giv = map.get("given").and_then(Value::as_str).map(str::to_string);
        let fam = map.get("family").and_then(Value::as_str).map(str::to_string);
        if let (Some(g), Some(f)) = (giv, fam) {
            if g.trim().is_empty() {
                continue;
            }
            let combined = format!("{g} {f}");
            if src.contains(&fold(&combined)) {
                map.insert("family".into(), Value::String(combined.clone()));
                map.insert("literal".into(), Value::String(combined));
                map.remove("given");
            }
        }
    }
}
