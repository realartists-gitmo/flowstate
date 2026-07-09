//! Run the Rust pipeline over the labelled set and report every divergence from the labels,
//! so repairable cases can be tracked down exhaustively.
//!
//! Usage:
//!   `diverge <model_dir> <out.jsonl> <labelled.jsonl>... [--limit N] [--threads N] [--dump-raws F]`
//!   `diverge --from-raws <raws.jsonl> <out.jsonl>`              (re-analyse from cache, no model)
//!
//! int8 greedy is deterministic, so `--dump-raws` caches every raw decode; a later
//! `--from-raws` re-runs only the deterministic layer, instantly.

use anyhow::{Context, Result};
use flowstate_citation::process_raw_with_reason;
use flowstate_citation::text::fold;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Write;

struct Cite {
    id: String,
    input: String,
    raw: String,
    target: Value,
}

fn famset(v: &Value) -> BTreeSet<String> {
    v.get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("family").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn givenset(v: &Value) -> BTreeSet<String> {
    v.get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("given").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn fstr(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).map(fold).unwrap_or_default()
}

/// Apply the deterministic pipeline to every cached cite and write a divergence report.
fn analyze(cites: &[Cite], out_path: &str) -> Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(out_path)?);
    let (mut n_pass, mut n_human, mut n_div) = (0usize, 0usize, 0usize);
    let (mut d_fam, mut d_src, mut d_title, mut d_given, mut d_status) = (0usize, 0, 0, 0, 0);
    for c in cites {
        let (final_obj, reason) = process_raw_with_reason(&c.raw, &c.input);
        let passed = reason.is_none();
        if passed { n_pass += 1; } else { n_human += 1; }
        let empty = Value::Object(serde_json::Map::new());
        let fin = final_obj.as_ref().unwrap_or(&empty);
        let tgt = &c.target;
        let fam_diff = famset(fin) != famset(tgt);
        let src_diff = fstr(fin, "source_type") != fstr(tgt, "source_type");
        let title_diff = fstr(fin, "title") != fstr(tgt, "title");
        let given_diff = givenset(fin) != givenset(tgt);
        let status_diff = fstr(fin, "status") != fstr(tgt, "status");
        if fam_diff || src_diff || title_diff || given_diff || status_diff || !passed {
            n_div += 1;
            d_fam += fam_diff as usize;
            d_src += src_diff as usize;
            d_title += title_diff as usize;
            d_given += given_diff as usize;
            d_status += status_diff as usize;
            let rec = serde_json::json!({
                "id": c.id, "input": c.input.replace("parse citation: ", ""), "human_review": !passed,
                "reason": reason,
                "diffs": {"family_set": fam_diff, "source_type": src_diff, "title": title_diff, "given": given_diff, "status": status_diff},
                "final": fin, "target": tgt,
            });
            writeln!(out, "{}", serde_json::to_string(&rec)?)?;
        }
    }
    out.flush()?;
    let n = cites.len();
    println!("=== divergence report over {n} cites ===");
    println!("passed checks: {n_pass}  |  human review: {n_human} ({:.2}%)", 100.0 * n_human as f64 / n as f64);
    println!("diverged from label (any field): {n_div} ({:.1}%)", 100.0 * n_div as f64 / n as f64);
    println!("  family_set: {d_fam}   source_type: {d_src}   title: {d_title}   given: {d_given}   status: {d_status}");
    println!("details written to {out_path}");
    Ok(())
}

fn load_targets(paths: &[String], limit: Option<usize>) -> Result<Vec<(String, String, Value)>> {
    let mut rows = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?;
        for line in text.lines() {
            if line.trim().is_empty() { continue; }
            let v: Value = serde_json::from_str(line)?;
            let target = serde_json::from_str(v["target"].as_str().unwrap_or("{}"))?;
            rows.push((v["id"].as_str().unwrap_or("").to_string(), v["input"].as_str().unwrap_or("").to_string(), target));
            if limit.is_some_and(|l| rows.len() >= l) { return Ok(rows); }
        }
    }
    Ok(rows)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // --from-raws mode: no model, re-analyse the cached decodes.
    if let Some(i) = args.iter().position(|a| a == "--from-raws") {
        let cache = &args[i + 1];
        let out_path = &args[i + 2];
        let text = std::fs::read_to_string(cache)?;
        let cites: Vec<Cite> = text.lines().filter(|l| !l.trim().is_empty()).map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            Cite { id: v["id"].as_str().unwrap_or("").into(), input: v["input"].as_str().unwrap_or("").into(),
                   raw: v["raw"].as_str().unwrap_or("").into(), target: v["target"].clone() }
        }).collect();
        eprintln!("loaded {} cached decodes from {cache}", cites.len());
        return analyze(&cites, out_path);
    }

    if args.len() < 4 {
        eprintln!("usage: diverge <model_dir> <out.jsonl> <labelled.jsonl>... [--limit N] [--threads N] [--dump-raws F]");
        eprintln!("       diverge --from-raws <raws.jsonl> <out.jsonl>");
        std::process::exit(2);
    }
    let model_dir = &args[1];
    let out_path = &args[2];
    let (mut files, mut limit, mut threads, mut dump_raws) = (Vec::new(), None, 16usize, None);
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => { limit = Some(args[i + 1].parse()?); i += 2; }
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--dump-raws" => { dump_raws = Some(args[i + 1].clone()); i += 2; }
            f => { files.push(f.to_string()); i += 1; }
        }
    }

    let targets = load_targets(&files, limit)?;
    eprintln!("loaded {} labelled cites; decoding int8...", targets.len());

    use ct2rs::tokenizers::auto::Tokenizer;
    use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
    let int8: Translator<Tokenizer> = Translator::new(
        model_dir,
        &Config { device: Device::CPU, compute_type: ComputeType::INT8, num_threads_per_replica: threads, ..Default::default() },
    )?;
    let opts = TranslationOptions::<String, String> { beam_size: 1, max_decoding_length: 640, ..Default::default() };

    let mut raw_cache = dump_raws.map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    let mut cites = Vec::with_capacity(targets.len());
    for (ci, chunk) in targets.chunks(64).enumerate() {
        let inputs: Vec<&str> = chunk.iter().map(|(_, inp, _)| inp.as_str()).collect();
        let raws = int8.translate_batch(&inputs, &opts, None)?;
        for ((id, input, target), (raw, _)) in chunk.iter().zip(raws) {
            if let Some(w) = raw_cache.as_mut() {
                let rec = serde_json::json!({"id": id, "input": input, "raw": raw, "target": target});
                writeln!(w, "{}", serde_json::to_string(&rec)?)?;
            }
            cites.push(Cite { id: id.clone(), input: input.clone(), raw, target: target.clone() });
        }
        if ci % 10 == 0 {
            eprintln!("  decoded {} cites...", (ci + 1) * 64);
        }
    }
    if let Some(mut w) = raw_cache { w.flush()?; }
    analyze(&cites, out_path)
}
