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
use flowstate_citation::limits::MODEL_TOKEN_LIMIT;
use flowstate_citation::text::fold;
use flowstate_citation::{FlagMode, process_with_mode};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Write;

struct Cite {
    id: String,
    input: String,
    raw: String,
    target: Value,
}

fn surnameset(v: &Value) -> BTreeSet<String> {
    v.get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("surname").and_then(Value::as_str)).map(fold).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

const HONORIFICS: &[&str] = &[
    "dr", "prof", "professor", "mr", "mrs", "ms", "mx", "sir", "maj", "lt", "col", "gen", "capt",
    "rev", "hon", "the", "and",
];

/// Eval-normalize a full name to first+last (drop honorifics, middle names/initials, case,
/// diacritics). `Kenneth R. Westphal` ≡ `Kenneth Westphal`, but `Mike Bartels` ≢ `Meghan Bartels`.
/// This is a COMPARISON normalizer only — it never touches production JSON.
fn norm_name(s: &str) -> String {
    let toks: Vec<String> = s
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphabetic()).to_string())
        .filter(|w| !w.is_empty() && !HONORIFICS.contains(&fold(w).as_str()))
        .collect();
    match toks.len() {
        0 => String::new(),
        1 => fold(&toks[0]),
        _ => format!("{} {}", fold(&toks[0]), fold(toks.last().unwrap())),
    }
}

fn nameset(v: &Value) -> BTreeSet<String> {
    v.get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("name").and_then(Value::as_str)).map(norm_name).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// Eval-normalize a title: strip leading section labels and a trailing `Author(s):` tail, then
/// fold (case/quotes/dashes/punct/whitespace collapse). Comparison-only.
fn norm_title(s: &str) -> String {
    let mut t = s.trim().to_lowercase();
    for lab in ["article:", "essay:", "chapter title is", "review of", "report:", "book review:"] {
        if let Some(rest) = t.strip_prefix(lab) {
            t = rest.trim().to_string();
        }
    }
    for tail in ["author(s):", "authors:", " by "] {
        if let Some(p) = t.find(tail) {
            t.truncate(p);
        }
    }
    fold(&t)
}

fn fstr(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).map(fold).unwrap_or_default()
}

fn tstr(v: &Value) -> String {
    v.get("title").and_then(Value::as_str).map(norm_title).unwrap_or_default()
}

