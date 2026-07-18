//! The flow's INTENT vocabulary (excel flow spec §2): plain-data descriptions
//! of every grid mutation, constructed by the app without any runtime types.
//! The runtime (`flowstate-collab/src/flow`) resolves and executes them under
//! the write gate; the schema-level `FlowDocument::apply_intent` executes
//! them directly for solo/fixture/test use — one executor, two entrances.

use flowstate_document::InputParagraph;

use crate::format::{ArgumentSide, CellId, ColumnId, RowId, SheetId, SheetTypeId};
use crate::projection::{AnnotationOriginator, AnnotationStroke};

/// Initial rich text for a new cell.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CellSeed {
  /// One empty TAG paragraph (the shipped `Cell::plain` seed).
  #[default]
  Empty,
  Paragraphs(Vec<InputParagraph>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationScope {
  Sheet(SheetId),
  AllSheets,
}

#[derive(Clone, Debug)]
pub enum FlowIntent {
  CreateSheet {
    sheet_id: SheetId,
    name: String,
    sheet_type_id: SheetTypeId,
  },
  RenameSheet {
    sheet_id: SheetId,
    name: String,
  },
  DeleteSheet {
    sheet_id: SheetId,
  },
  MoveSheet {
    sheet_id: SheetId,
    /// Identity anchor: land immediately before this sheet; `None` = end.
    before: Option<SheetId>,
  },
  /// New rows enter the sheet-global order immediately before `before`
  /// (`None` = end). Fresh ids are minted by the caller so intents replay
  /// deterministically.
  InsertRows {
    sheet_id: SheetId,
    before: Option<RowId>,
    row_ids: Vec<RowId>,
  },
  /// Move a (contiguous-in-intent) run of rows before the anchor, preserving
  /// their relative order. Order-list `mov` ops only — never touches cells.
  MoveRows {
    sheet_id: SheetId,
    row_ids: Vec<RowId>,
    before: Option<RowId>,
  },
  /// Deletes the rows, every cell resident in them, and their height
  /// overrides.
  DeleteRows {
    sheet_id: SheetId,
    row_ids: Vec<RowId>,
  },
  /// D4: manual row height override; `None` clears back to autofit.
  SetRowHeight {
    sheet_id: SheetId,
    row_id: RowId,
    height: Option<f32>,
  },
  AddColumn {
    sheet_id: SheetId,
    column_id: ColumnId,
    label: String,
    side: ArgumentSide,
    /// Identity anchor: land immediately before this column; `None` = end.
    before: Option<ColumnId>,
  },
  RenameColumn {
    sheet_id: SheetId,
    column_id: ColumnId,
    label: String,
  },
  MoveColumn {
    sheet_id: SheetId,
    column_id: ColumnId,
    before: Option<ColumnId>,
  },
  /// Deletes the column and every cell resident in it. The last column of a
  /// sheet cannot be deleted.
  DeleteColumn {
    sheet_id: SheetId,
    column_id: ColumnId,
  },
  SetColumnWidth {
    sheet_id: SheetId,
    column_id: ColumnId,
    /// `None` clears back to automatic width.
    width: Option<f32>,
  },
  /// A cell is born AT an address (D1 placement map). Rejects if the slot is
  /// occupied at execute time; concurrent-merge collisions are the
  /// normalizer's job (D2 bump-down), never the executor's.
  AddCell {
    sheet_id: SheetId,
    cell_id: CellId,
    row_id: RowId,
    column_id: ColumnId,
    seed: CellSeed,
  },
  DeleteCell {
    sheet_id: SheetId,
    cell_id: CellId,
  },
  /// The move: two LWW register writes on the cell, nothing else. The nested
  /// text container is never touched, so concurrent typing merges through a
  /// move (D1). Rejects if the target slot is occupied by another cell.
  SetCellAddress {
    sheet_id: SheetId,
    cell_id: CellId,
    row_id: RowId,
    column_id: ColumnId,
  },
  /// Exchange two cells' addresses in ONE commit (drag-onto-occupied). Both
  /// LWW register writes land atomically, so neither cell transiently sees the
  /// other's occupied slot — a two-step swap through `SetCellAddress` would be
  /// rejected by the per-write occupancy guard. Lossless: the occupant takes
  /// the dragged cell's vacated address.
  SwapCells {
    sheet_id: SheetId,
    a: CellId,
    b: CellId,
  },
  /// Reposition many cells in ONE atomic commit — the block-swap primitive.
  /// The whole placement set lands together, so a rigid permutation (cells
  /// moving +Δ while the cards they displace slide −Δ into the vacated slots)
  /// never trips the per-write occupancy guard. Rejects if the final
  /// arrangement would put two cells on one address, or collide with a cell
  /// outside the placement set.
  SetCellAddresses {
    sheet_id: SheetId,
    placements: Vec<(CellId, RowId, ColumnId)>,
  },
  SetCellStruck {
    sheet_id: SheetId,
    cell_id: CellId,
    struck: bool,
  },
  /// First paragraph becomes TAG so the cell renders in summary mode.
  EnsureCellEditable {
    sheet_id: SheetId,
    cell_id: CellId,
  },
  /// Programmatic full-content replacement (import/paste); interactive typing
  /// rides `CellText` instead.
  ReplaceCellContent {
    sheet_id: SheetId,
    cell_id: CellId,
    paragraphs: Vec<InputParagraph>,
  },
  AddAnnotation {
    stroke: AnnotationStroke,
  },
  DeleteAnnotation {
    sheet_id: SheetId,
    stroke_id: crate::format::StrokeId,
    originator: AnnotationOriginator,
  },
  ClearAnnotations {
    scope: AnnotationScope,
    originator: AnnotationOriginator,
  },
  /// A rich-text intent routed into one cell's flow by the per-cell
  /// authority (runtime-only; the schema executor rejects it — cell text
  /// editing always rides the gate).
  CellText {
    cell_id: CellId,
    intent: flowstate_document::LocalIntent,
  },
}

impl FlowIntent {
  /// Commit-message class (one commit per intent, message = class — the
  /// same discipline as `local_write/commit.rs`).
  pub fn class(&self) -> &'static str {
    match self {
      Self::CreateSheet { .. } => "flow.create-sheet",
      Self::RenameSheet { .. } => "flow.rename-sheet",
      Self::DeleteSheet { .. } => "flow.delete-sheet",
      Self::MoveSheet { .. } => "flow.move-sheet",
      Self::InsertRows { .. } => "flow.insert-rows",
      Self::MoveRows { .. } => "flow.move-rows",
      Self::DeleteRows { .. } => "flow.delete-rows",
      Self::SetRowHeight { .. } => "flow.set-row-height",
      Self::AddColumn { .. } => "flow.add-column",
      Self::RenameColumn { .. } => "flow.rename-column",
      Self::MoveColumn { .. } => "flow.move-column",
      Self::DeleteColumn { .. } => "flow.delete-column",
      Self::SetColumnWidth { .. } => "flow.set-column-width",
      Self::AddCell { .. } => "flow.add-cell",
      Self::DeleteCell { .. } => "flow.delete-cell",
      Self::SetCellAddress { .. } => "flow.set-cell-address",
      Self::SwapCells { .. } => "flow.swap-cells",
      Self::SetCellAddresses { .. } => "flow.set-cell-addresses",
      Self::SetCellStruck { .. } => "flow.set-cell-struck",
      Self::EnsureCellEditable { .. } => "flow.ensure-cell-editable",
      Self::ReplaceCellContent { .. } => "flow.replace-cell-content",
      Self::AddAnnotation { .. } => "flow.add-annotation",
      Self::DeleteAnnotation { .. } => "flow.delete-annotation",
      Self::ClearAnnotations { .. } => "flow.clear-annotations",
      Self::CellText { .. } => "cell.text",
    }
  }

  pub fn sheet_id(&self) -> Option<SheetId> {
    match self {
      Self::CreateSheet { sheet_id, .. }
      | Self::RenameSheet { sheet_id, .. }
      | Self::DeleteSheet { sheet_id }
      | Self::MoveSheet { sheet_id, .. }
      | Self::InsertRows { sheet_id, .. }
      | Self::MoveRows { sheet_id, .. }
      | Self::DeleteRows { sheet_id, .. }
      | Self::SetRowHeight { sheet_id, .. }
      | Self::AddColumn { sheet_id, .. }
      | Self::RenameColumn { sheet_id, .. }
      | Self::MoveColumn { sheet_id, .. }
      | Self::DeleteColumn { sheet_id, .. }
      | Self::SetColumnWidth { sheet_id, .. }
      | Self::AddCell { sheet_id, .. }
      | Self::DeleteCell { sheet_id, .. }
      | Self::SetCellAddress { sheet_id, .. }
      | Self::SwapCells { sheet_id, .. }
      | Self::SetCellAddresses { sheet_id, .. }
      | Self::SetCellStruck { sheet_id, .. }
      | Self::EnsureCellEditable { sheet_id, .. }
      | Self::ReplaceCellContent { sheet_id, .. }
      | Self::DeleteAnnotation { sheet_id, .. } => Some(*sheet_id),
      Self::AddAnnotation { stroke } => Some(stroke.sheet_id),
      Self::ClearAnnotations {
        scope: AnnotationScope::Sheet(sheet_id),
        ..
      } => Some(*sheet_id),
      Self::ClearAnnotations {
        scope: AnnotationScope::AllSheets,
        ..
      }
      | Self::CellText { .. } => None,
    }
  }
}
