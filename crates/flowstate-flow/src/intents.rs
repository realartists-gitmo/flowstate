//! The flow's INTENT vocabulary (flow architecture spec Part 2.2): plain-data
//! descriptions of every mutation, constructed by the app without any runtime
//! types. The runtime (`flowstate-collab/src/flow`) resolves and executes
//! them under the write gate; the schema-level `FlowDocument::apply_intent`
//! executes them directly for solo/fixture/test use — one executor, two
//! entrances.

use flowstate_document::InputParagraph;

use crate::format::{CellId, SheetId, SheetTypeId};
use crate::projection::{AnnotationOriginator, AnnotationStroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativePosition {
  Before,
  After,
}

/// Where a moved cell (and its thread) lands. `RootInColumn`'s
/// `insertion_index` is a POSITIONAL HINT resolved and clamped inside the
/// executor against the live order — positional-as-hint, identity-anchored
/// variants preferred (UX spec: pads/keyboard emit the identity forms).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowDropIntent {
  BeforeSibling(CellId),
  AfterSibling(CellId),
  FirstChildOf(CellId),
  LastChildOf(CellId),
  RootInColumn { column_index: usize, insertion_index: usize },
}

/// Where a NEW cell lands (collapses the four legacy add_* entry points).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellPlacement {
  Before(CellId),
  After(CellId),
  FirstChildOf(CellId),
  LastChildOf(CellId),
  ColumnTop { column_index: usize },
  SheetEnd { column_index: usize },
}

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
  AddCell {
    sheet_id: SheetId,
    cell_id: CellId,
    placement: CellPlacement,
    seed: CellSeed,
  },
  DeleteCell {
    sheet_id: SheetId,
    cell_id: CellId,
  },
  MoveCellSubtree {
    sheet_id: SheetId,
    cell_id: CellId,
    drop: FlowDropIntent,
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
      Self::AddCell { .. } => "flow.add-cell",
      Self::DeleteCell { .. } => "flow.delete-cell",
      Self::MoveCellSubtree { .. } => "flow.move-cell-subtree",
      Self::SetCellStruck { .. } => "flow.set-cell-struck",
      Self::EnsureCellEditable { .. } => "flow.ensure-cell-editable",
      Self::ReplaceCellContent { .. } => "flow.replace-cell-content",
      Self::AddAnnotation { .. } => "flow.add-annotation",
      Self::DeleteAnnotation { .. } => "flow.delete-annotation",
      Self::ClearAnnotations { .. } => "flow.clear-annotations",
      Self::CellText { .. } => "cell.text",
    }
  }

  /// Column ids are per-sheet; `ColumnId` params in intents use indices into the
  /// sheet type's column list, so intents stay valid across format lookups.
  pub fn sheet_id(&self) -> Option<SheetId> {
    match self {
      Self::CreateSheet { sheet_id, .. }
      | Self::RenameSheet { sheet_id, .. }
      | Self::DeleteSheet { sheet_id }
      | Self::MoveSheet { sheet_id, .. }
      | Self::AddCell { sheet_id, .. }
      | Self::DeleteCell { sheet_id, .. }
      | Self::MoveCellSubtree { sheet_id, .. }
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
