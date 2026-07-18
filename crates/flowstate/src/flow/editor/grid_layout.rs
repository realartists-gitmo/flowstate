//! The grid's LAYOUT engine (excel flow spec §5): model-space geometry for a
//! sheet — per-column lefts/widths, per-row tops/heights as prefix sums, the
//! ghost-row run below the last real row, and O(log n) point → slot mapping.
//! Everything is computed in MODEL pixels (zoom 1); render scales uniformly.
//!
//! Rebuilds are invalidation-driven (content epoch / measurements / widths),
//! never per-frame: scroll only ever binary-searches the cached prefix sums.

use std::collections::HashMap;

use flowstate_flow::{Cell, CellId, RowId, Sheet};

pub(super) const DEFAULT_COLUMN_WIDTH: f32 = 280.0;
pub(super) const MIN_ROW_HEIGHT: f32 = 44.0;
pub(super) const MIN_COLUMN_WIDTH: f32 = 90.0;
/// Spreadsheet law: slots tile edge-to-edge — separation is the gridline's
/// job (a paint-time hairline), never layout gaps.
pub(super) const ROW_GAP: f32 = 0.0;
pub(super) const COLUMN_GAP: f32 = 0.0;
pub(super) const BOARD_PADDING: f32 = 0.0;
pub(super) const GUTTER_WIDTH: f32 = 44.0;
pub(super) const HEADER_HEIGHT: f32 = 32.0;
/// Minimum Excel-style run of render-only rows below the last real one; typing
/// into a ghost materializes it (`InsertRows` + `AddCell` as one undo group).
/// The run is virtualized and EXTENDS on demand (see `compute_to`) so it never
/// reads as an arbitrary wall — you can always scroll further into the aether.
pub(super) const GHOST_ROWS: usize = 16;
/// Runaway guard on the extended ghost run (a bad viewport can't allocate an
/// unbounded row table).
const GHOST_ROWS_MAX: usize = 20_000;
pub(super) const GHOST_ROW_HEIGHT: f32 = MIN_ROW_HEIGHT;

// --- Cell rect geometry (screen-space insets, must match `editor.rs` render) ---
// These three define how much of a column's width is chrome vs. the text box.
// The cell render in `editor.rs` and `cell_text_wrap_width` below BOTH read them
// so the wrap width a focused cell's editor is seeded with can never drift from
// the width its idle display element actually wrapped at (see `cell_text_wrap_width`).
/// 1px inset around each cell rect so the gridline underneath stays visible.
pub(super) const CELL_SLOT_INSET: f32 = 1.0;
/// The selection-ring / cell border thickness — a constant 2px per edge.
pub(super) const CELL_BORDER: f32 = 2.0;
/// Padding inside the cell content box, per edge (model px, scaled by zoom).
pub(super) const CELL_CONTENT_PADDING: f32 = 6.0;

/// Screen-space text wrap width for a cell whose column is `column_width` model
/// px wide, at `zoom`. This is the content box the text element (idle
/// `RichTextDocumentElement` OR the focused `RichTextEditor`) receives: the slot
/// width minus the 1px inset, both 2px borders, and both content-padding edges.
///
/// A focused cell's editor is SEEDED with exactly this via `seed_layout_width`,
/// so it wraps identically to the idle display path — otherwise a fresh editor
/// falls back to 900px, wraps to fewer lines, and the autofit row jumps on
/// focus. Keep this in lockstep with the div geometry in `editor.rs`.
pub(super) fn cell_text_wrap_width(column_width: f32, zoom: f32) -> f32 {
  (column_width * zoom - CELL_SLOT_INSET - 2.0 * CELL_BORDER - 2.0 * CELL_CONTENT_PADDING * zoom).max(1.0)
}

/// A cell's measured on-screen height, normalized back to model space so
/// zooming never redefines intrinsic height (T8.12-style identity cache).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CellMeasurement {
  pub model_height: f32,
  screen_height: f32,
  zoom: f32,
}

