//! Cache-aware batched decoder for the labelled eval set (and, in principle, any cite corpus).
//!
//! The model decode is the only nondeterministic, expensive input to the repair layer; repair is a
//! pure function of `(raw, source)`. So we decode every distinct cite exactly ONCE and persist the
//! raw strings to a cache keyed by normalized input. Re-runs skip everything already cached and
//! decode only the remainder, appending to the cache — the run is fully resumable (kill and restart
//! and it continues from where it stopped).
//!
//! Throughput comes from batching: inputs are sorted by length and packed into batches bounded by a
//! token budget (not a fixed count), so padding waste stays low and long cites don't blow up memory
//! the way a naive fixed-size batch does. `FLOWSTATE_CT2_REPLICAS` adds model replicas on top.
//!
//! Usage: `citecache <model_dir> --cache <cache.jsonl> --todo <ids.jsonl> [--threads N] [--perturb]
//!         [--limit N] [--batch-tokens N] [--max-seqs N]`

use anyhow::Result;
use flowstate_citation::backend::Ct2Backend;
use flowstate_citation::cascade::{Decode, Decoder, Tier};
use flowstate_citation::perturb;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufWriter, Write};

const PERTURB_MAX_TOKENS: usize = 512;

fn norm(s: &str) -> String {
    s.replace("parse citation:", "").trim().to_string()
}

fn issue_str(d: &Decode) -> Option<&'static str> {
    d.issue.map(|i| i.reason())
}

/// Rough token estimate for batch packing (`T5` `SentencePiece` averages ~3.7 chars/token).
fn est_tokens(s: &str) -> usize {
    (s.len() / 4).max(1)
}

/// Pack `items` (already sorted by length) into batches bounded by a token budget and a sequence
/// cap, so each batch has bounded padding and bounded memory.
fn pack(items: &[(String, String)], batch_tokens: usize, max_seqs: usize) -> Vec<&[(String, String)]> {
    let mut batches = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let mut end = start;
        let mut budget = 0;
        while end < items.len() && end - start < max_seqs {
            let t = est_tokens(&items[end].1);
            if end > start && budget + t > batch_tokens {
                break;
            }
            budget += t;
            end += 1;
        }
        batches.push(&items[start..end]);
        start = end;
    }
    batches
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let model_dir = &args[1];
    let (mut cache_path, mut todo_path) = (None, None);
    let (mut threads, mut perturb_on, mut limit) = (12usize, false, usize::MAX);
    let (mut batch_tokens, mut max_seqs) = (6000usize, 64usize);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--cache" => { cache_path = Some(args[i + 1].clone()); i += 2; }
            "--todo" => { todo_path = Some(args[i + 1].clone()); i += 2; }
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--perturb" => { perturb_on = true; i += 1; }
            "--limit" => { limit = args[i + 1].parse()?; i += 2; }
            "--batch-tokens" => { batch_tokens = args[i + 1].parse()?; i += 2; }
            "--max-seqs" => { max_seqs = args[i + 1].parse()?; i += 2; }
            _ => { i += 1; }
        }
    }
    let cache_path = cache_path.expect("--cache required");
    let todo_path = todo_path.expect("--todo required");

    // Already-cached inputs (by normalized key) — skip these.
    let mut cached: HashSet<String> = HashSet::new();
    if let Ok(s) = std::fs::read_to_string(&cache_path) {
        for l in s.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(v) = serde_json::from_str::<Value>(l)
                && let Some(inp) = v.get("input").and_then(Value::as_str)
            {
                cached.insert(norm(inp));
            }
        }
    }
    eprintln!("cache holds {} decoded cites", cached.len());

    // Load the to-do list, drop anything already cached, dedup, sort by length.
    let mut todo: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for l in std::fs::read_to_string(&todo_path)?.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(l)?;
        let id = v["id"].as_str().unwrap_or("").to_string();
        let inp = v["input"].as_str().unwrap_or("").to_string();
        let key = norm(&inp);
        if key.is_empty() || cached.contains(&key) || !seen.insert(key) {
            continue;
        }
        todo.push((id, inp));
    }
    todo.sort_by_key(|(_, inp)| inp.len());
    todo.truncate(limit);
    eprintln!("decoding {} new cites (perturb={perturb_on})…", todo.len());
    if todo.is_empty() {
        return Ok(());
    }

    let mut backend = Ct2Backend::new(model_dir, threads)?;
    // Append mode: a killed run resumes; nothing already written is lost.
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&cache_path)?;
    let mut w = BufWriter::new(file);

    let batches = pack(&todo, batch_tokens, max_seqs);
    let total = todo.len();
    let mut done = 0usize;
    for batch in batches {
        let inputs: Vec<String> = batch.iter().map(|(_, inp)| inp.clone()).collect();
        let origs = backend.decode_batch(&inputs, Tier::Int8, 0)?;

        // Perturbations (optional): one capped decode per cite that has a perturbation.
        let mut praws: Vec<(String, Option<&'static str>, bool)> = vec![(String::new(), None, false); batch.len()];
        if perturb_on {
            let mut pinputs = Vec::new();
            let mut pidx = Vec::new();
            for (j, (_, inp)) in batch.iter().enumerate() {
                if let Some(m) = perturb::merged(inp) {
                    pidx.push(j);
                    pinputs.push(m.input);
                }
            }
            if !pinputs.is_empty() {
                let pd = backend.decode_batch(&pinputs, Tier::Int8, PERTURB_MAX_TOKENS)?;
                for (k, d) in pd.iter().enumerate() {
                    praws[pidx[k]] = (d.raw.clone(), issue_str(d), true);
                }
            }
        }

        for (j, (id, inp)) in batch.iter().enumerate() {
            let (praw, pissue, has_p) = &praws[j];
            let rec = serde_json::json!({
                "id": id, "input": inp,
                "raw": origs[j].raw, "orig_issue": issue_str(&origs[j]),
                "praw": praw, "praw_issue": pissue, "has_perturb": has_p,
            });
            writeln!(w, "{}", serde_json::to_string(&rec)?)?;
        }
        w.flush()?;
        done += batch.len();
        eprintln!("  {done}/{total}");
    }
    Ok(())
}
