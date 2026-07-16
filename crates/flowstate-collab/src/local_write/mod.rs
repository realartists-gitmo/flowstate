//! Loro-first local write path (spec `flowstate_loro_first_spec.md` §4).
//!
//! The ONE way local editing mutates a document, identical for solo and
//! collaborative documents (invariant 5):
//!
//! ```text
//! editor input → typed intent → [write gate] resolve → mutate → commit
//!             → exact projection patches, synchronously
//! ```
//!
//! Raw projection-space commands do not exist here — the intent types make
//! them unrepresentable (invariant 6). Remote traffic reaches the same doc
//! only through `DocIoService`, which acquires the same gate per import chunk.

pub mod gate;
pub mod handle;
pub mod intents;

pub(crate) mod commit;
pub(crate) mod patch_synthesis;
pub(crate) mod recorded_inverse;
pub(crate) mod resolve;
pub(crate) mod table_cell_text;

#[cfg(test)]
mod tests;

pub use gate::{GateHoldRecord, GateHolder, GateMetrics, GatePoisonedError, WriteGate};
pub use handle::{LocalDocHandle, LocalWriteConfig};
pub use intents::{
  CursorEndpoint, DeleteBlocksIntent, DeleteRangeIntent, FragmentBlock, InsertObjectIntent, InsertRichFragmentIntent, InsertTextIntent,
  IntentCounters, JoinParagraphsIntent, LocalCommit, LocalIntent, LocalWriteAuthority, LocalWriteOutcome, MoveBlockIntent, ProjectionReplace,
  ReplaceEquationSourceRangeIntent, ReplaceImageAltTextIntent, ReplaceImageCaptionIntent, ReplaceMatch, ReplaceMatchesIntent,
  ReplaceObjectIntent, SelectionSnapshot, SetImageLayoutIntent, SetMarksIntent, SetParagraphStyleIntent, SetParagraphStylesIntent,
  SplitParagraphIntent, TableIntent, TextAnchor, UndoOutcome, WriteRejected,
};
