//! The grid PROJECTION: the read model every consumer (editor, previews,
//! summaries) sees. Materialized from the Loro doc by
//! [`crate::loro_projection`]; never the write path (excel flow spec §1 —
//! the CRDT is the single source of truth, projections are derived).

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::format::{ArgumentSide, CellId, ColumnId, FlowFormat, RowId, SheetId, SheetTypeId, StrokeId};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationOriginator(pub String);

/// A point in STROKE-LOCAL pixel space (zoom 1). Annotation geometry is a
/// constant point set; the grid never projects into it (rigid-body law, D6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
  pub x: f32,
  pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StrokeRect {
  pub min: StrokePoint,
  pub max: StrokePoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
  pub color_rgba: u32,
  pub width: f32,
  pub opacity: f32,
}

/// The ONE grid-projected point of a stroke (D6): the slot under the
/// stroke's first point at capture time, plus a pixel offset from that
/// slot's origin at zoom 1. Everything else about the stroke is anchored
/// relative to this point, so structure changes can only TRANSLATE the ink —
/// deformation would require per-point grid dependence the shape does not
/// have.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridAnchor {
  pub row_id: RowId,
  pub column_id: ColumnId,
  pub offset: StrokePoint,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationStroke {
  pub id: StrokeId,
  pub sheet_id: SheetId,
  pub originator: AnnotationOriginator,
  pub anchor: GridAnchor,
  /// Stroke-local pixels relative to the anchor point.
  pub points: Vec<StrokePoint>,
  pub style: StrokeStyle,
  /// Stroke-local bounding box.
  pub bbox: StrokeRect,
}

/// A11: the stroke blob's leading version byte. Postcard is not
/// self-describing, so without this byte ANY evolution of `AnnotationStroke`
/// silently strands every stroke ever written. Bump on struct change and
/// branch in `decode_stroke`.
pub const STROKE_BLOB_VERSION: u8 = 1;

/// Encode a stroke for the `flow.annotations` map: version byte + postcard.
pub fn encode_stroke(stroke: &AnnotationStroke) -> Result<Vec<u8>, postcard::Error> {
  let mut bytes = vec![STROKE_BLOB_VERSION];
  bytes.extend(postcard::to_allocvec(stroke)?);
  Ok(bytes)
}

/// Decode a stroke blob. Tries the versioned framing first; falls back to the
/// pre-version raw-postcard framing (dev-era files — nothing shipped) so no
/// existing ink is stranded. `None` = undecodable; callers warn-and-skip.
pub fn decode_stroke(bytes: &[u8]) -> Option<AnnotationStroke> {
  if let Some((&STROKE_BLOB_VERSION, rest)) = bytes.split_first()
    && let Ok(stroke) = postcard::from_bytes::<AnnotationStroke>(rest)
  {
    return Some(stroke);
  }
  postcard::from_bytes::<AnnotationStroke>(bytes).ok()
}

/// Cached, cheap read model of a cell's rich text — derived from the cell's
/// materialized `DocumentProjection` whenever the cell's flow changes, and
/// shared by `Arc` so board-projection clones are metadata-priced.
#[derive(Clone, Debug, PartialEq)]
pub struct CellSummary {
  pub summary_text: Arc<str>,
  pub uses_summary_projection: bool,
  /// Every non-empty run in the cell carries strikethrough — the board-level
  /// "struck" state (strike is a text mark, so concurrent typing merges
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

/// A cell: durable identity + a grid ADDRESS (D1 placement map). Rich text
/// lives in the Loro doc only; `summary` is the derived read model.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
  pub id: CellId,
  pub row_id: RowId,
  pub column_id: ColumnId,
  pub summary: CellSummary,
  /// Q-21/F2: where this card came from, when it was dropped in from a
  /// document or the tub — enough to jump back to the evidence.
  pub source: Option<CellSource>,
}

/// Q-21/F2: a flowed card's provenance. LWW blob on the cell record.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CellSource {
  /// The source document's path (a .db8, usually).
  pub path: String,
  /// The tub search-unit id, when the drop came from search.
  pub unit: Option<String>,
  /// An encoded durable cursor into the source document, when known.
  pub cursor: Option<Vec<u8>>,
}

/// A sheet column as materialized (per-sheet records seeded from the sheet
/// type at creation; user columns join the same list).
#[derive(Clone, Debug, PartialEq)]
pub struct GridColumn {
  pub id: ColumnId,
  pub label: String,
  pub side: ArgumentSide,
  /// Manual width override; `None` = automatic.
  pub width: Option<f32>,
}