impl CellMeasurement {
  pub fn new(screen_height: f32, zoom: f32) -> Self {
    Self {
      model_height: screen_height / zoom.max(0.01),
      screen_height,
      zoom,
    }
  }

  /// Returns true when the MODEL height changed (same zoom, new content
  /// size); a zoom change re-baselines without redefining intrinsic height.
  pub fn update(&mut self, screen_height: f32, zoom: f32) -> bool {
    if (self.zoom - zoom).abs() > f32::EPSILON {
      self.screen_height = screen_height;
      self.zoom = zoom;
      return false;
    }
    if (self.screen_height - screen_height).abs() <= 0.5 {
      return false;
    }
    self.screen_height = screen_height;
    self.model_height = screen_height / zoom.max(0.01);
    true
  }
}

/// Model-space grid geometry for one sheet. Origin (0,0) is the top-left of
/// the first slot (the render layer adds gutter/header/padding).
#[derive(Clone, Debug, Default)]
pub(super) struct GridLayout {
  /// Per-column left edge (prefix sums over widths + gaps).
  pub column_lefts: Vec<f32>,
  pub column_widths: Vec<f32>,
  /// Row top edges as prefix sums; `row_tops[real_rows + GHOST_ROWS]` is the
  /// grid's bottom edge.
  pub row_tops: Vec<f32>,
  pub real_rows: usize,
}

impl GridLayout {
  /// The layout with just the minimum ghost run (geometry-only callers).
  pub fn compute(sheet: &Sheet, measurements: &HashMap<CellId, CellMeasurement>) -> Self {
    Self::compute_to(sheet, measurements, 0.0)
  }

  /// D4 row-height law: manual override wins; else autofit = the tallest
  /// occupied cell's measured (or estimated) model height, floored. The ghost
  /// run extends past `min_ghost_bottom` (a model-space y) so a scrolled-down
  /// viewport always finds fresh ghost rows below it.
  pub fn compute_to(sheet: &Sheet, measurements: &HashMap<CellId, CellMeasurement>, min_ghost_bottom: f32) -> Self {
    let mut column_lefts = Vec::with_capacity(sheet.columns.len());
    let mut column_widths = Vec::with_capacity(sheet.columns.len());
    let mut x = 0.0;
    for column in &sheet.columns {
      column_lefts.push(x);
      let width = column.width.unwrap_or(DEFAULT_COLUMN_WIDTH).max(MIN_COLUMN_WIDTH);
      column_widths.push(width);
      x += width + COLUMN_GAP;
    }
    let mut row_tops = Vec::with_capacity(sheet.rows.len() + GHOST_ROWS + 1);
    let mut y = 0.0;
    for row in &sheet.rows {
      row_tops.push(y);
      let height = match row.height_override {
        Some(height) => height.max(12.0),
        None => row
          .cells
          .iter()
          .filter_map(Option::as_ref)
          .map(|cell| measured_cell_height(cell, measurements))
          .fold(MIN_ROW_HEIGHT, f32::max),
      };
      y += height + ROW_GAP;
    }
    // At least GHOST_ROWS ghosts, then keep going until the run passes the
    // requested bottom (capped) — this is what lets the sheet fall away
    // "arbitrarily far" as the user scrolls down.
    let mut ghosts = 0usize;
    while ghosts < GHOST_ROWS || (y < min_ghost_bottom && ghosts < GHOST_ROWS_MAX) {
      row_tops.push(y);
      y += GHOST_ROW_HEIGHT + ROW_GAP;
      ghosts += 1;
    }
    row_tops.push(y);
    Self {
      column_lefts,
      column_widths,
      row_tops,
      real_rows: sheet.rows.len(),
    }
  }

  pub fn total_rows(&self) -> usize {
    self.row_tops.len().saturating_sub(1)
  }

