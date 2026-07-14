//! The flow's FORMAT crate (flow architecture spec Part 2): the .fl0 v2 Loro
//! schema, the total normalized board materializer, the plain-data intent
//! vocabulary and its executors, pure board previews, and persistence. The
//! collaborative runtime lives in `flowstate-collab/src/flow` and builds on
//! exactly these pieces — one schema, one executor, one materializer.

pub mod board_ops;
mod document;
mod format;
mod intents;
pub mod loro_projection;
pub mod loro_schema;
pub mod mutate;
mod persistence;
mod projection;
#[cfg(test)]
mod tests;

pub use document::{FlowDocument, FlowFrontier};
pub use format::{ArgumentSide, CellId, ColumnDefinition, ColumnId, FlowFormat, FormatId, SheetId, SheetTypeDefinition, SheetTypeId, StrokeId};
pub use intents::{AnnotationScope, CellPlacement, CellSeed, FlowDropIntent, FlowIntent, RelativePosition};
pub use loro::VersionVector;
pub use loro_projection::{MaterializedBoard, board_from_loro, cell_document, derive_cell_summary};
pub use mutate::MutationReport;
pub use persistence::{decode as decode_flow_document, encode as encode_flow_document, load_flow_document, save_flow_document};
pub use projection::{
  AnnotationOriginator, AnnotationStroke, BoardPoint, BoardRect, Cell, CellSummary, FlowBoardProjection, FlowDefect, FlowProjection, Sheet,
  StrokeStyle,
};
