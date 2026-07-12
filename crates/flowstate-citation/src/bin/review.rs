//! Bench the definitive review path (`flag::review_run`) live over a labelled set: single int8
//! decode + deterministic flag + input-perturbation decorrelation. Scores the four buckets on the
//! author-surname set:
//!   PASSED & correct  = perfect        PASSED & wrong  = SILENT (the target: zero)
//!   FLAGGED & wrong   = good flag       FLAGGED & correct = over-flag (loud cost)
//!
//! Usage: `review <model_dir> <labelled.jsonl> [--limit N] [--threads N] [--out F]`

use anyhow::Result;
use flowstate_citation::backend::Ct2Backend;
use flowstate_citation::flag::{review_batch, review_run};
use flowstate_citation::text::fold;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

fn surnameset(v: &Value) -> BTreeSet<String> {
    v.get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("surname").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: review <model_dir> <labelled.jsonl> [--limit N] [--threads N] [--out F]");
        std::process::exit(2);
    }
    let (model_dir, file) = (&args[1], &args[2]);
    let (mut limit, mut threads, mut out_path, mut bulk) = (usize::MAX, 12usize, None, false);
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => { limit = args[i + 1].parse()?; i += 2; }
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--out" => { out_path = Some(args[i + 1].clone()); i += 2; }
            "--bulk" => { bulk = true; i += 1; }
            _ => { i += 1; }
        }
    }

    let rows: Vec<(String, String, BTreeSet<String>)> = std::fs::read_to_string(file)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            // `target` may be a JSON string (encoded) or an inline object, depending on the set.
            let tgt: Value = match &v["target"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                other => other.clone(),
            };
            (v["id"].as_str().unwrap_or("").to_string(), v["input"].as_str().unwrap_or("").to_string(), surnameset(&tgt))
        })
        .collect();
    eprintln!("loaded {} cites; running definitive review path (int8 + deterministic + perturbation)…", rows.len());

    let mut backend = Ct2Backend::new(model_dir, threads)?;
    let (mut perfect, mut silent, mut good, mut over) = (0usize, 0, 0, 0);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut writer = out_path.map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));

    // Bulk staged-batch decode of every cite at once (production path: a doc's mass cites), else the
    // per-cite interactive path. Both yield identical per-cite outcomes; bulk is far higher throughput.
    let outcomes = if bulk {
        eprintln!("bulk staged-batch mode: batching all originals + survivor perturbations…");
        review_batch(&mut backend, &rows.iter().map(|(_, inp, _)| inp.clone()).collect::<Vec<_>>())?
    } else {
        rows.iter().enumerate().map(|(n, (_, input, _))| {
            if n % 50 == 0 { eprintln!("  {n}/{}", rows.len()); }
            review_run(&mut backend, input)
        }).collect::<Result<Vec<_>>>()?
    };

    for ((id, input, gold), o) in rows.iter().zip(&outcomes) {
        let model_s = o.value.as_ref().map(surnameset).unwrap_or_default();
        let correct = &model_s == gold;
        match (o.passed, correct) {
            (true, true) => perfect += 1,
            (true, false) => silent += 1,
            (false, false) => good += 1,
            (false, true) => over += 1,
        }
        if !o.passed {
            *reasons.entry(o.reason.unwrap_or("?").to_string()).or_default() += 1;
        }
        if let Some(w) = writer.as_mut() {
            let authors = o.value.as_ref().and_then(|v| v.get("authors")).cloned().unwrap_or(Value::Null);
            let rec = serde_json::json!({ "id": id, "passed": o.passed, "reason": o.reason, "correct": correct,
                "input": input,
                "model_surnames": model_s.iter().collect::<Vec<_>>(), "gold_surnames": gold.iter().collect::<Vec<_>>(),
                "model_authors": authors });
            writeln!(w, "{}", serde_json::to_string(&rec)?)?;
        }
    }
    if let Some(mut w) = writer { w.flush()?; }

    let n = rows.len().max(1);
    let pct = |x: usize| 100.0 * x as f64 / n as f64;
    println!("\n=== definitive review path over {} cites (author-set) ===", rows.len());
    println!("PASSED & correct (perfect):  {perfect} ({:.1}%)", pct(perfect));
    println!("PASSED & wrong  (SILENT):    {silent} ({:.1}%)", pct(silent));
    println!("FLAGGED & wrong (good):      {good}");
    println!("FLAGGED & correct(over):     {over} ({:.1}%)", pct(over));
    println!("total flagged: {} ({:.1}%) | precision {:.0}%", good + over, pct(good + over), 100.0 * good as f64 / (good + over).max(1) as f64);
    println!("reasons: {reasons:?}");
    Ok(())
}
