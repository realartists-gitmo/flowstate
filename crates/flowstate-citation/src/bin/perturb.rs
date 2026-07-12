//! Measure **input-perturbation decorrelation** as a cheap flagger.
//!
//! For every cite we already have the original int8 decode (from `diverge --dump-raws`). Here we
//! additionally decode one or more meaning-preserving perturbations of the input and flag the cite
//! when the perturbed decode disagrees with the original on author identities or status. A
//! systematic omission is brittle to surface form, so the perturbed decode tends to keep the
//! author the original dropped — surfacing a silent failure the precision cascade needed a full
//! second fp32 decode (10s/cite) to catch.
//!
//! Reports each perturbation's precision/recall against the gold labels, plus the combined
//! "flag if ANY perturbation disagrees" flagger, so we can pick the best variant at ~2x latency.
//!
//! Usage: `perturb <model_dir> <raws_cache.jsonl> [--threads N] [--batch N] [--out F]`

use anyhow::Result;
use ct2rs::tokenizers::auto::Tokenizer;
use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
use flowstate_citation::limits::MODEL_TOKEN_LIMIT;
use flowstate_citation::text::fold;
use flowstate_citation::{FlagMode, perturb, process_with_mode};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

/// Canonical author identity set: folded surnames. Empty surnames dropped.
fn surnameset(v: Option<&Value>) -> BTreeSet<String> {
    v.and_then(|v| v.get("authors"))
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

fn status(v: Option<&Value>) -> String {
    v.and_then(|v| v.get("status")).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The scored identity of a decode: what a flag compares and what we grade against gold.
#[derive(PartialEq, Eq)]
struct Identity {
    surnames: BTreeSet<String>,
    status: String,
}

fn identity_of(raw: &str, source: &str) -> Identity {
    let (obj, _) = process_with_mode(raw, source, FlagMode::Passthrough);
    Identity { surnames: surnameset(obj.as_ref()), status: status(obj.as_ref()) }
}

struct Cite {
    input: String,
    orig_raw: String,
    gold: Identity,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: perturb <model_dir> <raws_cache.jsonl> [--threads N] [--batch N] [--out F]");
        std::process::exit(2);
    }
    let model_dir = &args[1];
    let cache = &args[2];
    let (mut threads, mut batch, mut out_path) = (12usize, 8usize, None);
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--batch" => { batch = args[i + 1].parse()?; i += 2; }
            "--out" => { out_path = Some(args[i + 1].clone()); i += 2; }
            _ => { i += 1; }
        }
    }

    // Load the cached original decodes and the gold labels.
    let cites: Vec<Cite> = std::fs::read_to_string(cache)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            let input = v["input"].as_str().unwrap_or("").to_string();
            // the raws cache stores `target` as an object (diverge pre-parsed it); older callers
            // may pass it as a JSON string — accept both.
            let tgt: Value = match &v["target"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                other => other.clone(),
            };
            Cite {
                orig_raw: v["raw"].as_str().unwrap_or("").to_string(),
                gold: Identity { surnames: surnameset(Some(&tgt)), status: status(Some(&tgt)) },
                input,
            }
        })
        .collect();
    eprintln!("loaded {} cached decodes", cites.len());

    // Build the perturbation jobs: (cite index, label, perturbed input).
    let jobs: Vec<(usize, &'static str, String)> = cites
        .iter()
        .enumerate()
        .flat_map(|(ci, c)| perturb::variants(&c.input).into_iter().map(move |v| (ci, v.label, v.input)))
        .collect();
    eprintln!("decoding {} perturbed inputs across {} cites...", jobs.len(), cites.len());

    let translator: Translator<Tokenizer> = Translator::new(
        model_dir,
        &Config { device: Device::CPU, compute_type: ComputeType::INT8, num_threads_per_replica: threads, ..Default::default() },
    )?;
    let opts = TranslationOptions::<String, String> {
        beam_size: 1,
        max_input_length: MODEL_TOKEN_LIMIT,
        max_decoding_length: MODEL_TOKEN_LIMIT,
        ..Default::default()
    };

    // per cite: label -> perturbed identity
    let mut pert: Vec<BTreeMap<&'static str, Identity>> = (0..cites.len()).map(|_| BTreeMap::new()).collect();
    for (bi, chunk) in jobs.chunks(batch).enumerate() {
        let inputs: Vec<&str> = chunk.iter().map(|(_, _, inp)| inp.as_str()).collect();
        let raws = translator.translate_batch(&inputs, &opts, None)?;
        for ((ci, label, _), (raw, _)) in chunk.iter().zip(raws) {
            let id = identity_of(&raw, &cites[*ci].input); // ground against the FULL original source
            pert[*ci].insert(label, id);
        }
        if bi % 10 == 0 {
            eprintln!("  decoded {} / {} perturbations...", (bi * batch + chunk.len()).min(jobs.len()), jobs.len());
        }
    }

    // Score. The shipped output is the ORIGINAL decode; perturbations only decide the flag.
    let n = cites.len();
    // per-label tallies, plus a combined "flag if ANY disagrees"
    let mut labels: Vec<&str> = perturb::LABELS.to_vec();
    labels.push("ANY");
    let mut buckets: BTreeMap<&str, [usize; 4]> = labels.iter().map(|l| (*l, [0usize; 4])).collect(); // [perfect, silent, good_flag, over_flag]
    let mut applicable: BTreeMap<&str, usize> = labels.iter().map(|l| (*l, 0usize)).collect();
    let mut writer = out_path.map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));

    for (ci, c) in cites.iter().enumerate() {
        let orig = identity_of(&c.orig_raw, &c.input);
        let correct = orig == c.gold;
        let mut any_flag = false;
        for &label in perturb::LABELS {
            let Some(p) = pert[ci].get(label) else { continue };
            *applicable.get_mut(label).unwrap() += 1;
            let disagree = p.surnames != orig.surnames || p.status != orig.status;
            any_flag |= disagree;
            let slot = match (disagree, correct) {
                (false, true) => 0,  // pass & correct
                (false, false) => 1, // pass & wrong (SILENT)
                (true, false) => 2,  // flag & wrong (good)
                (true, true) => 3,   // flag & correct (over-flag)
            };
            buckets.get_mut(label).unwrap()[slot] += 1;
        }
        *applicable.get_mut("ANY").unwrap() += 1;
        let slot = match (any_flag, correct) {
            (false, true) => 0, (false, false) => 1, (true, false) => 2, (true, true) => 3,
        };
        buckets.get_mut("ANY").unwrap()[slot] += 1;

        if let Some(w) = writer.as_mut() {
            let rec = serde_json::json!({
                "correct": correct, "any_flag": any_flag,
                "orig_surnames": orig.surnames.iter().collect::<Vec<_>>(),
                "gold_surnames": c.gold.surnames.iter().collect::<Vec<_>>(),
                "perturbed": pert[ci].iter().map(|(k, v)| (*k, v.surnames.iter().collect::<Vec<_>>())).collect::<BTreeMap<_, _>>(),
            });
            writeln!(w, "{}", serde_json::to_string(&rec)?)?;
        }
    }
    if let Some(mut w) = writer { w.flush()?; }

    println!("\n=== input-perturbation decorrelation over {n} cites ===");
    println!("(shipped output = original decode; perturbation decides the flag; identity = surnames + status)\n");
    println!("{:<9} {:>7} {:>8} {:>9} {:>9} {:>7} {:>8}", "variant", "applic", "perfect", "silent", "good_flg", "overflg", "precis");
    for &label in &labels {
        let [perfect, silent, good, over] = buckets[label];
        let ap = applicable[label];
        let precision = if good + over > 0 { 100.0 * good as f64 / (good + over) as f64 } else { 0.0 };
        println!("{label:<9} {ap:>7} {perfect:>8} {silent:>9} {good:>9} {over:>9} {precision:>6.0}%");
    }
    println!("\nbaseline (no flag): perfect 431, silent 90  |  target: convert silent -> good flags without over-flagging");
    Ok(())
}