/// Apply the deterministic pipeline to every cached cite and write a divergence report.
fn analyze(cites: &[Cite], out_path: &str, all_path: Option<&str>, mode: FlagMode) -> Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(out_path)?);
    let mut all = all_path
        .map(|p| std::fs::File::create(p).map(std::io::BufWriter::new))
        .transpose()?;
    let (mut n_pass, mut n_human, mut n_div) = (0usize, 0usize, 0usize);
    let (mut d_surname, mut d_src, mut d_title, mut d_name, mut d_status) = (0usize, 0, 0, 0, 0);
    for c in cites {
        let (final_obj, reason) = process_with_mode(&c.raw, &c.input, mode);
        let passed = reason.is_none();
        if passed { n_pass += 1; } else { n_human += 1; }
        let empty = Value::Object(serde_json::Map::new());
        let fin = final_obj.as_ref().unwrap_or(&empty);
        let tgt = &c.target;
        let surname_diff = surnameset(fin) != surnameset(tgt);
        let src_diff = fstr(fin, "source_type") != fstr(tgt, "source_type");
        let title_diff = tstr(fin) != tstr(tgt);
        let name_diff = nameset(fin) != nameset(tgt);
        let status_diff = fstr(fin, "status") != fstr(tgt, "status");
        let diverged = surname_diff || src_diff || title_diff || name_diff || status_diff || !passed;
        if let Some(w) = all.as_mut() {
            let rec = serde_json::json!({
                "id": c.id, "input": c.input.replace("parse citation: ", ""),
                "model": fin, "haiku": tgt, "diverged": diverged,
                "human_review": !passed, "reason": reason,
            });
            writeln!(w, "{}", serde_json::to_string(&rec)?)?;
        }
        if diverged {
            n_div += 1;
            d_surname += surname_diff as usize;
            d_src += src_diff as usize;
            d_title += title_diff as usize;
            d_name += name_diff as usize;
            d_status += status_diff as usize;
            let rec = serde_json::json!({
                "id": c.id, "input": c.input.replace("parse citation: ", ""), "human_review": !passed,
                "reason": reason,
                "diffs": {"surname_set": surname_diff, "source_type": src_diff, "title": title_diff, "name": name_diff, "status": status_diff},
                "final": fin, "target": tgt,
            });
            writeln!(out, "{}", serde_json::to_string(&rec)?)?;
        }
    }
    out.flush()?;
    if let Some(mut w) = all {
        w.flush()?;
    }
    let n = cites.len();
    println!("=== divergence report over {n} cites (flag mode: {mode:?}) ===");
    println!("passed checks: {n_pass}  |  human review: {n_human} ({:.2}%)", 100.0 * n_human as f64 / n as f64);
    println!("diverged from label (any field): {n_div} ({:.1}%)", 100.0 * n_div as f64 / n as f64);
    println!("  surname_set: {d_surname}   source_type: {d_src}   title: {d_title}   name: {d_name}   status: {d_status}");
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

    let mode_of = |args: &[String]| args.iter().position(|a| a == "--flag-mode").map_or(Ok(FlagMode::default()), |j| parse_mode(&args[j + 1]));

    // --from-raws mode: no model, re-analyse the cached decodes.
    if let Some(i) = args.iter().position(|a| a == "--from-raws") {
        let cache = &args[i + 1];
        let out_path = &args[i + 2];
        let all_path = args.iter().position(|a| a == "--dump-all").map(|j| args[j + 1].as_str());
        let text = std::fs::read_to_string(cache)?;
        let cites: Vec<Cite> = text.lines().filter(|l| !l.trim().is_empty()).map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            Cite { id: v["id"].as_str().unwrap_or("").into(), input: v["input"].as_str().unwrap_or("").into(),
                   raw: v["raw"].as_str().unwrap_or("").into(), target: v["target"].clone() }
        }).collect();
        eprintln!("loaded {} cached decodes from {cache}", cites.len());
        return analyze(&cites, out_path, all_path, mode_of(&args)?);
    }

    if args.len() < 4 {
        eprintln!("usage: diverge <model_dir> <out.jsonl> <labelled.jsonl>... [--limit N] [--threads N] [--dump-raws F]");
        eprintln!("       diverge --from-raws <raws.jsonl> <out.jsonl>");
        std::process::exit(2);
    }
    let model_dir = &args[1];
    let out_path = &args[2];
    let (mut files, mut limit, mut threads, mut dump_raws) = (Vec::new(), None, 16usize, None);
    let mut compute = "int8".to_string();
    // A batch is decoded to its longest member, and T5 attention is O(seq²); a single runaway
    // decode near the ceiling forces the whole batch to that length, so keep batches small to
    // bound peak memory. (Production mass-processing must batch by token budget for the same
    // reason; here a fixed small count is enough to keep the bench under a memory cap.)
    let mut batch = 16usize;
    let mut mode = FlagMode::default();
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => { limit = Some(args[i + 1].parse()?); i += 2; }
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--dump-raws" => { dump_raws = Some(args[i + 1].clone()); i += 2; }
            "--compute" => { compute = args[i + 1].clone(); i += 2; }
            "--batch" => { batch = args[i + 1].parse()?; i += 2; }
            "--flag-mode" => { mode = parse_mode(&args[i + 1])?; i += 2; }
            f => { files.push(f.to_string()); i += 1; }
        }
    }

    let targets = load_targets(&files, limit)?;

    use ct2rs::tokenizers::auto::Tokenizer;
    use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
    let ct = match compute.as_str() {
        "int8" => ComputeType::INT8,
        "int16" => ComputeType::INT16,
        "float32" | "float" | "fp32" => ComputeType::FLOAT32,
        other => { eprintln!("unknown --compute '{other}' (use int8|int16|float32)"); std::process::exit(2); }
    };
    eprintln!("loaded {} labelled cites; decoding {compute}...", targets.len());
    let int8: Translator<Tokenizer> = Translator::new(
        model_dir,
        &Config { device: Device::CPU, compute_type: ct, num_threads_per_replica: threads, ..Default::default() },
    )?;
    let opts = TranslationOptions::<String, String> {
        beam_size: 1,
        max_input_length: MODEL_TOKEN_LIMIT,
        max_decoding_length: MODEL_TOKEN_LIMIT,
        ..Default::default()
    };

    let mut raw_cache = dump_raws.map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    let mut cites = Vec::with_capacity(targets.len());
    for (ci, chunk) in targets.chunks(batch).enumerate() {
        let inputs: Vec<&str> = chunk.iter().map(|(_, inp, _)| inp.as_str()).collect();
        let raws = int8.translate_batch(&inputs, &opts, None)?;
        for ((id, input, target), (raw, _)) in chunk.iter().zip(raws) {
            if let Some(w) = raw_cache.as_mut() {
                let rec = serde_json::json!({"id": id, "input": input, "raw": raw, "target": target});
                writeln!(w, "{}", serde_json::to_string(&rec)?)?;
            }
            cites.push(Cite { id: id.clone(), input: input.clone(), raw, target: target.clone() });
        }
        // flush the raw cache each batch so a mid-run kill still leaves a usable prefix
        if let Some(w) = raw_cache.as_mut() {
            w.flush()?;
        }
        eprintln!("  decoded {} / {} cites...", (ci * batch + chunk.len()).min(targets.len()), targets.len());
    }
    if let Some(mut w) = raw_cache { w.flush()?; }
    analyze(&cites, out_path, None, mode)
}

/// Parse a `--flag-mode` argument. Defaults elsewhere to [`FlagMode::Passthrough`].
fn parse_mode(s: &str) -> Result<FlagMode> {
    match s {
        "passthrough" | "none" | "off" => Ok(FlagMode::Passthrough),
        "checks" => Ok(FlagMode::Checks),
        "review" => Ok(FlagMode::Review),
        other => anyhow::bail!("unknown --flag-mode '{other}' (use passthrough|checks|review)"),
    }
}
