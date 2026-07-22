//! Soak object #1: the app-side FlowEditor interaction net. Drives the real
//! public grid grammar built in P1–P6 — navigation, selection (range / row /
//! column / select-all / cycle), clipboard copy/cut/paste, and fill/series —
//! over a headless FlowEditor (no rendering), asserting on `board`/`cursor`/
//! `selected_cells`. This is the behavioral net the interaction layer lacked.

use gpui::{Entity, TestAppContext};

use super::support::{self, WorkspaceHarness};
use crate::flow::FlowEditor;
use crate::flow::editor::GridDirection;

/// Open a workspace with a fresh flow whose sheet has real columns.
fn open_flow(cx: &mut TestAppContext) -> (WorkspaceHarness, Entity<FlowEditor>) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  flow.update(cx, |editor, cx| editor.create_sheet(cx));
  cx.run_until_parked();
  (h, flow)
}

/// Seed a cell at (row, col) with `text` (creates + overwrites through the real
/// type path).
fn seed(flow: &Entity<FlowEditor>, cx: &mut TestAppContext, row: usize, col: usize, text: &str) {
  flow.update(cx, |editor, cx| {
    editor.set_cursor(row, col, cx);
    editor.overwrite_cursor(text, cx);
  });
  cx.run_until_parked();
}

/// The plain-text summary of the cell at (row, col), if occupied.
fn cell_text(flow: &Entity<FlowEditor>, cx: &mut TestAppContext, row: usize, col: usize) -> Option<String> {
  flow.read_with(cx, |editor, _cx| {
    editor.board().sheets[0].slot(row, col).map(|cell| cell.summary.summary_text.to_string())
  })
}

fn cursor(flow: &Entity<FlowEditor>, cx: &mut TestAppContext) -> Option<(usize, usize)> {
  flow.read_with(cx, |editor, _cx| editor.cursor())
}

fn selection_len(flow: &Entity<FlowEditor>, cx: &mut TestAppContext) -> usize {
  flow.read_with(cx, |editor, _cx| editor.selected_cells().len())
}

#[gpui::test]
fn navigation_clamps_at_the_grid_edges(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.navigate(GridDirection::Up, cx); // already at the top row
    editor.navigate(GridDirection::Left, cx); // already at the first column
  });
  cx.run_until_parked();
  assert_eq!(cursor(&flow, cx), Some((0, 0)), "arrows clamp, never wrap, at the top-left");

  // Right then down move one slot each.
  flow.update(cx, |editor, cx| {
    editor.navigate(GridDirection::Right, cx);
    editor.navigate(GridDirection::Down, cx);
  });
  cx.run_until_parked();
  assert_eq!(cursor(&flow, cx), Some((1, 1)), "right + down step one slot each");
}

#[gpui::test]
fn range_row_column_and_select_all(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  // A 2x2 block of cells.
  seed(&flow, cx, 0, 0, "a");
  seed(&flow, cx, 0, 1, "b");
  seed(&flow, cx, 1, 0, "c");
  seed(&flow, cx, 1, 1, "d");

  // Shift-extend a rectangle from (0,0) to (1,1): all four.
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(1, 1, cx);
  });
  assert_eq!(selection_len(&flow, cx), 4, "the 2x2 range selects all four cells");

  // Row selection = one row's cells.
  flow.update(cx, |editor, cx| editor.select_row(0, cx));
  assert_eq!(selection_len(&flow, cx), 2, "row 0 has two cells");

  // Column selection = one column's cells.
  flow.update(cx, |editor, cx| editor.select_column(0, cx));
  assert_eq!(selection_len(&flow, cx), 2, "column 0 has two cells");

  // Select-all = the whole sheet.
  flow.update(cx, |editor, cx| editor.select_all(cx));
  assert_eq!(selection_len(&flow, cx), 4, "select-all takes every cell");
}

#[gpui::test]
fn clipboard_copies_and_pastes_a_range(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  // Two cells stacked in column 0.
  seed(&flow, cx, 0, 0, "alpha");
  seed(&flow, cx, 1, 0, "beta");

  // Select both and copy.
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(1, 0, cx);
    editor.copy_selection(cx);
  });
  cx.run_until_parked();

  // Anchor at column 1 and paste — the block replicates one column right.
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 1, cx);
    editor.paste(cx);
  });
  cx.run_until_parked();

  assert_eq!(cell_text(&flow, cx, 0, 1).as_deref(), Some("alpha"), "paste anchored the top cell at the cursor");
  assert_eq!(cell_text(&flow, cx, 1, 1).as_deref(), Some("beta"), "the block tiled down from the anchor");
  // The source is untouched (copy, not cut).
  assert_eq!(cell_text(&flow, cx, 0, 0).as_deref(), Some("alpha"), "copy leaves the source in place");
}

