//! The board PROJECTION: the read model every consumer (editor, previews,
//! summaries) sees. Materialized from the Loro doc by
//! [`crate::loro_projection`]; never the write path (flow architecture spec
//! Part 2 — the CRDT is the single source of truth, projections are derived).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};

use crate::format::{CellId, ColumnId, FlowFormat, SheetId, SheetTypeId, StrokeId};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationOriginator(pub String);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardPoint {
  pub x: f32,
  pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardRect {
  pub min: BoardPoint,
  pub max: BoardPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
  pub color_rgba: u32,
  pub width: f32,
  pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationStroke {
  pub id: StrokeId,
  pub sheet_id: SheetId,
  pub originator: AnnotationOriginator,
  pub points: Vec<BoardPoint>,
  pub style: StrokeStyle,
  pub bbox: BoardRect,
}

/// Cached, cheap read model of a cell's rich text — derived from the cell's
/// materialized `DocumentProjection` whenever the cell's flow changes, and
/// shared by `Arc` so board-projection clones are metadata-priced.
#[derive(Clone, Debug, PartialEq)]
pub struct CellSummary {
  pub summary_text: Arc<str>,
  pub uses_summary_projection: bool,
  /// Every non-empty run in the cell carries strikethrough — the board-level
  /// "struck" state (v2: strike is a text mark, so concurrent typing merges
  /// under it char-level instead of clobbering a whole-cell blob).
  pub struck: bool,
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

/// A cell's board-level identity: WHERE it lives (column + flat sheet order,
/// held by the sheet) and WHO it answers (`parent_id`). Rich text lives in
/// the Loro doc only; `summary` is the derived read model.
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
  /// Flat sheet order (the `cell_order` `MovableList`, normalized).
  pub cells: Vec<Cell>,
  pub annotations: Vec<AnnotationStroke>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowBoardProjection {
  pub format: FlowFormat,
  pub sheets: Vec<Sheet>,
}

/// Transitional alias for pre-v2 call sites; new code names the board.
pub type FlowProjection = FlowBoardProjection;

impl Default for FlowBoardProjection {
  fn default() -> Self {
    Self {
      format: FlowFormat::policy_debate(),
      sheets: Vec::new(),
    }
  }
}

impl FlowBoardProjection {
  pub fn sheet(&self, id: SheetId) -> Option<&Sheet> {
    self.sheets.iter().find(|sheet| sheet.id == id)
  }

  pub fn sheet_column_ids(&self, sheet_id: SheetId) -> anyhow::Result<Vec<ColumnId>> {
    let sheet = self.sheet(sheet_id).context("unknown sheet")?;
    let definition = self
      .format
      .sheet_type(sheet.sheet_type_id)
      .context("unknown sheet type")?;
    Ok(definition.columns.iter().map(|column| column.id).collect())
  }

  /// Structural invariants the normalized materializer guarantees; used by
  /// tests and the schema-level write wrapper as a belt-and-suspenders check.
  /// (Unlike v1 this never deserializes cell content — content correctness is
  /// the cell materializer's concern.)
  pub fn validate(&self) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    let mut column_levels = HashMap::new();
    if !ids.insert(self.format.id) {
      bail!("duplicate format id");
    }
    for sheet_type in &self.format.sheet_types {
      if !ids.insert(sheet_type.id) || sheet_type.columns.is_empty() {
        bail!("invalid sheet type {}", sheet_type.name);
      }
      for (level, column) in sheet_type.columns.iter().enumerate() {
        if !ids.insert(column.id) {
          bail!("duplicate column id");
        }
        column_levels.insert(column.id, level);
      }
    }
    for sheet in &self.sheets {
      let definition = self
        .format
        .sheet_type(sheet.sheet_type_id)
        .with_context(|| format!("sheet {} references unknown type", sheet.name))?;
      if !ids.insert(sheet.id) {
        bail!("duplicate sheet id");
      }
      let valid_columns: HashSet<_> = definition.columns.iter().map(|column| column.id).collect();
      let cells: HashMap<_, _> = sheet.cells.iter().map(|cell| (cell.id, cell)).collect();
      if cells.len() != sheet.cells.len() {
        bail!("sheet {} contains duplicate cell ids", sheet.name);
      }
      for cell in &sheet.cells {
        if !valid_columns.contains(&cell.column_id) {
          bail!("cell references a column outside its sheet type");
        }
        if let Some(parent_id) = cell.parent_id {
          let parent = cells
            .get(&parent_id)
            .context("cell references missing parent")?;
          let child_level = column_levels[&cell.column_id];
          let parent_level = column_levels[&parent.column_id];
          if child_level != parent_level + 1 {
            bail!("parent-child link must connect adjacent columns");
          }
        }
      }
      for column in &definition.columns {
        let column_cells: Vec<_> = sheet
          .cells
          .iter()
          .filter(|cell| cell.column_id == column.id)
          .collect();
        let mut completed_parents = HashSet::new();
        let mut current_parent = None;
        for cell in column_cells {
          if cell.parent_id != current_parent {
            if let Some(parent) = current_parent {
              completed_parents.insert(parent);
            }
            if cell
              .parent_id
              .is_some_and(|parent| completed_parents.contains(&parent))
            {
              bail!("orphan or unrelated cell breaks a sibling run");
            }
            current_parent = cell.parent_id;
          }
        }
      }
      if sheet
        .annotations
        .iter()
        .any(|stroke| stroke.sheet_id != sheet.id)
      {
        bail!("annotation references the wrong sheet");
      }
    }
    Ok(())
  }
}

/// Defects the TOTAL materializer repaired (in projection space) while
/// normalizing a merged state — the .db8 quarantine philosophy: never fail,
/// converge deterministically, report what was mended (flow architecture spec
/// Part 2.1, normalization law).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowDefect {
  SheetMissingFromOrder { sheet: SheetId },
  OrderEntryMissingSheet { entry: String },
  SheetTypeUnknown { sheet: SheetId },
  CellMissingFromOrder { sheet: SheetId, cell: CellId },
  OrderEntryMissingCell { sheet: SheetId, entry: String },
  ParentCycleBroken { cell: CellId },
  DanglingParentOrphaned { cell: CellId, parent: CellId },
  ColumnAdjacencyOrphaned { cell: CellId },
  RunSplitRegrouped { cell: CellId },
  UnknownColumnReassigned { cell: CellId },
  CellFlowInvalid { cell: CellId, error: String },
}

impl std::fmt::Display for FlowDefect {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::SheetMissingFromOrder { sheet } => write!(f, "sheet {sheet} was missing from the order list"),
      Self::OrderEntryMissingSheet { entry } => write!(f, "sheet order entry `{entry}` has no record"),
      Self::SheetTypeUnknown { sheet } => write!(f, "sheet {sheet} references an unknown sheet type"),
      Self::CellMissingFromOrder { sheet, cell } => write!(f, "cell {cell} was missing from sheet {sheet}'s order"),
      Self::OrderEntryMissingCell { sheet, entry } => write!(f, "cell order entry `{entry}` in sheet {sheet} has no record"),
      Self::ParentCycleBroken { cell } => write!(f, "parent cycle broken at cell {cell}"),
      Self::DanglingParentOrphaned { cell, parent } => write!(f, "cell {cell} referenced missing parent {parent}"),
      Self::ColumnAdjacencyOrphaned { cell } => write!(f, "cell {cell} orphaned: parent link crossed non-adjacent columns"),
      Self::RunSplitRegrouped { cell } => write!(f, "cell {cell} relocated to regroup a split sibling run"),
      Self::UnknownColumnReassigned { cell } => write!(f, "cell {cell} referenced a column outside its sheet type"),
      Self::CellFlowInvalid { cell, error } => write!(f, "cell {cell} rich text failed to materialize: {error}"),
    }
  }
}
