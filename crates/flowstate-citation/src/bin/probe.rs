//! Minimal probe: time a few int8 decodes and dump the raw model output.
use ct2rs::tokenizers::auto::Tokenizer;
use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
use serde_json::Value;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("model dir");
    let t0 = Instant::now();
    let tr: Translator<Tokenizer> = Translator::new(
        &dir,
        &Config { device: Device::CPU, compute_type: ComputeType::INT8, num_threads_per_replica: 16, ..Default::default() },
    )?;
    eprintln!("model loaded in {:?}", t0.elapsed());

    let opts = TranslationOptions::<String, String> { beam_size: 1, max_decoding_length: 640, ..Default::default() };
    let rows: Vec<Value> = std::fs::read_to_string("datasets/citation_finetune/haiku/gold_eval.jsonl")?
        .lines().take(3).map(|l| serde_json::from_str(l).unwrap()).collect();
    for r in &rows {
        let input = r["input"].as_str().unwrap();
        let t = Instant::now();
        let out = tr.translate_batch(&[input.to_string()], &opts, None)?;
        let raw = out.into_iter().next().map(|(s, _)| s).unwrap_or_default();
        println!("\n[{:?}] {}", t.elapsed(), &input[..input.len().min(70)]);
        println!("RAW: {}", &raw[..raw.len().min(400)]);
    }
    Ok(())
}
