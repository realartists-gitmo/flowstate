//! Native `CTranslate2` model backend (`ct2` feature) — a [`Decoder`] over `ct2rs`.
//!
//! Compute precision is fixed at load time, so each precision tier is a separate translator
//! sharing the same on-disk `model.bin`. Int8 is loaded eagerly and float32 is lazy.

use crate::cascade::{Decode, DecodeIssue, Decoder, Tier};
use crate::limits::MODEL_TOKEN_LIMIT;
use anyhow::{Result, bail};
use ct2rs::tokenizers::auto::Tokenizer as AutoTokenizer;
use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
use std::path::{Path, PathBuf};

type T = Translator<AutoTokenizer>;

const EOS: &str = "</s>";

pub struct Ct2Backend {
    dir: PathBuf,
    threads: usize,
    int8: T,
    float32: Option<T>,
}

fn load(dir: &Path, threads: usize, ct: ComputeType) -> Result<T> {
    // `FLOWSTATE_CT2_REPLICAS=N` loads N model replicas (CTranslate2 inter_threads) so independent
    // decodes — e.g. a cite's perturbations — run concurrently instead of serializing through one
    // replica. Set `--threads` to the per-replica thread count so N × threads ≈ core count. Default
    // 0 = a single replica (unchanged behaviour). Requires the vendored ct2rs inter_threads patch.
    let inter_threads = std::env::var("FLOWSTATE_CT2_REPLICAS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let config = Config {
        device: Device::CPU,
        compute_type: ct,
        inter_threads,
        num_threads_per_replica: threads,
        ..Default::default()
    };
    Translator::new(dir, &config)
}

impl Ct2Backend {
    /// Load the model directory (must contain `model.bin` + `tokenizer.json`). `threads` is the
    /// intra-op thread count per decode; use 1 when parallelising across many cites externally.
    pub fn new<P: AsRef<Path>>(dir: P, threads: usize) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let int8 = load(&dir, threads, ComputeType::INT8)?;
        // Oversized input is truncated to MODEL_TOKEN_LIMIT by the shipped tokenizer (and
        // `max_input_length`); the head holds the cite tag + byline, so the truncated decode is the
        // right answer, not an error. We therefore decode it rather than flagging length.
        Ok(Self { dir, threads, int8, float32: None })
    }

    fn float32(&mut self) -> Result<&T> {
        if self.float32.is_none() {
            self.float32 = Some(load(&self.dir, self.threads, ComputeType::FLOAT32)?);
        }
        Ok(self.float32.as_ref().unwrap())
    }
}

fn opts(tier: Tier) -> TranslationOptions<String, String> {
    let mut o = TranslationOptions {
        max_input_length: MODEL_TOKEN_LIMIT,
        max_decoding_length: MODEL_TOKEN_LIMIT,
        // This distinguishes normal EOS completion from exhausting the output ceiling. The
        // tokenizer decoder renders a returned T5 EOS literally as `</s>`.
        return_end_token: true,
        ..Default::default()
    };
    match tier {
        Tier::Beam => o.beam_size = 8,
        Tier::BestOf => {
            o.beam_size = 1;
            o.sampling_topk = 20;
            o.sampling_temperature = 0.8;
        }
        _ => o.beam_size = 1, // int8 / int16 / float32 greedy
    }
    o
}

impl Decoder for Ct2Backend {
    fn decode(&mut self, input: &str, tier: Tier) -> Result<String> {
        let decoded = self.decode_inner(input, tier)?;
        if let Some(issue) = decoded.issue {
            bail!("citation decode is incomplete: {issue:?}");
        }
        Ok(decoded.raw)
    }

    fn decode_with_metadata(&mut self, input: &str, tier: Tier) -> Result<Decode> {
        self.decode_inner(input, tier)
    }

    fn decode_batch(&mut self, inputs: &[String], tier: Tier, max_output: usize) -> Result<Vec<Decode>> {
        self.decode_batch_inner(inputs, tier, max_output)
    }
}

/// Distinguish a normally-terminated decode (trailing EOS) from one that hit the output ceiling.
fn finish(raw: String) -> Decode {
    let trimmed = raw.trim_end();
    match trimmed.strip_suffix(EOS) {
        Some(complete) => Decode::complete(complete.trim_end()),
        None => Decode::incomplete(raw, DecodeIssue::OutputLengthLimit),
    }
}

impl Ct2Backend {
    fn decode_inner(&mut self, input: &str, tier: Tier) -> Result<Decode> {
        let o = opts(tier);
        // beam runs on float32; best-of-N sampling runs on int8 (precision is irrelevant to
        // sampling quality and int8 is cheapest).
        let translator: &T = match tier {
            // int8 stays greedy; best-of-N samples on int8. int16 (unsupported on the ruy CPU
            // backend) falls back to float32.
            Tier::Int8 | Tier::BestOf => &self.int8,
            Tier::Int16 | Tier::Float32 | Tier::Beam => self.float32()?,
        };
        let out = translator.translate_batch(&[input.to_string()], &o, None)?;
        let raw = out.into_iter().next().map(|(text, _)| text).unwrap_or_default();
        Ok(finish(raw))
    }

    /// Decode all `inputs` in a single engine batch. Oversized inputs are flagged without decoding
    /// and the rest are translated together, so a cite's perturbations cost one batched decode.
    /// `max_output` (when non-zero) caps `max_decoding_length` below the model ceiling.
    fn decode_batch_inner(&mut self, inputs: &[String], tier: Tier, max_output: usize) -> Result<Vec<Decode>> {
        let mut o = opts(tier);
        if max_output > 0 {
            o.max_decoding_length = max_output;
        }
        let translator: &T = match tier {
            Tier::Int8 | Tier::BestOf => &self.int8,
            Tier::Int16 | Tier::Float32 | Tier::Beam => self.float32()?,
        };
        let out = translator.translate_batch(inputs, &o, None)?;
        Ok(out.into_iter().map(|(text, _)| finish(text)).collect())
    }
}
