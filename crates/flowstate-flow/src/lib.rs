//! The .fl0 flow FORMAT crate (CRDT-first, spec Part A) — what
//! `flowstate-document` is to `.db8`: the Loro container schema, the total
//! board materializer + normalization law, the plain-data intent vocabulary,
//! the pure board operations behind drag preview AND commit, and the FLOWFL0
//! v2 container. The gated write path lives in `flowstate-collab::flow`.

pub mod board_ops;
pub mod format;
pub mod intents;
pub mod loro_projection;
pub mod loro_schema;
mod persistence;
pub mod projection;

pub use format::{
  AnnotationOriginator, AnnotationStroke, ArgumentSide, BoardPoint, BoardRect, CellId, ColumnDefinition, ColumnId, FlowFormat, FormatId,
  SheetId, SheetTypeDefinition, SheetTypeId, StrokeId, StrokeStyle,
};
pub use intents::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent, RelativePosition};
pub use loro::VersionVector;
pub use loro_projection::FlowDefect;
pub use persistence::{FL0_VERSION, decode_fl0_snapshot, encode_fl0_snapshot, read_fl0, write_fl0};
pub use projection::{Cell, CellSummary, FlowBoardProjection, Sheet};

pub const FLOW_EXTENSION: &str = "fl0";
