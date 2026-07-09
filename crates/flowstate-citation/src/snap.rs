//! Snap-to-source: a mangled name is a corruption of a token present verbatim in the source
//! (the model's job is extraction). Snap each name field to the nearest source span,
//! positionally anchored on the author's sibling field. Fixes typos (`Wink`→`Wick`) and
//! restores characters T5 cannot emit (`ari`→`Šarić`). Applied only to flagged outputs.

use crate::text::{fold, sim, subseq};
use serde_json::Value;

const PUNCT: &str = ".,;:()[]{}\"“”‘’";

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| PUNCT.contains(c)).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Source positions of a sibling field's tokens — fuzzy, so a corrupted anchor (`Toto`)
/// still locates its source token (`Tomo`) to anchor the family snap.
fn anchor_idxs(tokvals: &[String], folded: &[String]) -> Vec<usize> {
    let mut idxs = Vec::new();
    for tv in tokvals {
        if tv.chars().count() < 3 {
            continue;
        }
        let exact: Vec<usize> = folded
            .iter()
            .enumerate()
            .filter(|(_, x)| *x == tv)
            .map(|(j, _)| j)
            .collect();
        if !exact.is_empty() {
            idxs.extend(exact);
            continue;
        }
        let mut best = None;
        let mut bestr = 0.0f64;
        for (j, x) in folded.iter().enumerate() {
            let s = sim(tv, x);
            if s > bestr {
                bestr = s;
                best = Some(j);
            }
        }
        if let Some(j) = best
            && bestr >= 0.8
        {
            idxs.push(j);
        }
    }
    idxs
}

/// Snap a single word to the best source token, or `None` if nothing clears the gate.
fn snap_word(val: &str, anchor: &[usize], w: &[String], folded: &[String]) -> Option<String> {
    let nv = fold(val);
    if nv.chars().count() < 2 {
        return None;
    }
    // restore the richer source form: a source token that folds identically but whose original
    // differs carries characters the model dropped that folding can't recover — stroke letters
    // (`Ø`,`ł`,`þ`) that don't NFKD-decompose, and case (`DWOSKIN`→`Dwoskin`). Prefer a form with
    // non-ASCII (a real diacritic restoration) over a mere case change.
    let has_na = |s: &str| s.chars().any(|c| !c.is_ascii());
    let mut restore: Option<usize> = None;
    for (j, ff) in folded.iter().enumerate() {
        if *ff == nv && w[j] != val {
            let better = restore.is_none_or(|ri| {
                (has_na(&w[j]) && !has_na(&w[ri])) || w[j].chars().count() > w[ri].chars().count()
            });
            if better {
                restore = Some(j);
            }
        }
    }
    if let Some(j) = restore
        && has_na(&w[j])
    {
        return Some(w[j].clone()); // only auto-apply when it restores a real diacritic
    }
    // adjacency candidates: neighbours of a confirmed sibling anchor
    let mut adj: Vec<usize> = Vec::new();
    for &ai in anchor {
        for d in [1i64, -1, 2, -2] {
            let j = ai as i64 + d;
            if j >= 0 && (j as usize) < w.len() && !adj.contains(&(j as usize)) {
                adj.push(j as usize);
            }
        }
    }
    // adjacency pass: looser 0.72 gate (position justifies it) + subsequence match
    let mut best = None;
    let mut bestr = 0.0f64;
    for &j in &adj {
        let mut s = sim(&nv, &folded[j]);
        if nv.chars().count() >= 3
            && folded[j].chars().count() >= 3
            && (subseq(&nv, &folded[j]) || subseq(&folded[j], &nv))
        {
            s = s.max(0.85);
        }
        if s > bestr {
            bestr = s;
            best = Some(j);
        }
    }
    if let Some(j) = best
        && bestr >= 0.72
        && folded[j] != nv
    {
        return Some(w[j].clone());
    }
    // global pass: strict 0.80 gate
    let mut best = None;
    let mut bestr = 0.0f64;
    for (j, x) in folded.iter().enumerate() {
        let s = sim(&nv, x);
        if s > bestr {
            bestr = s;
            best = Some(j);
        }
    }
    if let Some(j) = best
        && bestr >= 0.8
        && folded[j] != nv
    {
        return Some(w[j].clone());
    }
    None
}

/// Title correction: if the model's title is not already grounded as a contiguous span in the
/// source, find the best-matching source word-window and replace it. Recovers truncated/
/// paraphrased titles. Runs only when the title isn't already a folded substring of the source.
/// Returns true if changed.
pub fn snap_title(obj: &mut Value, source: &str) -> bool {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return false;
    }
    let title = match obj.get("title").and_then(Value::as_str) {
        Some(t) if t.chars().count() >= 8 => t.to_string(),
        _ => return false,
    };
    let nt = fold(&title);
    if nt.is_empty() || fold(source).contains(&nt) {
        return false; // empty, or already grounded — leave it
    }
    let sw = words(source);
    let tw = title.split_whitespace().count().max(1);
    let mut best: Option<String> = None;
    let mut bestr = 0.0f64;
    for wn in [tw.saturating_sub(1).max(1), tw, tw + 1, tw + 2] {
        if wn == 0 || wn > sw.len() {
            continue;
        }
        for i in 0..=sw.len() - wn {
            let span = sw[i..i + wn].join(" ");
            let s = sim(&nt, &fold(&span));
            if s > bestr {
                bestr = s;
                best = Some(span);
            }
        }
    }
    if let Some(span) = best
        && bestr >= 0.85
        && fold(&span) != nt
    {
        obj["title"] = Value::String(span);
        return true;
    }
    false
}

