use serde::{Deserialize, Serialize};

use crate::transcript::{Confidence, TranscriptSegment};

pub trait BoundaryModel {
  fn predict(&self, segment: &TranscriptSegment) -> BoundaryPrediction;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BoundaryPrediction {
  pub segment_id: String,
  pub boundaries: Vec<RowBoundary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RowBoundary {
  pub word_index: usize,
  pub decision: BoundaryDecision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BoundaryDecision {
  pub class: BoundaryClass,
  pub confidence: Confidence,
  pub evidence: BoundaryEvidence,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BoundaryClass {
  Hard,
  Soft,
  None,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct BoundaryEvidence {
  pub pause_before_ms: Option<u32>,
  pub delimiter_word: Option<String>,
  pub list_marker: bool,
  pub pitch_reset: bool,
  pub emphasized: bool,
}

#[derive(Clone, Debug)]
pub struct HeuristicBoundaryModel {
  hard_pause_ms: u32,
  soft_pause_ms: u32,
  emphasis_centis: i16,
}

impl Default for HeuristicBoundaryModel {
  fn default() -> Self {
    Self {
      hard_pause_ms: 500,
      soft_pause_ms: 250,
      emphasis_centis: 125,
    }
  }
}

impl BoundaryModel for HeuristicBoundaryModel {
  fn predict(&self, segment: &TranscriptSegment) -> BoundaryPrediction {
    let boundaries = segment
      .words
      .iter()
      .enumerate()
      .map(|(word_index, word)| {
        let normalized = word.normalized_text();
        let pause_before_ms = word.pause_before_ms;
        let delimiter_word = is_delimiter(&normalized).then_some(normalized.clone());
        let list_marker = is_list_marker(&normalized);
        let pitch_reset = word.pitch_reset.unwrap_or(false);
        let emphasized = word.volume_z.is_some_and(|volume| volume.centis() >= self.emphasis_centis);
        let evidence = BoundaryEvidence {
          pause_before_ms,
          delimiter_word,
          list_marker,
          pitch_reset,
          emphasized,
        };

        RowBoundary {
          word_index,
          decision: self.decide(&evidence),
        }
      })
      .collect();

    BoundaryPrediction {
      segment_id: segment.segment_id.clone(),
      boundaries,
    }
  }
}

impl HeuristicBoundaryModel {
  #[must_use]
  pub const fn new(hard_pause_ms: u32, soft_pause_ms: u32, emphasis_centis: i16) -> Self {
    Self {
      hard_pause_ms,
      soft_pause_ms,
      emphasis_centis,
    }
  }

  fn decide(&self, evidence: &BoundaryEvidence) -> BoundaryDecision {
    let pause = evidence.pause_before_ms.unwrap_or_default();
    let has_strong_marker = evidence.list_marker || evidence.delimiter_word.is_some();
    let acoustic_reset = evidence.pitch_reset || evidence.emphasized;

    let (class, confidence) = if pause >= self.hard_pause_ms && (has_strong_marker || acoustic_reset) {
      (BoundaryClass::Hard, Confidence::from_millis(940))
    } else if has_strong_marker && acoustic_reset {
      (BoundaryClass::Hard, Confidence::from_millis(900))
    } else if pause >= self.soft_pause_ms || has_strong_marker || acoustic_reset {
      (BoundaryClass::Soft, Confidence::from_millis(680))
    } else {
      (BoundaryClass::None, Confidence::from_millis(850))
    };

    BoundaryDecision {
      class,
      confidence,
      evidence: evidence.clone(),
    }
  }
}

fn is_delimiter(word: &str) -> bool {
  matches!(word, "and" | "next" | "also" | "then" | "plus")
}

fn is_list_marker(word: &str) -> bool {
  matches!(
    word,
    "one" | "two" | "three" | "four" | "five" | "first" | "second" | "third" | "a" | "b" | "c"
  )
}

#[cfg(test)]
mod tests {
  use crate::{
    boundary::{BoundaryClass, BoundaryModel, HeuristicBoundaryModel},
    transcript::{AsrWord, ScaledFeature, TranscriptSegment},
  };

  #[test]
  fn marks_pronounced_delimiter_after_pause_as_hard_boundary() {
    let mut next = AsrWord::new("NEXT", 1_000, 1_120);
    next.pause_before_ms = Some(620);
    next.volume_z = Some(ScaledFeature::from_centis(150));

    let segment = TranscriptSegment::new("seg", "seg.wav", vec![next]);
    let prediction = HeuristicBoundaryModel::default().predict(&segment);

    assert_eq!(prediction.boundaries[0].decision.class, BoundaryClass::Hard);
  }
}