#[gpui::test]
fn cut_then_paste_moves_the_cells(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  seed(&flow, cx, 0, 0, "one");

  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(0, 0, cx);
    editor.cut_selection(cx);
  });
  cx.run_until_parked();
  flow.update(cx, |editor, cx| {
    editor.set_cursor(2, 1, cx);
    editor.paste(cx);
  });
  cx.run_until_parked();

  assert_eq!(cell_text(&flow, cx, 2, 1).as_deref(), Some("one"), "cut cell lands at the paste anchor");
  assert_eq!(cell_text(&flow, cx, 0, 0), None, "cut removes the source on paste (a move)");
}

#[gpui::test]
fn fill_down_replicates_the_top_of_a_range(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  // A column of three occupied cells; fill-down replaces the lower two with the
  // top cell's content.
  seed(&flow, cx, 0, 0, "x");
  seed(&flow, cx, 1, 0, "old-1");
  seed(&flow, cx, 2, 0, "old-2");
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(2, 0, cx); // the whole occupied column
    editor.fill_down(cx);
  });
  cx.run_until_parked();
  assert_eq!(cell_text(&flow, cx, 1, 0).as_deref(), Some("x"), "fill-down copies the top cell down");
  assert_eq!(cell_text(&flow, cx, 2, 0).as_deref(), Some("x"), "…through the whole range");
}

#[gpui::test]
fn fill_handle_continues_an_arithmetic_series(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  seed(&flow, cx, 0, 0, "1");
  seed(&flow, cx, 1, 0, "2");
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(1, 0, cx); // the 1,2 block
    editor.fill_handle_drop(3, 0, cx); // drag the handle down two rows
  });
  cx.run_until_parked();
  assert_eq!(cell_text(&flow, cx, 2, 0).as_deref(), Some("3"), "the arithmetic series continues");
  assert_eq!(cell_text(&flow, cx, 3, 0).as_deref(), Some("4"), "…by the detected step");
}

#[gpui::test]
fn plain_navigation_collapses_a_stale_selection(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  seed(&flow, cx, 0, 0, "a");
  seed(&flow, cx, 0, 1, "b");
  seed(&flow, cx, 1, 0, "c");
  seed(&flow, cx, 1, 1, "d");

  // A 2x2 range is selected…
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(1, 1, cx);
  });
  assert_eq!(selection_len(&flow, cx), 4, "the range selects all four");

  // …then a plain arrow move COLLAPSES it, so a following Delete/Cut/Copy (which
  // read the selection) act on the cursor's cell alone, not the stale rectangle.
  // `select_cell_range` leaves the cursor at the range's moving corner (1,1);
  // an arrow Down from there lands on (2,1).
  flow.update(cx, |editor, cx| editor.navigate(GridDirection::Down, cx));
  assert_eq!(selection_len(&flow, cx), 0, "an arrow move clears the multi-cell selection");
  assert_eq!(cursor(&flow, cx), Some((2, 1)), "the cursor moved down one row from the range corner");
}

#[gpui::test]
fn tab_wraps_from_the_last_column_to_the_next_row(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  let columns = flow.read_with(cx, |editor, _cx| editor.board().sheets[0].columns.len());
  assert!(columns >= 2, "the sheet has real columns to tab across");

  // Tab across the whole row from (0,0): each Tab advances one column, and the
  // Tab past the last column wraps to the next row's run-start column (Excel),
  // never a silent dead-end.
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    for _ in 0..columns {
      editor.tab_navigate(false, cx);
    }
  });
  cx.run_until_parked();
  assert_eq!(cursor(&flow, cx), Some((1, 0)), "Tab past the last column wraps down to the anchor column");
}

#[gpui::test]
fn type_overwrite_replaces_an_occupied_cell(cx: &mut TestAppContext) {
  let (_h, flow) = open_flow(cx);
  seed(&flow, cx, 0, 0, "original");
  assert_eq!(cell_text(&flow, cx, 0, 0).as_deref(), Some("original"));

  // Typing on the occupied slot replaces its content (Excel overwrite).
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.overwrite_cursor("z", cx);
  });
  cx.run_until_parked();
  assert_eq!(cell_text(&flow, cx, 0, 0).as_deref(), Some("z"), "type-to-overwrite replaces the cell's content");
}