/// One sheet-global row. `cells` is aligned with `Sheet::columns` — direct
/// grid indexing for the renderer, no lookups on the paint path.
#[derive(Clone, Debug, PartialEq)]
pub struct GridRow {
  pub id: RowId,
  /// D4 manual height override; `None` = autofit.
  pub height_override: Option<f32>,
  pub cells: Vec<Option<Cell>>,
}

impl GridRow {
  pub fn is_empty(&self) -> bool {
    self.cells.iter().all(Option::is_none)
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
  pub id: SheetId,
  pub name: String,
  pub sheet_type_id: SheetTypeId,
  pub columns: Vec<GridColumn>,
  pub rows: Vec<GridRow>,
  pub annotations: Vec<AnnotationStroke>,
}

impl Sheet {
  pub fn column_index(&self, column_id: ColumnId) -> Option<usize> {
    self.columns.iter().position(|column| column.id == column_id)
  }

  pub fn row_index(&self, row_id: RowId) -> Option<usize> {
    self.rows.iter().position(|row| row.id == row_id)
  }

  pub fn slot(&self, row_ix: usize, column_ix: usize) -> Option<&Cell> {
    self.rows.get(row_ix)?.cells.get(column_ix)?.as_ref()
  }

  pub fn slot_by_ids(&self, row_id: RowId, column_id: ColumnId) -> Option<&Cell> {
    self.slot(self.row_index(row_id)?, self.column_index(column_id)?)
  }

  pub fn cells(&self) -> impl Iterator<Item = &Cell> {
    self
      .rows
      .iter()
      .flat_map(|row| row.cells.iter().filter_map(Option::as_ref))
  }

  pub fn find_cell(&self, cell_id: CellId) -> Option<&Cell> {
    self.cells().find(|cell| cell.id == cell_id)
  }

  /// (row index, column index) of a cell.
  pub fn cell_position(&self, cell_id: CellId) -> Option<(usize, usize)> {
    for (row_ix, row) in self.rows.iter().enumerate() {
      for (column_ix, slot) in row.cells.iter().enumerate() {
        if slot.as_ref().is_some_and(|cell| cell.id == cell_id) {
          return Some((row_ix, column_ix));
        }
      }
    }
    None
  }

  /// Resolve a stroke anchor to a live (row index, column index) — the ONE
  /// grid projection an annotation gets (D6). Deterministic fallbacks for
  /// dead anchors: rows fall back to the LAST live row (the nearest thing to
  /// "just above where it was" that needs no history), columns to the first
  /// column; an empty grid resolves to origin (0, 0).
  pub fn resolve_anchor(&self, anchor: &GridAnchor) -> (usize, usize) {
    let row_ix = self
      .row_index(anchor.row_id)
      .unwrap_or_else(|| self.rows.len().saturating_sub(1));
    let column_ix = self.column_index(anchor.column_id).unwrap_or(0);
    (row_ix, column_ix)
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowBoardProjection {
  pub format: FlowFormat,
  pub sheets: Vec<Sheet>,
  /// E10: round identity from `flow.meta` (LWW per field).
  pub round: RoundMetadata,
}

/// A cheap, stable drift signature for a board — a soak/self-check tripwire so
/// "live incremental board == fresh full rematerialization" reduces to a `u64`
/// compare (see the flow fidelity hook). Hashes the deterministic `Debug`
/// rendering, which sidesteps the `f32` fields that block a derived `Hash`.
#[must_use]
pub fn board_hash(board: &FlowBoardProjection) -> u64 {
  use std::hash::{Hash as _, Hasher as _};
  let mut hasher = std::collections::hash_map::DefaultHasher::new();
  format!("{board:?}").hash(&mut hasher);
  hasher.finish()
}

impl Default for FlowBoardProjection {
  fn default() -> Self {
    Self {
      format: FlowFormat::policy_debate(),
      sheets: Vec::new(),
      round: RoundMetadata::default(),
    }
  }
}

/// E10: the round's identity — "round 3 vs Northwestern, judge X, we won" —
/// six LWW string fields under `flow.meta`. Empty string = unset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundMetadata {
  pub tournament: String,
  pub round: String,
  pub opponent: String,
  pub judge: String,
  pub side: String,
  pub result: String,
}

impl RoundMetadata {
  pub fn is_empty(&self) -> bool {
    self.tournament.is_empty()
      && self.round.is_empty()
      && self.opponent.is_empty()
      && self.judge.is_empty()
      && self.side.is_empty()
      && self.result.is_empty()
  }

  /// A compact one-line identity for tab titles and recents:
  /// "Aldrich R3 vs Northwestern".
  pub fn summary(&self) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !self.tournament.is_empty() {
      parts.push(self.tournament.clone());
    }
    if !self.round.is_empty() {
      parts.push(format!("R{}", self.round.trim_start_matches(['r', 'R'])));
    }
    if !self.opponent.is_empty() {
      parts.push(format!("vs {}", self.opponent));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
  }
}

impl FlowBoardProjection {
  pub fn sheet(&self, id: SheetId) -> Option<&Sheet> {
    self.sheets.iter().find(|sheet| sheet.id == id)
  }

