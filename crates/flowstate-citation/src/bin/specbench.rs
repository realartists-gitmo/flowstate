//! Verify prompt-lookup speculative decoding is LOSSLESS and measure its speedup.
//!
//! Runs each input twice — baseline greedy (`prompt_lookup_ngram = 0`) and speculative
//! (`prompt_lookup_ngram = N`) — asserts the raw decode is byte-identical, and reports the
//! wall-clock speedup.
//!
//! Usage: `specbench <model_dir> <inputs.jsonl> [--limit N] [--ngram K] [--max-draft M]`
//! where each line of `inputs.jsonl` has an `"input"` field (`"parse citation: ..."`).

use anyhow::Result;
use ct2rs::tokenizers::auto::Tokenizer;
use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
use flowstate_citation::limits::MODEL_TOKEN_LIMIT;
use serde_json::Value;
use std::time::Instant;

fn decode_all(tr: &Translator<Tokenizer>, inputs: &[&str], ngram: usize, max_draft: usize) -> Result<(Vec<String>, f64)> {
    let opts = TranslationOptions::<String, String> {
        beam_size: 1,
        max_input_length: MODEL_TOKEN_LIMIT,
        max_decoding_length: MODEL_TOKEN_LIMIT,
        min_decoding_length: 0,
        // length_penalty left at its default (1.0) — the gate now allows it (greedy tokens
        // are unaffected), so this exercises the production-realistic path.
        prompt_lookup_ngram: ngram,
        prompt_lookup_max_tokens: max_draft,
        ..Default::default()
    };
    let t0 = Instant::now();
    let mut out = Vec::with_capacity(inputs.len());
    // decode one at a time: prompt-lookup engages only at batch size 1 (the latency path)
    for inp in inputs {
        let res = tr.translate_batch(&[*inp], &opts, None)?;
        out.push(res.into_iter().next().map(|(r, _)| r).unwrap_or_default());
    }
    Ok((out, t0.elapsed().as_secs_f64()))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: specbench <model_dir> <inputs.jsonl> [--limit N] [--ngram K] [--max-draft M]");
        std::process::exit(2);
    }
    let model_dir = &args[1];
    let inputs_path = &args[2];
    let mut limit = 200usize;
    let mut ngram = 3usize;
    let mut max_draft = 10usize;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => { limit = args[i + 1].parse()?; i += 2; }
            "--ngram" => { ngram = args[i + 1].parse()?; i += 2; }
            "--max-draft" => { max_draft = args[i + 1].parse()?; i += 2; }
            _ => { i += 1; }
        }
    }

    let text = std::fs::read_to_string(inputs_path)?;
    let inputs: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("input").and_then(Value::as_str).map(str::to_string))
        .take(limit)
        .collect();
    let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
    eprintln!("loaded {} inputs; model={model_dir}; ngram={ngram} max_draft={max_draft}", refs.len());

    let tr: Translator<Tokenizer> = Translator::new(
        model_dir,
        &Config { device: Device::CPU, compute_type: ComputeType::INT8, num_threads_per_replica: 4, ..Default::default() },
    )?;

    // warm up (first call pays model-load / allocation costs)
    let _ = decode_all(&tr, &refs[..refs.len().min(8)], 0, max_draft)?;

    let (base, t_base) = decode_all(&tr, &refs, 0, max_draft)?;
    let (spec, t_spec) = decode_all(&tr, &refs, ngram, max_draft)?;

    // losslessness check
    let mut mismatches = 0usize;
    for (idx, (b, s)) in base.iter().zip(&spec).enumerate() {
        if b != s {
            mismatches += 1;
            if mismatches <= 5 {
                eprintln!("MISMATCH #{idx}:\n  base: {b}\n  spec: {s}");
            }
        }
    }

    println!("=== prompt-lookup specbench ({} cites) ===", refs.len());
    println!("lossless: {}  (mismatches: {mismatches})", if mismatches == 0 { "YES ✓" } else { "NO ✗" });
    println!("baseline greedy : {t_base:.3}s  ({:.1} ms/cite)", 1000.0 * t_base / refs.len() as f64);
    println!("speculative     : {t_spec:.3}s  ({:.1} ms/cite)", 1000.0 * t_spec / refs.len() as f64);
    println!("speedup         : {:.2}x", t_base / t_spec);
    if mismatches > 0 {
        std::process::exit(1);
    }
    Ok(())
}
