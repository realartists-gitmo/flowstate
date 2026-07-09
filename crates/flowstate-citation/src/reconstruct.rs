//! Brace-free → valid JSON reconstruction with two deterministic repairs.
//!
//! The model emits brace-free JSON (T5's vocab lacks `{`/`}`) and, for titles with inner
//! quotes, drops the escaping backslash (source curly quotes collapse to `"`). This module:
//!   1. `escape_content` — re-escapes content quotes using the fixed key grammar,
//!   2. `reconstruct`     — re-inserts the top-level and per-author braces,
//!   3. `balance_brackets`— drops spurious closers / appends missing ones.
//!
//! `to_json` tries the repair ladder until `serde_json` accepts an object.

use serde_json::Value;

/// Every valid key (top-level + author object). The grammar is fixed, so a `"` closes a
/// string value only when followed by `]`, end, or `,"<one-of-these>":`.
pub const KEYS: &[&str] = &[
    "accessed_date", "authors", "card_signatures", "container_title", "database",
    "debate_annotations", "doi", "evidence", "issue", "no_date", "pages", "publication",
    "published_date", "publisher", "raw_tail", "reject_reason", "source_type",
    "spillover_start_index", "spillover_start_text", "status", "title", "url", "volume",
    "warnings", "year", "family", "given", "literal", "qualifications",
];

/// Does `c[pos..]` match `,"<KEY>"\s*:` — i.e. a value-closing quote followed by the next key?
fn keycolon_at(c: &[char], pos: usize) -> bool {
    if pos + 1 >= c.len() || c[pos] != ',' || c[pos + 1] != '"' {
        return false;
    }
    let rest: String = c[pos + 2..].iter().collect();
    for &k in KEYS {
        if let Some(after) = rest.strip_prefix(k) {
            let ab: Vec<char> = after.chars().collect();
            if ab.first() == Some(&'"') {
                let mut i = 1;
                while i < ab.len() && ab[i].is_whitespace() {
                    i += 1;
                }
                if i < ab.len() && ab[i] == ':' {
                    return true;
                }
            }
        }
    }
    false
}

/// Is the quote at index `j` a *structural* close for the current string?
fn closes(c: &[char], j: usize, expect_key: bool, in_str_array: bool) -> bool {
    let n = c.len();
    if expect_key {
        return j + 1 < n && c[j + 1] == ':';
    }
    if in_str_array {
        if j + 1 >= n || c[j + 1] == ']' {
            return true;
        }
        return j + 2 < n && c[j + 1] == ',' && c[j + 2] == '"';
    }
    if j + 1 >= n || c[j + 1] == ']' {
        return true;
    }
    keycolon_at(c, j + 1)
}