  /// Structural invariants the normalized materializer guarantees; used by
  /// tests and the schema-level write wrapper as a belt-and-suspenders check.
  pub fn validate(&self) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    if !ids.insert(self.format.id) {
      bail!("duplicate format id");
    }
    for sheet_type in &self.format.sheet_types {
      if !ids.insert(sheet_type.id) || sheet_type.columns.is_empty() {
        bail!("invalid sheet type {}", sheet_type.name);
      }
    }
    for sheet in &self.sheets {
      if !ids.insert(sheet.id) {
        bail!("duplicate sheet id");
      }
      if sheet.columns.is_empty() {
        bail!("sheet {} has no columns", sheet.name);
      }
      let mut column_ids = HashSet::new();
      for column in &sheet.columns {
        if !column_ids.insert(column.id) {
          bail!("sheet {} contains duplicate column ids", sheet.name);
        }
      }
      let mut row_ids = HashSet::new();
      let mut cell_ids = HashSet::new();
      for (row_ix, row) in sheet.rows.iter().enumerate() {
        if !row_ids.insert(row.id) {
          bail!("sheet {} contains duplicate row ids", sheet.name);
        }
        if row.cells.len() != sheet.columns.len() {
          bail!("sheet {} row {row_ix} is not aligned with its columns", sheet.name);
        }
        for (column_ix, slot) in row.cells.iter().enumerate() {
          let Some(cell) = slot else { continue };
          if !cell_ids.insert(cell.id) {
            bail!("sheet {} contains duplicate cell ids", sheet.name);
          }
          if cell.row_id != row.id || cell.column_id != sheet.columns[column_ix].id {
            bail!("cell {} address disagrees with its slot", cell.id);
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
/// converge deterministically, report what was mended (excel flow spec §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowDefect {
  SheetMissingFromOrder { sheet: SheetId },
  OrderEntryMissingSheet { entry: String },
  SheetTypeUnknown { sheet: SheetId },
  ColumnMissingFromOrder { sheet: SheetId, column: ColumnId },
  OrderEntryMissingColumn { sheet: SheetId, entry: String },
  ColumnsSeededFromType { sheet: SheetId },
  OrderEntryInvalidRow { sheet: SheetId, entry: String },
  /// A cell referenced a row absent from `row_order`; the row was
  /// materialized as a phantom at the bottom of the grid (repair pass writes
  /// it into the order for real).
  RowMissingFromOrder { sheet: SheetId, row: RowId },
  /// A cell record had no usable row address at all; it was assigned its
  /// deterministic bump row.
  CellRowInvalid { cell: CellId },
  UnknownColumnReassigned { cell: CellId },
  /// D2: this cell lost a slot collision and was bumped into its
  /// deterministically synthesized row below the contested one.
  SlotCollisionBumped { cell: CellId, row: RowId, column: ColumnId },
  CellFlowInvalid { cell: CellId, error: String },
}

impl std::fmt::Display for FlowDefect {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::SheetMissingFromOrder { sheet } => write!(f, "sheet {sheet} was missing from the order list"),
      Self::OrderEntryMissingSheet { entry } => write!(f, "sheet order entry `{entry}` has no record"),
      Self::SheetTypeUnknown { sheet } => write!(f, "sheet {sheet} references an unknown sheet type"),
      Self::ColumnMissingFromOrder { sheet, column } => write!(f, "column {column} was missing from sheet {sheet}'s order"),
      Self::OrderEntryMissingColumn { sheet, entry } => write!(f, "column order entry `{entry}` in sheet {sheet} has no record"),
      Self::ColumnsSeededFromType { sheet } => write!(f, "sheet {sheet} had no live columns; seeded from its sheet type"),
      Self::OrderEntryInvalidRow { sheet, entry } => write!(f, "row order entry `{entry}` in sheet {sheet} is not a row id"),
      Self::RowMissingFromOrder { sheet, row } => write!(f, "row {row} was missing from sheet {sheet}'s order"),
      Self::CellRowInvalid { cell } => write!(f, "cell {cell} had no usable row address"),
      Self::UnknownColumnReassigned { cell } => write!(f, "cell {cell} referenced a column outside its sheet"),
      Self::SlotCollisionBumped { cell, row, column } => {
        write!(f, "cell {cell} lost the slot ({row}, {column}) and was bumped to a synthesized row")
      },
      Self::CellFlowInvalid { cell, error } => write!(f, "cell {cell} rich text failed to materialize: {error}"),
    }
  }
}
