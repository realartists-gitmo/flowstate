use serde::{Deserialize, Serialize};

use crate::{
  boundary::{BoundaryClass, BoundaryDecision, BoundaryPrediction},
  transcript::TranscriptSegment,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FlowEvent {
  pub segment_id: String,
  pub start_word_index: usize,
  pub end_word_index: usize,
  pub text: String,
  pub boundary: RowBoundarySource,
  pub flow_target: Option<FlowTarget>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RowBoundarySource {
  pub class: BoundaryClass,
  pub decision: BoundaryDecision,
  pub human_review_needed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FlowTarget {
  pub sheet: String,
  pub argument: Option<String>,
  pub column: Option<String>,
}

#[must_use]
pub fn events_from_boundaries(segment: &TranscriptSegment, prediction: &BoundaryPrediction) -> Vec<FlowEvent> {
  let mut events = Vec::new();
  let mut row_start = 0;
  let mut row_boundary = prediction
    .boundaries
    .first()
    .map(|boundary| boundary.decision.clone())
    .unwrap_or_else(default_initial_boundary);

  for boundary in prediction.boundaries.iter().skip(1) {
    if boundary.decision.class == BoundaryClass::None {
      continue;
    }

    events.push(build_event(segment, row_start, boundary.word_index, row_boundary));
    row_start = boundary.word_index;
    row_boundary = boundary.decision.clone();
  }

  if row_start < segment.words.len() {
    events.push(build_event(segment, row_start, segment.words.len(), row_boundary));
  }

  events
}

fn build_event(segment: &TranscriptSegment, start_word_index: usize, end_word_index: usize, decision: BoundaryDecision) -> FlowEvent {
  let text = segment.words[start_word_index..end_word_index]
    .iter()
    .map(|word| word.text.as_str())
    .collect::<Vec<_>>()
    .join(" ");
  let class = decision.class;

  FlowEvent {
    segment_id: segment.segment_id.clone(),
    start_word_index,
    end_word_index,
    text,
    boundary: RowBoundarySource {
      class,
      decision,
      human_review_needed: class == BoundaryClass::Soft,
    },
    flow_target: None,
  }
}

fn default_initial_boundary() -> BoundaryDecision {
  BoundaryDecision {
    class: BoundaryClass::Hard,
    confidence: crate::transcript::Confidence::from_millis(1_000),
    evidence: crate::boundary::BoundaryEvidence::default(),
  }
}
