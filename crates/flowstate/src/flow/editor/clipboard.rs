//! P4: the flow grid clipboard — Excel-style Copy / Cut / Paste over a cell or
//! a rectangular range. The wire format is TSV (tab between columns, newline
//! between rows), so it round-trips with Excel and Sheets. Cell content is
//! carried as PLAIN TEXT (the cell summary); rich styling is not preserved on
//! paste — a cell paste re-seeds a fresh cell with the text.

use flowstate_flow::{CellId, CellSeed, FlowIntent};
use gpui::{ClipboardItem, Context};

use super::FlowEditor;

/// Split a string into `(prefix, integer, suffix)` around its first digit run,
/// e.g. `"1AC"` → `("", 1, "AC")`, `"R7"` → `("R", 7, "")`.
fn split_number(text: &str) -> Option<(String, i64, String)> {
  let start = text.find(|c: char| c.is_ascii_digit())?;
  let end = text[start..].find(|c: char| !c.is_ascii_digit()).map_or(text.len(), |offset| start + offset);
  let number = text[start..end].parse::<i64>().ok()?;
  Some((text[..start].to_string(), number, text[end..].to_string()))
}

/// Extend `sources` by `beyond` more values: continue an arithmetic series when
/// the sources share a prefix/suffix and step by a constant (`1, 2, 3…` or
/// `1AC, 2AC…`), otherwise tile the block (Excel drag-fill).
fn series_or_tile(sources: &[String], beyond: usize) -> Vec<String> {
  if let Some(parsed) = sources.iter().map(|s| split_number(s)).collect::<Option<Vec<_>>>()
    && parsed.len() >= 2
    && parsed.windows(2).all(|w| w[0].0 == w[1].0 && w[0].2 == w[1].2)
  {
    let step = parsed[1].1 - parsed[0].1;
    if parsed.windows(2).all(|w| w[1].1 - w[0].1 == step) {
      let (prefix, last, suffix) = parsed.last().cloned().unwrap();
      return (1..=beyond).map(|k| format!("{prefix}{}{suffix}", last + step * k as i64)).collect();
    }
  }
  let height = sources.len().max(1);
  (0..beyond).map(|k| sources.get(k % height).cloned().unwrap_or_default()).collect()
}

