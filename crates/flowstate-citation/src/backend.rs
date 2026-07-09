//! Native `CTranslate2` model backend (`ct2` feature) — a [`Decoder`] over `ct2rs`.
//!
//! Compute precision is fixed at load time, so each precision tier is a separate translator
//! sharing the same on-disk `model.bin`. int8 is loaded eagerly (the 96% happy path); int16 and
//! float32 are lazy (they and the beam tier rarely fire once the deterministic layer is applied).

use crate::cascade::{Decoder, Tier};
use anyhow::Result;
use ct2rs::tokenizers::auto::Tokenizer;
use ct2rs::{ComputeType, Config, Device, TranslationOptions, Translator};
use std::path::{Path, PathBuf};

type T = Translator<Tokenizer>;

pub struct Ct2Backend {
    dir: PathBuf,
    threads: usize,
    int8: T,
    float32: Option<T>,
}

fn load(dir: &Path, threads: usize, ct: ComputeType) -> Result<T> {
    let config = Config {
        device: Device::CPU,
        compute_type: ct,
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
    let mut o = TranslationOptions { max_decoding_length: 640, ..Default::default() };
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
        Ok(out.into_iter().next().map(|(s, _)| s).unwrap_or_default())
    }
}
