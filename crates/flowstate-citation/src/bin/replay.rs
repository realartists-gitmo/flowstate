//! Offline replay of the definitive review path from dumped raw decodes (see `dumpraw.rs`). Applies
//! the current deterministic repair + `review_reason` + perturbation-decorrelation logic to the
//! captured raws and scores against gold — reproducing `review_run` with ZERO model cost, so repair
//! rules can be iterated in milliseconds. Rebuild + rerun after every library change.
//!
//! Usage: `replay <raws.jsonl> <labelled.jsonl> [--out scored.jsonl]`

use anyhow::Result;
use flowstate_citation::text::fold;
use flowstate_citation::flag::review_reason as flag_review_reason;
use flowstate_citation::{FlagMode, process_with_mode};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

fn surnameset(v: Option<&Value>) -> BTreeSet<String> {
    v.and_then(|o| o.get("authors"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("surname").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn gold_surnames(v: &Value) -> BTreeSet<String> {
    v.get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("surname").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (raws_path, gold_path) = (&args[1], &args[2]);
    let mut out_path = None;
    let mut i = 3;
    while i < args.len() {
        if args[i] == "--out" { out_path = Some(args[i + 1].clone()); i += 2; } else { i += 1; }
    }

    // gold by id
    let mut gold: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut gold_authors: BTreeMap<String, Value> = BTreeMap::new();
    for l in std::fs::read_to_string(gold_path)?.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(l)?;
        let tgt: Value = match &v["target"] {
            Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            other => other.clone(),
        };
        let id = v["id"].as_str().unwrap_or("").to_string();
        gold.insert(id.clone(), gold_surnames(&tgt));
        gold_authors.insert(id, tgt.get("authors").cloned().unwrap_or(Value::Null));
    }

    let (mut perfect, mut silent, mut good, mut over) = (0usize, 0, 0, 0);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut writer = out_path.map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));

    for l in std::fs::read_to_string(raws_path)?.lines().filter(|l| !l.trim().is_empty()) {
        let r: Value = serde_json::from_str(l)?;
        let id = r["id"].as_str().unwrap_or("").to_string();
        let input = r["input"].as_str().unwrap_or("");
        let raw = r["raw"].as_str().unwrap_or("");
        let orig_issue = r["orig_issue"].as_str();

        let (obj, _) = process_with_mode(raw, input, FlagMode::Passthrough);
        let base = surnameset(obj.as_ref());
        let mut reason: Option<String> = None;
        if let Some(iss) = orig_issue {
            reason = Some(iss.to_string());
        } else if let Some(rr) = flag_review_reason(obj.as_ref(), input) {
            reason = Some(rr.to_string());
        } else if r["has_perturb"].as_bool().unwrap_or(false) {
            let praw = r["praw"].as_str().unwrap_or("");
            let pissue = r["praw_issue"].as_str();
            if pissue != Some("decode_input_length_limit") {
                let (pobj, _) = process_with_mode(praw, input, FlagMode::Passthrough);
                let pset = surnameset(pobj.as_ref());
                let disagree = if pissue.is_some() {
                    pset.difference(&base).next().is_some()
                } else {
                    pset != base
                };
                if disagree {
                    reason = Some("decode_perturbation_disagreement".to_string());
                }
            }
        }

        let g = gold.get(&id).cloned().unwrap_or_default();
        let correct = base == g;
        let passed = reason.is_none();
        match (passed, correct) {
            (true, true) => perfect += 1,
            (true, false) => silent += 1,
            (false, false) => good += 1,
            (false, true) => over += 1,
        }
        if let Some(rr) = &reason {
            *reasons.entry(rr.clone()).or_default() += 1;
        }
        if let Some(wr) = writer.as_mut() {
            let authors = obj.as_ref().and_then(|v| v.get("authors")).cloned().unwrap_or(Value::Null);
            let rec = serde_json::json!({ "id": id, "passed": passed, "reason": reason, "correct": correct,
                "input": input,
                "model_surnames": base.iter().collect::<Vec<_>>(), "gold_surnames": g.iter().collect::<Vec<_>>(),
                "model_authors": authors, "gold_authors": gold_authors.get(&id),
                "final_obj": obj });
            writeln!(wr, "{}", serde_json::to_string(&rec)?)?;
        }
    }
    if let Some(mut wr) = writer { wr.flush()?; }

    let n = (perfect + silent + good + over).max(1);
    let pct = |x: usize| 100.0 * x as f64 / n as f64;
    println!("=== replay ({n} cites) ===");
    println!("perfect: {perfect} ({:.1}%)  SILENT: {silent} ({:.1}%)  good-flag: {good}  over-flag: {over} ({:.1}%)", pct(perfect), pct(silent), pct(over));
    println!("flag precision: {:.0}%  ({} flagged)", 100.0 * good as f64 / (good + over).max(1) as f64, good + over);
    println!("reasons: {reasons:?}");
    Ok(())
}