impl FlowEditor {
  /// Ctrl+C: copy the selection (or the cursor's cell) to the clipboard as TSV.
  pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
    self.cut_pending = None;
    if let Some(tsv) = self.selection_tsv() {
      cx.write_to_clipboard(ClipboardItem::new_string(tsv));
    }
  }

  /// Ctrl+X: copy, and remember the source cells so the next paste moves them
  /// (Excel cut).
  pub fn cut_selection(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    let Some(tsv) = self.selection_tsv() else { return };
    cx.write_to_clipboard(ClipboardItem::new_string(tsv));
    self.cut_pending = Some(self.operation_set(sheet_id));
  }

  /// The selection as a TSV grid, tiled over its bounding rectangle (gaps are
  /// empty cells). `None` when nothing is selected/active.
  fn selection_tsv(&self) -> Option<String> {
    let sheet_id = self.active_sheet?;
    let sheet = self.active_sheet_ref()?;
    let cells: Vec<((usize, usize), String)> = self
      .operation_set(sheet_id)
      .into_iter()
      .filter_map(|id| {
        let position = sheet.cell_position(id)?;
        let text = sheet.find_cell(id)?.summary.summary_text.to_string();
        Some((position, text))
      })
      .collect();
    if cells.is_empty() {
      return None;
    }
    let min_row = cells.iter().map(|((row, _), _)| *row).min()?;
    let min_col = cells.iter().map(|((_, col), _)| *col).min()?;
    let max_row = cells.iter().map(|((row, _), _)| *row).max()?;
    let max_col = cells.iter().map(|((_, col), _)| *col).max()?;
    let mut grid = vec![vec![String::new(); max_col - min_col + 1]; max_row - min_row + 1];
    for ((row, col), text) in cells {
      // TSV can't carry embedded tabs/newlines; flatten them to spaces so the
      // grid shape survives the round-trip.
      grid[row - min_row][col - min_col] = text.replace(['\t', '\n'], " ");
    }
    Some(
      grid
        .into_iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n"),
    )
  }

  /// Ctrl+V: paste the clipboard TSV with its top-left anchored at the cursor.
  /// Occupied targets are overwritten; blanks in the grid are skipped. A cut's
  /// source cells are removed first (the move). One undo group.
  pub fn paste(&mut self, cx: &mut Context<Self>) {
    let Some(item) = cx.read_from_clipboard() else { return };
    let Some(text) = item.text() else { return };
    let grid: Vec<Vec<String>> = text
      .replace("\r\n", "\n")
      .trim_end_matches('\n')
      .split('\n')
      .map(|line| line.split('\t').map(|cell| cell.to_string()).collect())
      .collect();
    if grid.is_empty() {
      return;
    }
    let Some(sheet_id) = self.active_sheet else { return };
    let (anchor_row, anchor_col) = self.cursor.unwrap_or((0, 0));
    let max_row = anchor_row + grid.len().saturating_sub(1);
    let cut = self.cut_pending.take();

    let _ = self.handle.undo_group_start();
    let existing = self.active_sheet_ref().map(|sheet| sheet.rows.len()).unwrap_or(0);
    let needed = (max_row + 1).saturating_sub(existing);
    if needed > 0 {
      self.materialize_rows(sheet_id, needed, cx);
    }
    // Clear the cut's origin cells before placing (a move that overlaps its own
    // source re-creates the overlap fresh).
    if let Some(sources) = &cut {
      for &cell_id in sources {
        let _ = self.apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx);
      }
    }
    let columns = self.active_sheet_ref().map(|sheet| sheet.columns.len()).unwrap_or(0);
    for (delta_row, line) in grid.iter().enumerate() {
      for (delta_col, cell_text) in line.iter().enumerate() {
        let column_ix = anchor_col + delta_col;
        if column_ix >= columns || cell_text.is_empty() {
          continue; // clip past the last column; blanks don't overwrite
        }
        self.place_cell_text(sheet_id, anchor_row + delta_row, column_ix, cell_text, cx);
      }
    }
    let _ = self.handle.undo_group_end();
    self.changed(None, cx);
  }

  /// Ctrl+D: fill down. A range fills its top row into the rows below; a single
  /// cell copies the cell directly above it (Excel).
  pub fn fill_down(&mut self, cx: &mut Context<Self>) {
    let Some((sheet_id, r0, c0, r1, c1)) = self.fill_bounds() else { return };
    let plan: Vec<(usize, usize, String)> = if r0 == r1 {
      if r0 == 0 {
        return; // nothing above to pull from
      }
      (c0..=c1)
        .filter_map(|c| self.cell_text_at(r0 - 1, c).map(|text| (r0, c, text)))
        .collect()
    } else {
      (c0..=c1)
        .filter_map(|c| self.cell_text_at(r0, c).map(|text| (c, text)))
        .flat_map(|(c, text)| (r0 + 1..=r1).map(move |r| (r, c, text.clone())))
        .collect()
    };
    self.apply_fill(sheet_id, plan, cx);
  }

  /// Ctrl+R: fill right. A range fills its left column across; a single cell
  /// copies the cell directly to its left.
  pub fn fill_right(&mut self, cx: &mut Context<Self>) {
    let Some((sheet_id, r0, c0, r1, c1)) = self.fill_bounds() else { return };
    let plan: Vec<(usize, usize, String)> = if c0 == c1 {
      if c0 == 0 {
        return;
      }
      (r0..=r1)
        .filter_map(|r| self.cell_text_at(r, c0 - 1).map(|text| (r, c0, text)))
        .collect()
    } else {
      (r0..=r1)
        .filter_map(|r| self.cell_text_at(r, c0).map(|text| (r, text)))
        .flat_map(|(r, text)| (c0 + 1..=c1).map(move |c| (r, c, text.clone())))
        .collect()
    };
    self.apply_fill(sheet_id, plan, cx);
  }

  /// Fill-handle drop: extend the selection to the swept corner and tile the
  /// selection's content into the new cells (Excel drag-fill). Constrained to
  /// one axis — whichever grew more.
  pub fn fill_handle_drop(&mut self, target_row: usize, target_col: usize, cx: &mut Context<Self>) {
    let Some((sheet_id, r0, c0, r1, c1)) = self.fill_bounds() else { return };
    let down = target_row > r1;
    let right = target_col > c1;
    let mut plan: Vec<(usize, usize, String)> = Vec::new();
    if down && (target_row - r1) >= target_col.saturating_sub(c1) {
      for column in c0..=c1 {
        let sources: Vec<String> = (r0..=r1).map(|row| self.cell_text_at(row, column).unwrap_or_default()).collect();
        let values = series_or_tile(&sources, target_row - r1);
        for (offset, row) in ((r1 + 1)..=target_row).enumerate() {
          plan.push((row, column, values[offset].clone()));
        }
      }
    } else if right {
      for row in r0..=r1 {
        let sources: Vec<String> = (c0..=c1).map(|column| self.cell_text_at(row, column).unwrap_or_default()).collect();
        let values = series_or_tile(&sources, target_col - c1);
        for (offset, column) in ((c1 + 1)..=target_col).enumerate() {
          plan.push((row, column, values[offset].clone()));
        }
      }
    } else {
      return;
    }
    let plan: Vec<_> = plan.into_iter().filter(|(_, _, text)| !text.is_empty()).collect();
    let Some(max_row) = plan.iter().map(|(row, _, _)| *row).max() else { return };
    let _ = self.handle.undo_group_start();
    let existing = self.active_sheet_ref().map(|sheet| sheet.rows.len()).unwrap_or(0);
    let needed = (max_row + 1).saturating_sub(existing);
    if needed > 0 {
      self.materialize_rows(sheet_id, needed, cx);
    }
    for (row, column, text) in plan {
      self.place_cell_text(sheet_id, row, column, &text, cx);
    }
    let _ = self.handle.undo_group_end();
    self.changed(None, cx);
  }

  /// Place each `(row, col, text)` as one undo group (blank sources skipped).
  fn apply_fill(&mut self, sheet_id: flowstate_flow::SheetId, plan: Vec<(usize, usize, String)>, cx: &mut Context<Self>) {
    let plan: Vec<_> = plan.into_iter().filter(|(_, _, text)| !text.is_empty()).collect();
    if plan.is_empty() {
      return;
    }
    let _ = self.handle.undo_group_start();
    for (row, col, text) in plan {
      self.place_cell_text(sheet_id, row, col, &text, cx);
    }
    let _ = self.handle.undo_group_end();
    self.changed(None, cx);
  }

  /// The selection's bounding rectangle (or the cursor cell), as
  /// `(sheet_id, r0, c0, r1, c1)`.
  fn fill_bounds(&self) -> Option<(flowstate_flow::SheetId, usize, usize, usize, usize)> {
    let sheet_id = self.active_sheet?;
    let sheet = self.active_sheet_ref()?;
    let positions: Vec<(usize, usize)> = if self.selected_cells.is_empty() {
      self.cursor.into_iter().collect()
    } else {
      self.selected_cells.iter().filter_map(|id| sheet.cell_position(*id)).collect()
    };
    let positions = if positions.is_empty() { self.cursor.into_iter().collect::<Vec<_>>() } else { positions };
    let r0 = positions.iter().map(|(r, _)| *r).min()?;
    let c0 = positions.iter().map(|(_, c)| *c).min()?;
    let r1 = positions.iter().map(|(r, _)| *r).max()?;
    let c1 = positions.iter().map(|(_, c)| *c).max()?;
    Some((sheet_id, r0, c0, r1, c1))
  }

  /// The plain-text content of the cell at a slot, if occupied.
  fn cell_text_at(&self, row_ix: usize, column_ix: usize) -> Option<String> {
    let sheet = self.active_sheet_ref()?;
    sheet.slot(row_ix, column_ix).map(|cell| cell.summary.summary_text.to_string())
  }

  /// Overwrite (or create) the cell at a slot with plain text. The row must
  /// already exist (paste materializes rows up front).
  fn place_cell_text(&mut self, sheet_id: flowstate_flow::SheetId, row_ix: usize, column_ix: usize, text: &str, cx: &mut Context<Self>) {
    let (column_id, row_id, occupant) = {
      let Some(sheet) = self.active_sheet_ref() else { return };
      let Some(column_id) = sheet.columns.get(column_ix).map(|column| column.id) else { return };
      let Some(row_id) = sheet.rows.get(row_ix).map(|row| row.id) else { return };
      (column_id, row_id, sheet.slot(row_ix, column_ix).map(|cell| cell.id))
    };
    if let Some(cell_id) = occupant {
      let _ = self.apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx);
    }
    let seed = CellSeed::Paragraphs(vec![flowstate_document::InputParagraph {
      style: flowstate_document::PARAGRAPH_TAG,
      runs: vec![flowstate_document::InputRun {
        text: text.to_string(),
        styles: flowstate_document::RunStyles::default(),
      }],
    }]);
    let cell_id: CellId = uuid::Uuid::new_v4();
    let _ = self.apply_intent(
      &FlowIntent::AddCell {
        sheet_id,
        cell_id,
        row_id,
        column_id,
        seed,
      },
      cx,
    );
  }
}

