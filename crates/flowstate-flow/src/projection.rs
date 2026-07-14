//! The materialized flow BOARD projection (.fl0 v2 spec, Part A).
//!
//! What `DocumentProjection` is to a .db8 body, `FlowBoardProjection` is to a
//! flow board: a plain-data, deterministic materialization of canonical Loro
//! state. Cell rich text is NOT embedded — each cell carries a cached
//! [`CellSummary`] (the board renders summaries; the full per-cell
//! `DocumentProjection` is materialized on demand by the runtime when a cell
//! editor opens).

use std::sync::Arc;

use crate::format::{AnnotationStroke, CellId, ColumnId, FlowFormat, SheetId, SheetTypeId};

/// Board-visible digest of one cell's rich text, derived from the cell's
/// materialized rows (the former `Cell::summary_text` /
/// `uses_summary_projection` logic, computed once per text change).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSummary {
  /// Tag/undertag/analytic rows + cite runs in document order, joined by
  /// newlines; the full text when no summary rows exist.
  pub summary_text: Arc<str>,
  /// Whether the cell renders through the summary projection (any summary
  /// row or cite run present).
  pub uses_summary_projection: bool,
  /// Whether every (non-empty) run is struck through.
  pub struck: bool,
  /// Whether the cell has no visible text at all.
  pub is_empty: bool,
}

impl Default for CellSummary {
  fn default() -> Self {
    Self {
      summary_text: Arc::from(""),
      uses_summary_projection: false,
      struck: false,
      is_empty: true,
    }
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
  pub id: CellId,
  pub column_id: ColumnId,
  pub parent_id: Option<CellId>,
  pub summary: CellSummary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
  pub id: SheetId,
  pub name: String,
  pub sheet_type_id: SheetTypeId,
  /// Flat sheet order (the canonical DFS linearization: every subtree
  /// contiguous, roots and siblings in order-list order).
  pub cells: Vec<Cell>,
  pub annotations: Vec<AnnotationStroke>,
}

impl Sheet {
  #[must_use]
  pub fn cell(&self, id: CellId) -> Option<&Cell> {
    self.cells.iter().find(|cell| cell.id == id)
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowBoardProjection {
  pub format: FlowFormat,
  pub sheets: Vec<Sheet>,
}

impl FlowBoardProjection {
  #[must_use]
  pub fn sheet(&self, id: SheetId) -> Option<&Sheet> {
    self.sheets.iter().find(|sheet| sheet.id == id)
  }

  #[must_use]
  pub fn sheet_mut(&mut self, id: SheetId) -> Option<&mut Sheet> {
    self.sheets.iter_mut().find(|sheet| sheet.id == id)
  }

  /// The sheet owning `cell`, plus the cell itself.
  #[must_use]
  pub fn cell(&self, cell: CellId) -> Option<(&Sheet, &Cell)> {
    self
      .sheets
      .iter()
      .find_map(|sheet| sheet.cell(cell).map(|found| (sheet, found)))
  }
}
