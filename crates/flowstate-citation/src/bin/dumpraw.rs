//! Decode every labelled cite ONCE (original full decode + merged-perturbation capped decode) and
//! dump the raw model strings, so repair-layer changes can be replayed offline (see `replay.rs`)
//! without paying for the model. The raw decode is the model's only nondeterministic input; repair
//! is a pure function of (raw, source), so this captures everything needed to iterate on repair.
//!
//! Usage: `dumpraw <model_dir> <labelled.jsonl> [--threads N] --out raws.jsonl`

use anyhow::Result;
use flowstate_citation::backend::Ct2Backend;
use flowstate_citation::cascade::{Decoder, Tier};
use flowstate_citation::perturb;
use serde_json::Value;
use std::io::Write;

const PERTURB_MAX_TOKENS: usize = 512;
const MAX_BATCH: usize = 48;

fn issue_str(d: &flowstate_citation::cascade::Decode) -> Option<&'static str> {
    d.issue.map(|i| i.reason())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (model_dir, file) = (&args[1], &args[2]);
    let (mut threads, mut out_path) = (12usize, None);
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--threads" => { threads = args[i + 1].parse()?; i += 2; }
            "--out" => { out_path = Some(args[i + 1].clone()); i += 2; }
            _ => { i += 1; }
        }
    }
    let rows: Vec<(String, String)> = std::fs::read_to_string(file)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (v["id"].as_str().unwrap_or("").to_string(), v["input"].as_str().unwrap_or("").to_string())
        })
        .collect();
    eprintln!("decoding {} cites (original + merged perturbation)…", rows.len());

    let _ = MAX_BATCH;
    let mut backend = Ct2Backend::new(model_dir, threads)?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(out_path.unwrap())?);

    // Per-cite decode (memory-flat; the batched path bloats padding across long cites). Original at
    // full length; the single merged perturbation capped short. Run once, then replay offline.
    for (n, (id, inp)) in rows.iter().enumerate() {
        let orig = backend.decode_with_metadata(inp, Tier::Int8)?;
        let (praw, pissue, has_p) = match perturb::merged(inp) {
            Some(m) => {
                let d = backend.decode_batch(std::slice::from_ref(&m.input), Tier::Int8, PERTURB_MAX_TOKENS)?;
                let d0 = &d[0];
                (d0.raw.clone(), issue_str(d0), true)
            }
            None => (String::new(), None, false),
        };
        let rec = serde_json::json!({
            "id": id, "input": inp,
            "raw": orig.raw, "orig_issue": issue_str(&orig),
            "praw": praw, "praw_issue": pissue, "has_perturb": has_p,
        });
        writeln!(w, "{}", serde_json::to_string(&rec)?)?;
        if n % 50 == 0 { w.flush()?; eprintln!("  {}/{}", n, rows.len()); }
    }
    w.flush()?;
    Ok(())
}
