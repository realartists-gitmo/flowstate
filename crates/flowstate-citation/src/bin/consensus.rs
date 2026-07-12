//! Bench the deployed consensus cascade (`cascade::run`) over a labelled set.
//!
//! Unlike `diverge` (single int8 decode + deterministic layer), this exercises the real runtime:
//! two-tier consensus, escalation, schema validation, and truncation detection. Every cite lands
//! in one of four buckets, scored on the fields that matter (author surname-set + status):
//!   PASSED & correct  = perfect
//!   PASSED & wrong     = SILENT failure (the thing we are trying to eliminate)
//!   FLAGGED & wrong    = correctly sent to review
//!   FLAGGED & correct  = over-flag (the human-review cost of the consensus gate)
//!
//! Usage: `consensus <model_dir> <labelled.jsonl> [--limit N] [--threads N] [--out F]`

use anyhow::Result;
use flowstate_citation::backend::Ct2Backend;
use flowstate_citation::run;
use flowstate_citation::text::fold;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

fn surnameset(v: &Value) -> BTreeSet<String> {
    v.get("authors")
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

fn status(v: &Value) -> String {
    v.get("status").and_then(Value::as_str).unwrap_or("").to_string()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: consensus <model_dir> <labelled.jsonl> [--limit N] [--threads N] [--out F]");
        std::process::exit(2);
    }
    let model_dir = &args[1];
    let file = &args[2];
    let (mut limit, mut threads, mut out_path) = (usize::MAX, 16usize, None);
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => { limit = args[i + 1].parse()?; i += 2; }
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--out" => { out_path = Some(args[i + 1].clone()); i += 2; }
            _ => { i += 1; }
        }
    }

    let rows: Vec<(String, String, BTreeSet<String>, String)> = std::fs::read_to_string(file)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            let tgt: Value = serde_json::from_str(v["target"].as_str().unwrap_or("{}")).unwrap();
            (
                v["id"].as_str().unwrap_or("").to_string(),
                v["input"].as_str().unwrap_or("").to_string(),
                surnameset(&tgt),
                status(&tgt),
            )
        })
        .collect();
    eprintln!("loaded {} cites; running consensus cascade (int8+float32 …)…", rows.len());

    let mut backend = Ct2Backend::new(model_dir, threads)?;
    let (mut perfect, mut silent, mut correct_flag, mut over_flag) = (0usize, 0, 0, 0);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut writer = out_path.map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));

    for (n, (id, input, gold, gstatus)) in rows.iter().enumerate() {
        let outcome = run(&mut backend, input)?;
        let model_s = outcome.value.as_ref().map(surnameset).unwrap_or_default();
        let model_status = outcome.value.as_ref().map(status).unwrap_or_default();
        let correct = &model_s == gold && &model_status == gstatus;
        if outcome.passed {
            if correct { perfect += 1 } else { silent += 1 }
        } else {
            *reasons.entry(outcome.reason.unwrap_or("?").to_string()).or_default() += 1;
            if correct { over_flag += 1 } else { correct_flag += 1 }
        }
        if let Some(w) = writer.as_mut() {
            let rec = serde_json::json!({
                "id": id, "passed": outcome.passed, "reason": outcome.reason, "correct": correct,
                "model_surnames": model_s.iter().collect::<Vec<_>>(),
                "gold_surnames": gold.iter().collect::<Vec<_>>(),
            });
            writeln!(w, "{}", serde_json::to_string(&rec)?)?;
        }
        if n % 50 == 0 { eprintln!("  {n}/{}", rows.len()); }
    }
    if let Some(mut w) = writer { w.flush()?; }

    let total = rows.len().max(1);
    let pct = |x: usize| 100.0 * x as f64 / total as f64;
    println!("\n=== consensus cascade over {} cites (author-set + status vs gold) ===", rows.len());
    println!("PASSED & correct  (perfect):        {perfect} ({:.1}%)", pct(perfect));
    println!("PASSED & wrong    (SILENT):         {silent} ({:.1}%)", pct(silent));
    println!("FLAGGED & wrong   (correct flag):   {correct_flag}");
    println!("FLAGGED & correct (over-flag cost): {over_flag}");
    println!("total flagged for review:           {} ({:.1}%)", correct_flag + over_flag, pct(correct_flag + over_flag));
    println!("\nflag reasons: {reasons:?}");
    Ok(())
}
