//! The plain-data flow intent vocabulary (.fl0 v2 spec, Part B).
//!
//! Every local mutation of a flow document is one of these intents, applied
//! through the flow's gated write path (`flowstate-collab::flow`). The types
//! are pure data — no Loro handles, no projection references — so the editor
//! can construct them without holding the gate, and the fuzz harnesses can
//! generate them.

use gpui_flowtext::local_intents::LocalIntent;
use serde::{Deserialize, Serialize};

use crate::format::{AnnotationOriginator, AnnotationStroke, CellId, SheetId, SheetTypeId, StrokeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativePosition {
  Before,
  After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowDropIntent {
  BeforeSibling(CellId),
  AfterSibling(CellId),
  FirstChildOf(CellId),
  LastChildOf(CellId),
  RootInColumn { column_index: usize, insertion_index: usize },
}

/// Where a NEW cell lands, replacing the former `add_plain_cell` /
/// `add_orphan_at_column_top` / `add_sibling` / `add_response` /
/// `add_first_response` quintet with one resolved-at-commit placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellPlacement {
  /// Root cell appended at the end of the sheet order.
  ColumnEnd { column_index: usize },
  /// Root cell at the very top of the sheet order (`add_orphan_at_column_top`).
  ColumnTop { column_index: usize },
  /// Immediately before/after an existing cell, sharing its column and parent.
  Sibling { of: CellId, position: RelativePosition },
  /// First child of `parent` (before its existing child subtrees).
  FirstResponseTo { parent: CellId },
  /// Last child of `parent` (after every existing descendant).
  ResponseTo { parent: CellId },
}

/// Initial rich-text content for a new cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum CellSeed {
  /// The canonical empty tag-paragraph seed (`seed_cell_flow`).
  #[default]
  Empty,
  /// Full paragraph content (paste of a card, import).
  Paragraphs(Vec<gpui_flowtext::InputParagraph>),
}

/// One local flow intent = one gate hold = one Loro commit = one undo member.
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
    target_index: usize,
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
  /// Toggle-free strike state: `struck` is the desired end state, expressed as
  /// a whole-text `MARK_STRIKETHROUGH` mark so concurrent typing merges under
  /// it char-level.
  SetCellStruck {
    sheet_id: SheetId,
    cell_id: CellId,
    struck: bool,
  },
  /// Restyle the first paragraph to the editable tag style when the cell's
  /// content has no summary-projection row yet (former
  /// `ensure_cell_editable_projection`).
  EnsureCellEditable {
    sheet_id: SheetId,
    cell_id: CellId,
  },
  ReplaceCellContent {
    sheet_id: SheetId,
    cell_id: CellId,
    paragraphs: Vec<gpui_flowtext::InputParagraph>,
  },
  AddAnnotation {
    sheet_id: SheetId,
    stroke: AnnotationStroke,
  },
  DeleteAnnotation {
    sheet_id: SheetId,
    stroke_id: StrokeId,
    originator: AnnotationOriginator,
  },
  ClearAnnotations {
    /// `None` clears the originator's strokes on every sheet.
    sheet_id: Option<SheetId>,
    originator: AnnotationOriginator,
  },
  /// A rich-text intent scoped to one cell's flow, translated from the cell's
  /// `RichTextEditor` by its `FlowCellAuthority`.
  CellText {
    cell_id: CellId,
    intent: LocalIntent,
  },
}

impl FlowIntent {
  /// Commit-message class (field forensics, mirrors `LocalIntent::class`).
  #[must_use]
  pub fn class(&self) -> &'static str {
    match self {
      Self::CreateSheet { .. } => "flow-create-sheet",
      Self::RenameSheet { .. } => "flow-rename-sheet",
      Self::DeleteSheet { .. } => "flow-delete-sheet",
      Self::MoveSheet { .. } => "flow-move-sheet",
      Self::AddCell { .. } => "flow-add-cell",
      Self::DeleteCell { .. } => "flow-delete-cell",
      Self::MoveCellSubtree { .. } => "flow-move-cell-subtree",
      Self::SetCellStruck { .. } => "flow-set-cell-struck",
      Self::EnsureCellEditable { .. } => "flow-ensure-cell-editable",
      Self::ReplaceCellContent { .. } => "flow-replace-cell-content",
      Self::AddAnnotation { .. } => "flow-add-annotation",
      Self::DeleteAnnotation { .. } => "flow-delete-annotation",
      Self::ClearAnnotations { .. } => "flow-clear-annotations",
      Self::CellText { .. } => "flow-cell-text",
    }
  }
}