/// Universal family correction, applied to *every* parsed output (not just flagged ones).
///
/// The `name_ungrounded` check treats a family as grounded when it fuzzily matches a source
/// token (ratio ≥ 0.85 / substring), so small typos and dropped diacritics never trip a flag
/// and never reach `snap_authors` — e.g. `Routl`→`Routel`, `berg`→`Öberg`, `Hohroth`→`Hochroth`.
/// This snaps each family to its best source token regardless. It is safe because a *correct*
/// family appears verbatim in the source, so its exact token (similarity 1.0) always wins and
/// `snap_word` leaves it untouched; only non-verbatim families (corruptions) are replaced.
pub fn snap_families(obj: &mut Value, source: &str) -> usize {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return 0;
    }
    let w = words(source);
    let folded: Vec<String> = w.iter().map(|x| fold(x)).collect();
    let mut changes = 0;
    let Some(authors) = obj.get_mut("authors").and_then(Value::as_array_mut) else {
        return 0;
    };
    for a in authors.iter_mut() {
        let Some(map) = a.as_object_mut() else { continue };
        let giv = map.get("given").and_then(Value::as_str).unwrap_or("").to_string();
        let gtok: Vec<String> = giv.split_whitespace().map(fold).filter(|x| x.chars().count() >= 3).collect();
        let ganch = anchor_idxs(&gtok, &folded);
        if let Some(fam) = map.get("family").and_then(Value::as_str).map(str::to_string) {
            let mut parts = Vec::new();
            let mut ch = false;
            for word in fam.split_whitespace() {
                match snap_word(word, &ganch, &w, &folded) {
                    Some(nw) => {
                        parts.push(nw);
                        ch = true;
                    }
                    None => parts.push(word.to_string()),
                }
            }
            let nf = parts.join(" ");
            if ch && nf != fam {
                map.insert("family".into(), Value::String(nf));
                changes += 1;
            }
        }
    }
    changes
}

/// Universal given correction, mirror of [`snap_families`] for the `given` field (anchored on
/// the family). Fixes given-name typos-of-source; a correct given is verbatim so its exact
/// token wins and it is left untouched. Truly ungrounded givens are not snapped here — they
/// remain for the `given_ungrounded` check to loud-fail.
pub fn snap_givens(obj: &mut Value, source: &str) -> usize {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return 0;
    }
    let w = words(source);
    let folded: Vec<String> = w.iter().map(|x| fold(x)).collect();
    let mut changes = 0;
    let Some(authors) = obj.get_mut("authors").and_then(Value::as_array_mut) else {
        return 0;
    };
    for a in authors.iter_mut() {
        let Some(map) = a.as_object_mut() else { continue };
        let fam = map.get("family").and_then(Value::as_str).unwrap_or("").to_string();
        let ftok: Vec<String> = fam.split_whitespace().map(fold).filter(|x| x.chars().count() >= 3).collect();
        let fanch = anchor_idxs(&ftok, &folded);
        if let Some(giv) = map.get("given").and_then(Value::as_str).map(str::to_string) {
            let mut parts = Vec::new();
            let mut ch = false;
            for word in giv.split_whitespace() {
                match snap_word(word, &fanch, &w, &folded) {
                    Some(nw) => {
                        parts.push(nw);
                        ch = true;
                    }
                    None => parts.push(word.to_string()),
                }
            }
            let ng = parts.join(" ");
            if ch && ng != giv {
                map.insert("given".into(), Value::String(ng));
                changes += 1;
            }
        }
    }
    changes
}

/// Snap all author name fields toward the source. Returns the number of fields changed.
pub fn snap_authors(obj: &mut Value, source: &str) -> usize {
    if obj.get("status").and_then(Value::as_str) != Some("parsed") {
        return 0;
    }
    let w = words(source);
    let folded: Vec<String> = w.iter().map(|x| fold(x)).collect();
    let mut changes = 0;
    let Some(authors) = obj.get_mut("authors").and_then(Value::as_array_mut) else {
        return 0;
    };
    for a in authors.iter_mut() {
        let Some(map) = a.as_object_mut() else { continue };
        // family, anchored on the given
        let giv = map.get("given").and_then(Value::as_str).unwrap_or("").to_string();
        let gtok: Vec<String> = giv.split_whitespace().map(fold).filter(|x| x.chars().count() >= 3).collect();
        let ganch = anchor_idxs(&gtok, &folded);
        if let Some(fam) = map.get("family").and_then(Value::as_str).map(str::to_string) {
            let mut parts = Vec::new();
            let mut ch = false;
            for word in fam.split_whitespace() {
                match snap_word(word, &ganch, &w, &folded) {
                    Some(nw) => {
                        parts.push(nw);
                        ch = true;
                    }
                    None => parts.push(word.to_string()),
                }
            }
            let nf = parts.join(" ");
            if ch && nf != fam {
                map.insert("family".into(), Value::String(nf));
                changes += 1;
            }
        }
        // given, anchored on the (corrected) family
        let fam2 = map.get("family").and_then(Value::as_str).unwrap_or("").to_string();
        let ftok: Vec<String> = fam2.split_whitespace().map(fold).filter(|x| x.chars().count() >= 3).collect();
        let fanch = anchor_idxs(&ftok, &folded);
        if let Some(g) = map.get("given").and_then(Value::as_str).map(str::to_string) {
            let mut parts = Vec::new();
            let mut ch = false;
            for word in g.split_whitespace() {
                match snap_word(word, &fanch, &w, &folded) {
                    Some(nw) => {
                        parts.push(nw);
                        ch = true;
                    }
                    None => parts.push(word.to_string()),
                }
            }
            let ng = parts.join(" ");
            if ch && ng != g {
                map.insert("given".into(), Value::String(ng));
                changes += 1;
            }
        }
    }
    changes
}
