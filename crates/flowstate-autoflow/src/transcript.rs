use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TranscriptSegment {
  pub segment_id: String,
  pub audio_path: String,
  pub speech_context: Option<SpeechContext>,
  pub words: Vec<AsrWord>,
  pub transcript_reviewed: bool,
  pub row_labels_reviewed: bool,
  pub trainable: bool,
}

impl TranscriptSegment {
  #[must_use]
  pub fn new(segment_id: impl Into<String>, audio_path: impl Into<String>, words: Vec<AsrWord>) -> Self {
    Self {
      segment_id: segment_id.into(),
      audio_path: audio_path.into(),
      speech_context: None,
      words,
      transcript_reviewed: false,
      row_labels_reviewed: false,
      trainable: false,
    }
  }

  #[must_use]
  pub fn text(&self) -> String {
    self.words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" ")
  }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AsrWord {
  pub text: String,
  pub timing: WordTiming,
  pub asr_confidence: Option<Confidence>,
  pub pause_before_ms: Option<u32>,
  pub pause_after_ms: Option<u32>,
  pub volume_z: Option<ScaledFeature>,
  pub pitch_reset: Option<bool>,
}

impl AsrWord {
  #[must_use]
  pub fn new(text: impl Into<String>, start_ms: u32, end_ms: u32) -> Self {
    Self {
      text: text.into(),
      timing: WordTiming { start_ms, end_ms },
      asr_confidence: None,
      pause_before_ms: None,
      pause_after_ms: None,
      volume_z: None,
      pitch_reset: None,
    }
  }

  #[must_use]
  pub fn normalized_text(&self) -> String {
    self.text.trim_matches(|character: char| !character.is_alphanumeric()).to_lowercase()
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WordTiming {
  pub start_ms: u32,
  pub end_ms: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Confidence(u16);

impl Confidence {
  pub const MAX: u16 = 1_000;

  #[must_use]
  pub fn from_millis(value: u16) -> Self {
    Self(value.min(Self::MAX))
  }

  #[must_use]
  pub const fn millis(self) -> u16 {
    self.0
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ScaledFeature(i16);

impl ScaledFeature {
  #[must_use]
  pub const fn from_centis(value: i16) -> Self {
    Self(value)
  }

  #[must_use]
  pub const fn centis(self) -> i16 {
    self.0
  }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SpeechContext {
  Constructive { side: DebateSide, speech: String },
  Rebuttal { side: DebateSide, speech: String },
  CrossEx,
  JudgeDisclosure,
  Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DebateSide {
  Affirmative,
  Negative,
}
