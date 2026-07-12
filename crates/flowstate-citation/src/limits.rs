//! Token ceilings shared by training-compatible inference entry points.

/// The deployed checkpoint and tokenizer were trained/configured for sequences up to 3,072
/// tokens. `CTranslate2` has lower defaults, so every inference path must set this explicitly or it
/// can silently discard source text or stop halfway through a long author list.
pub const MODEL_TOKEN_LIMIT: usize = 3_072;