#[cfg(test)]
mod tests {
  use super::{series_or_tile, split_number};

  #[test]
  fn split_number_finds_the_first_digit_run() {
    assert_eq!(split_number("1AC"), Some((String::new(), 1, "AC".into())));
    assert_eq!(split_number("R7"), Some(("R".into(), 7, String::new())));
    assert_eq!(split_number("42"), Some((String::new(), 42, String::new())));
    assert_eq!(split_number("hello"), None);
  }

  #[test]
  fn arithmetic_series_continues() {
    assert_eq!(series_or_tile(&["1".into(), "2".into()], 3), vec!["3", "4", "5"]);
    assert_eq!(series_or_tile(&["1AC".into(), "2AC".into()], 2), vec!["3AC", "4AC"]);
    // Constant step of 2.
    assert_eq!(series_or_tile(&["0".into(), "2".into(), "4".into()], 2), vec!["6", "8"]);
  }

  #[test]
  fn non_series_tiles_the_block() {
    // A single cell copies (no series without ≥2 samples).
    assert_eq!(series_or_tile(&["x".into()], 3), vec!["x", "x", "x"]);
    // Mismatched suffix falls back to tiling.
    assert_eq!(series_or_tile(&["1A".into(), "2B".into()], 2), vec!["1A", "2B"]);
    // Non-arithmetic step tiles.
    assert_eq!(series_or_tile(&["1".into(), "4".into(), "5".into()], 2), vec!["1", "4"]);
  }
}