/// Re-escape every non-structural `"` so an inner title quote can't terminate the string.
#[allow(clippy::many_single_char_names, reason = "JSON character-scanning state machine")]
pub fn escape_content(s: &str) -> String {
    let c: Vec<char> = s.trim().chars().collect();
    let n = c.len();
    let mut out = String::new();
    let mut i = 0usize;
    let mut expect_key = true;
    let mut arr: Vec<bool> = Vec::new(); // true = string-array, false = authors (object) array
    let mut last_key = String::new();
    while i < n {
        let ch = c[i];
        match ch {
            '"' => {
                let mut j = i + 1;
                let mut buf = String::new();
                while j < n {
                    let d = c[j];
                    if d == '\\' && j + 1 < n {
                        buf.push(d);
                        buf.push(c[j + 1]);
                        j += 2;
                        continue;
                    }
                    if d == '"' {
                        if closes(&c, j, expect_key, arr.last() == Some(&true)) {
                            break;
                        }
                        buf.push_str("\\\"");
                        j += 1;
                        continue;
                    }
                    buf.push(d);
                    j += 1;
                }
                out.push('"');
                out.push_str(&buf);
                out.push('"');
                if expect_key {
                    last_key = buf;
                }
                i = j + 1;
            }
            ':' => {
                expect_key = false;
                out.push(ch);
                i += 1;
            }
            ',' => {
                expect_key = arr.last() != Some(&true);
                out.push(ch);
                i += 1;
            }
            '[' => {
                let is_str = last_key != "authors";
                arr.push(is_str);
                expect_key = !is_str;
                out.push(ch);
                i += 1;
            }
            ']' => {
                arr.pop();
                expect_key = true;
                out.push(ch);
                i += 1;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// Drop unmatched closing brackets and append missing ones, respecting string state.
pub fn balance_brackets(s: &str) -> String {
    let mut out = String::new();
    let mut stack: Vec<char> = Vec::new();
    let mut instr = false;
    let mut esc = false;
    for ch in s.chars() {
        if instr {
            out.push(ch);
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                instr = false;
            }
            continue;
        }
        match ch {
            '"' => {
                instr = true;
                out.push(ch);
            }
            '{' | '[' => {
                stack.push(ch);
                out.push(ch);
            }
            '}' | ']' => {
                let want = if ch == '}' { '{' } else { '[' };
                if stack.last() == Some(&want) {
                    stack.pop();
                    out.push(ch);
                }
                // else: spurious closer, drop
            }
            _ => out.push(ch),
        }
    }
    while let Some(op) = stack.pop() {
        out.push(if op == '{' { '}' } else { ']' });
    }
    out
}

/// Index of the `]` matching the `[` at `o`, tracking quotes/escapes; -1 if none.
#[allow(clippy::many_single_char_names, reason = "bracket-matching character scan")]
fn array_end(c: &[char], o: usize) -> isize {
    let (mut d, mut q, mut e) = (0i32, false, false);
    let mut k = o;
    while k < c.len() {
        let ch = c[k];
        if e {
            e = false;
        } else if ch == '\\' {
            e = true;
        } else if ch == '"' {
            q = !q;
        } else if !q {
            if ch == '[' {
                d += 1;
            } else if ch == ']' {
                d -= 1;
                if d == 0 {
                    return k as isize;
                }
            }
        }
        k += 1;
    }
    -1
}

fn starts_with_family(c: &[char], pos: usize) -> bool {
    let pat: Vec<char> = "\"family\":".chars().collect();
    pos + pat.len() <= c.len() && c[pos..pos + pat.len()] == pat[..]
}

/// Split the authors-array inner text into per-author fragments at `,"family":` (depth 0).
fn split_authors(inner: &[char]) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let (mut st, mut d, mut q, mut e, mut k) = (0usize, 0i32, false, false, 0usize);
    while k < inner.len() {
        let ch = inner[k];
        if e {
            e = false;
        } else if ch == '\\' {
            e = true;
        } else if ch == '"' {
            q = !q;
        } else if !q {
            if ch == '[' {
                d += 1;
            } else if ch == ']' {
                d -= 1;
            } else if ch == ',' && d == 0 && starts_with_family(inner, k + 1) {
                parts.push(inner[st..k].iter().collect());
                st = k + 1;
            }
        }
        k += 1;
    }
    parts.push(inner[st..].iter().collect());
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

fn find_sub(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == needle[..])
}

/// Insert the top-level braces and per-author object braces around the brace-free body.
pub fn reconstruct(s: &str) -> String {
    let trimmed = s.trim().trim_start_matches('{').trim_end_matches('}').trim();
    let c: Vec<char> = trimmed.chars().collect();
    let key: Vec<char> = "\"authors\":[".chars().collect();
    if let Some(i) = find_sub(&c, &key) {
        let oi = i + key.len() - 1; // index of the '['
        let ei = array_end(&c, oi);
        if ei > 0 {
            let ei = ei as usize;
            let inner = &c[oi + 1..ei];
            let inner_str: String = if inner.iter().collect::<String>().trim().is_empty() {
                inner.iter().collect()
            } else {
                split_authors(inner)
                    .into_iter()
                    .map(|p| format!("{{{p}}}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let mut rebuilt: String = c[..=oi].iter().collect();
            rebuilt.push_str(&inner_str);
            rebuilt.extend(c[ei..].iter());
            return format!("{{{rebuilt}}}");
        }
    }
    format!("{{{trimmed}}}")
}

/// Parse a raw model output into a JSON object, trying the repair ladder in order.
///
/// T5's vocab lacks `{`/`}`, so the model emits `<unk>` where a brace belongs (Python's
/// `skip_special_tokens` dropped these; native decoders render them literally). Strip them to
/// recover the brace-free form the reconstructor expects.
pub fn to_json(raw: &str) -> Option<Value> {
    let cleaned = raw.replace("<unk>", "");
    let raw = cleaned.as_str();
    let esc = escape_content(raw);
    let candidates = [
        raw.to_string(),
        reconstruct(raw),
        reconstruct(&esc),
        balance_brackets(&reconstruct(&esc)),
        balance_brackets(&esc),
        esc,
    ];
    for cand in candidates {
        if let Ok(v) = serde_json::from_str::<Value>(&cand)
            && v.is_object()
        {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_inner_quotes() {
        let raw = r#""status":"parsed","authors":["family":"Whitehouse","given":"Tom"],"title":"Critical "Metals" and Cleantech","source_type":"web_page""#;
        let v = to_json(raw).expect("should parse");
        assert_eq!(v["title"], "Critical \"Metals\" and Cleantech");
        assert_eq!(v["authors"][0]["family"], "Whitehouse");
    }

    #[test]
    fn drops_spurious_bracket() {
        let raw = r#""status":"parsed","authors":["family":"Booth","given":"Ken"]],"year":2007"#;
        let v = to_json(raw).expect("should parse after bracket balance");
        assert_eq!(v["authors"][0]["family"], "Booth");
        assert_eq!(v["year"], 2007);
    }
}