  pub fn total_width(&self) -> f32 {
    match (self.column_lefts.last(), self.column_widths.last()) {
      (Some(left), Some(width)) => left + width,
      _ => 0.0,
    }
  }

  pub fn total_height(&self) -> f32 {
    self.row_tops.last().copied().unwrap_or(0.0)
  }

  pub fn row_top(&self, row_ix: usize) -> f32 {
    self.row_tops.get(row_ix).copied().unwrap_or(0.0)
  }

  pub fn row_height(&self, row_ix: usize) -> f32 {
    match (self.row_tops.get(row_ix), self.row_tops.get(row_ix + 1)) {
      (Some(top), Some(next)) => (next - top - ROW_GAP).max(0.0),
      _ => 0.0,
    }
  }

  pub fn slot_origin(&self, row_ix: usize, column_ix: usize) -> (f32, f32) {
    (self.column_lefts.get(column_ix).copied().unwrap_or(0.0), self.row_top(row_ix))
  }

  /// Column under a model-space x (gaps resolve to the nearer column).
  pub fn column_at(&self, x: f32) -> Option<usize> {
    if self.column_lefts.is_empty() {
      return None;
    }
    let index = match self
      .column_lefts
      .binary_search_by(|left| left.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal))
    {
      Ok(index) => index,
      Err(0) => 0,
      Err(insertion) => insertion - 1,
    };
    Some(index.min(self.column_lefts.len() - 1))
  }

  /// Row (including ghosts) under a model-space y — O(log rows).
  pub fn row_at(&self, y: f32) -> Option<usize> {
    if self.total_rows() == 0 {
      return None;
    }
    let index = match self.row_tops[..self.total_rows()]
      .binary_search_by(|top| top.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal))
    {
      Ok(index) => index,
      Err(0) => 0,
      Err(insertion) => insertion - 1,
    };
    Some(index.min(self.total_rows() - 1))
  }

  /// The visible row range for a model-space vertical window — O(log rows),
  /// with one row of overscan on each side.
  pub fn visible_rows(&self, top: f32, bottom: f32) -> std::ops::Range<usize> {
    let total = self.total_rows();
    if total == 0 {
      return 0..0;
    }
    let first = self.row_at(top).unwrap_or(0).saturating_sub(1);
    let last = self.row_at(bottom).unwrap_or(total - 1) + 2;
    first..last.min(total)
  }
}

fn measured_cell_height(cell: &Cell, measurements: &HashMap<CellId, CellMeasurement>) -> f32 {
  measurements
    .get(&cell.id)
    .map(|measurement| measurement.model_height)
    .unwrap_or_else(|| estimated_cell_height(cell))
}

/// Estimate for unmeasured cells: line-wrapped summary text at the default
/// column width (refined by real measurement on first paint, with the row's
/// prefix sums rebuilt — scroll anchoring keeps the viewport stable).
fn estimated_cell_height(cell: &Cell) -> f32 {
  let text = cell.summary.summary_text.to_string();
  let lines = text
    .lines()
    .map(|line| line.chars().count().div_ceil(34).max(1))
    .sum::<usize>()
    .max(1);
  16.0 + 22.0 * lines as f32
}

