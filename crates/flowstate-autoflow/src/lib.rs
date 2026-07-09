//! Debate autoflow primitives and row-boundary prediction scaffolding.
//!
//! This crate intentionally contains no GPUI code and no concrete ASR runtime.
//! It owns the stable data contracts between transcription, row segmentation,
//! deterministic flow routing, and future model-backed prediction.

mod boundary;
mod event;
mod transcript;

pub use boundary::{
  BoundaryClass, BoundaryDecision, BoundaryEvidence, BoundaryModel, BoundaryPrediction, HeuristicBoundaryModel, RowBoundary,
};
pub use event::{FlowEvent, FlowTarget, RowBoundarySource, events_from_boundaries};
pub use transcript::{AsrWord, SpeechContext, TranscriptSegment, WordTiming};
