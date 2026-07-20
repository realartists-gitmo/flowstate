//! The flow's FORMAT crate (excel flow spec, `Junk/flowstate_excel_flow_spec.md`):
//! the .fl0 v3 grid schema, the total normalized grid materializer, the
//! plain-data intent vocabulary and its executors, and persistence. The
//! collaborative runtime lives in `flowstate-collab/src/flow` and builds on
//! exactly these pieces — one schema, one executor, one materializer.

mod document;
mod format;
mod intents;
pub mod loro_projection;
pub mod loro_schema;
pub mod mutate;
pub mod persistence;
mod projection;
#[cfg(test)]
mod tests;

pub use document::{FlowDocument, FlowFrontier};
pub use format::{ArgumentSide, CellId, ColumnDefinition, ColumnId, FlowFormat, FormatId, RowId, SheetId, SheetTypeDefinition, SheetTypeId, StrokeId};
pub use intents::{AnnotationScope, CellSeed, FlowIntent};
pub use loro::VersionVector;
pub use loro_projection::{MaterializedBoard, board_from_loro, board_from_loro_cached, bump_row_id, cell_document, derive_cell_summary};
pub use mutate::MutationReport;
pub use persistence::{decode as decode_flow_document, encode as encode_flow_document, load_flow_document, load_flow_snapshot, save_flow_document};
pub use projection::{
  AnnotationOriginator, AnnotationStroke, Cell, CellSummary, FlowBoardProjection, FlowDefect, GridAnchor, GridColumn, GridRow, Sheet, StrokePoint,
  StrokeRect, StrokeStyle,
};