/// Row identity for a (possibly ghost) row index: real rows map to their id.
pub(super) fn row_id_at(sheet: &Sheet, row_ix: usize) -> Option<RowId> {
  sheet.rows.get(row_ix).map(|row| row.id)
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_flow::{ArgumentSide, CellSummary, GridColumn, GridRow, SheetId, SheetTypeId};

  fn column(width: Option<f32>) -> GridColumn {
    GridColumn {
      id: uuid::Uuid::new_v4(),
      label: "1AC".into(),
      side: ArgumentSide::One,
      width,
    }
  }

  fn sheet_with(rows: Vec<GridRow>, columns: Vec<GridColumn>) -> Sheet {
    Sheet {
      id: SheetId::new_v4(),
      name: String::new(),
      sheet_type_id: SheetTypeId::new_v4(),
      columns,
      rows,
      annotations: Vec::new(),
    }
  }

  fn empty_row(columns: usize) -> GridRow {
    GridRow {
      id: uuid::Uuid::new_v4(),
      height_override: None,
      cells: vec![None; columns],
    }
  }

  #[test]
  fn override_beats_autofit_and_ghosts_extend_the_grid() {
    let columns = vec![column(None), column(Some(120.0))];
    let mut tall = empty_row(2);
    tall.height_override = Some(200.0);
    let sheet = sheet_with(vec![empty_row(2), tall], columns);
    let layout = GridLayout::compute(&sheet, &HashMap::new());

    assert_eq!(layout.real_rows, 2);
    assert_eq!(layout.total_rows(), 2 + GHOST_ROWS);
    assert_eq!(layout.row_height(0), MIN_ROW_HEIGHT);
    assert_eq!(layout.row_height(1), 200.0);
    assert_eq!(layout.column_widths, vec![DEFAULT_COLUMN_WIDTH, 120.0]);
    assert_eq!(layout.column_lefts[1], DEFAULT_COLUMN_WIDTH + COLUMN_GAP);
  }

  #[test]
  fn autofit_tracks_the_tallest_occupied_cell() {
    let columns = vec![column(None), column(None)];
    let mut row = empty_row(2);
    let cell = Cell {
      id: uuid::Uuid::new_v4(),
      row_id: row.id,
      column_id: columns[1].id,
      summary: CellSummary::default(),
    };
    let cell_id = cell.id;
    row.cells[1] = Some(cell);
    let sheet = sheet_with(vec![row], columns);
    let measurements = HashMap::from([(cell_id, CellMeasurement::new(130.0, 1.0))]);
    let layout = GridLayout::compute(&sheet, &measurements);
    assert_eq!(layout.row_height(0), 130.0);
  }

  #[test]
  fn point_to_slot_round_trips_across_the_grid() {
    let columns = vec![column(None), column(None), column(None)];
    let sheet = sheet_with(vec![empty_row(3), empty_row(3), empty_row(3)], columns);
    let layout = GridLayout::compute(&sheet, &HashMap::new());

    for row_ix in 0..layout.total_rows() {
      for column_ix in 0..3 {
        let (x, y) = layout.slot_origin(row_ix, column_ix);
        assert_eq!(layout.column_at(x + 1.0), Some(column_ix));
        assert_eq!(layout.row_at(y + 1.0), Some(row_ix));
      }
    }
    // Past the right edge clamps to the last column; past the bottom clamps
    // to the last ghost row.
    assert_eq!(layout.column_at(1e6), Some(2));
    assert_eq!(layout.row_at(1e6), Some(layout.total_rows() - 1));
  }

  #[test]
  fn visible_rows_is_a_tight_window_with_overscan() {
    let columns = vec![column(None)];
    let rows: Vec<GridRow> = (0..100).map(|_| empty_row(1)).collect();
    let sheet = sheet_with(rows, columns);
    let layout = GridLayout::compute(&sheet, &HashMap::new());
    let stride = MIN_ROW_HEIGHT + ROW_GAP;
    let window = layout.visible_rows(stride * 40.0, stride * 50.0);
    assert!(window.start >= 38 && window.start <= 40, "start {window:?}");
    assert!(window.end >= 51 && window.end <= 53, "end {window:?}");
    assert!(window.len() < 20, "the window must not include off-screen rows");
  }

  #[test]
  fn zoom_measurements_do_not_redefine_intrinsic_cell_height() {
    let mut measurement = CellMeasurement::new(80.0, 1.0);
    assert!(!measurement.update(24.0, 0.25));
    assert_eq!(measurement.model_height, 80.0);
    assert!(!measurement.update(322.0, 4.0));
    assert_eq!(measurement.model_height, 80.0);
    assert!(measurement.update(120.0, 4.0));
    assert_eq!(measurement.model_height, 30.0);
  }
}
