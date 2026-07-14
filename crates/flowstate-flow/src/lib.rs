mod collaboration;
mod document;
mod operations;
mod persistence;

// ---- .fl0 v2 (CRDT-first) — spec Parts A/B ---------------------------------
pub mod board_ops;
pub mod format;
pub mod intents;
pub mod loro_projection;
pub mod loro_schema;
pub mod projection;

pub use collaboration::{
  FlowCommitResult, FlowFrontier, FlowProjectionSnapshot, FlowRuntimeEvent, FlowTransactionId, FlowUpdateBytes, StaleFlowProjectionError,
};
pub use document::db8_bytes as cell_db8_bytes;
pub use document::{
  AnnotationOriginator, AnnotationStroke, ArgumentSide, BoardPoint, BoardRect, Cell, CellId, ColumnDefinition, ColumnId, FlowDocument,
  FlowFormat, FlowProjection, FormatId, Sheet, SheetId, SheetTypeDefinition, SheetTypeId, StrokeId, StrokeStyle,
};
pub use intents::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent, RelativePosition};
pub use loro::VersionVector;
pub use loro_projection::FlowDefect;
pub use persistence::{
  FL0_VERSION, decode as decode_flow_document, decode_fl0_snapshot, encode as encode_flow_document, encode_fl0_snapshot, load_flow_document,
  read_fl0, save_flow_document, write_fl0,
};
pub use projection::{CellSummary, FlowBoardProjection};
