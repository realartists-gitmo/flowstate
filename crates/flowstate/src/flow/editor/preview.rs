//! A static, non-interactive preview of a flow board — the faithful "starting
//! viewport" a recent-file card shows, the flow analogue of the `.db8` card's
//! `RichTextDocumentElement`. It reuses the REAL cell machinery — `flow_cell_fill`
//! for the side wash, per-cell rich-text `DocumentProjection`s, the side-palette
//! header, and the row-number gutter — so it renders like the top-left of the
//! open flow at 100% zoom.
//!
//! PERF: this runs on EVERY start-screen frame, so it must be cheap.
//! - Cell projections are themed ONCE at load (`theme_flow_preview`), never per
//!   frame — a whole-`DocumentTheme` clone per cell per frame was the stall.
//! - Rows use FIXED heights, so cells never feed measured auto-height back into
//!   the render (which drove continuous re-layout).
//! - Only the cells that fit the card viewport are laid out (rows and columns
//!   are capped), not the whole 40-row sheet.

use std::collections::HashMap;

use flowstate_document::DocumentProjection;
use flowstate_flow::{CellId, FlowBoardProjection};
use gpui::prelude::*;
use gpui::{AnyElement, FontWeight, SharedString, div, px};

use crate::app_settings::load_document_theme;
use crate::flow::cell_theme::{apply_flow_cell_theme, flow_cell_fill};
use crate::flow::resolve_flow_theme;
use crate::rich_text_element::RichTextDocumentElement;

use super::grid_layout::{self, CellMeasurement, GridLayout};

/// Board data a card preview needs, materialized off-thread from the loaded
/// `FlowDocument`: the board projection plus each occupied cell's rich-text
/// projection — THEMED once by `theme_flow_preview` so the render path never
/// re-themes.
pub(crate) struct FlowPreview {
  pub board: FlowBoardProjection,
  pub cell_documents: HashMap<CellId, DocumentProjection>,
}

/// Card viewport zoom. 1.0 = the flow's real default zoom (keyed in on the
/// top-left, and unscaled so cell text stays crisp). The card crops the rest.
const PREVIEW_ZOOM: f32 = 1.0;
/// Rows / columns laid out — sized to cover a card viewport at 100%, not the
/// whole sheet. The card's `overflow_hidden` crops; anything past this is
/// off-card, so laying it out is pure waste.
const PREVIEW_MAX_ROWS: usize = 14;
const PREVIEW_MAX_COLUMNS: usize = 5;

/// The occupied cell ids the preview will actually render — the top-left
/// viewport only. The loader materializes exactly these (not every cell in the
/// flow), so preview cost is bounded by the card, not the document size.
pub(crate) fn preview_cell_ids(board: &FlowBoardProjection) -> Vec<CellId> {
  let Some(sheet) = board.sheets.first() else {
    return Vec::new();
  };
  let mut ids = Vec::new();
  for row in sheet.rows.iter().take(PREVIEW_MAX_ROWS) {
    for slot in row.cells.iter().take(PREVIEW_MAX_COLUMNS) {
      if let Some(cell) = slot.as_ref().filter(|cell| !cell.summary.is_empty) {
        ids.push(cell.id);
      }
    }
  }
  ids
}

/// Theme every occupied cell's projection ONCE (neutral flow text + zoom), on
/// load, so the per-frame render does zero theme work. The flow palette is
/// app-theme-independent, so this needs no `cx`. A later palette change re-runs
/// the preview refresh.
pub(crate) fn theme_flow_preview(preview: &mut FlowPreview) {
  let client_theme = load_document_theme();
  let flow_theme = resolve_flow_theme();
  // C2: cell body text is the flow palette's neutral `text`, over its surface.
  for sheet in &preview.board.sheets {
    for cell in sheet.cells() {
      if let Some(document) = preview.cell_documents.get_mut(&cell.id) {
        apply_flow_cell_theme(document, &client_theme, flow_theme.text, flow_theme.surface, PREVIEW_ZOOM);
      }
    }
  }
}

/// Render the first sheet of `preview` as a static top-left board viewport.
pub(crate) fn render_flow_board_preview(preview: &FlowPreview) -> AnyElement {
  let flow_theme = resolve_flow_theme();
  let Some(sheet) = preview.board.sheets.first() else {
    return div()
      .size_full()
      .flex()
      .items_center()
      .justify_center()
      .text_sm()
      .text_color(flow_theme.muted_text)
      .child("Empty flow")
      .into_any_element();
  };

  let zoom = PREVIEW_ZOOM;
  let measurements: HashMap<CellId, CellMeasurement> = HashMap::new();
  let layout = GridLayout::compute(sheet, &measurements);
  let grid_line = flow_theme.gridline;
  let gutter_line = flow_theme.chrome_border;
  let chrome_bg = flow_theme.gutter_bg;
  let gutter_width = px(grid_layout::GUTTER_WIDTH * zoom);
  let header_height = px(grid_layout::HEADER_HEIGHT * zoom);
  let content_padding = px(grid_layout::CELL_CONTENT_PADDING * zoom);
  let column_count = sheet.columns.len().min(PREVIEW_MAX_COLUMNS);
  let column_width = |column_ix: usize| px(layout.column_widths[column_ix] * zoom);

  // Header: a gutter corner + one side-colored label per (capped) column.
  let header = div()
    .flex()
    .flex_none()
    .child(
      div()
        .flex_none()
        .w(gutter_width)
        .h(header_height)
        .border_b(px(1.0))
        .border_r(px(1.0))
        .border_color(gutter_line)
        .bg(chrome_bg),
    )
    .children(sheet.columns.iter().take(column_count).enumerate().map(|(column_ix, column)| {
      let side = flow_theme.side(column.side);
      div()
        .flex_none()
        .w(column_width(column_ix))
        .h(header_height)
        .flex()
        .items_center()
        .px(px(6.0 * zoom))
        .overflow_hidden()
        .whitespace_nowrap()
        .bg(side.base.opacity(0.14))
        .border_b(px(2.0 * zoom))
        .border_color(side.base)
        .font_weight(FontWeight::BOLD)
        .text_size(px(13.0 * zoom))
        .text_color(side.base)
        .child(SharedString::from(column.label.clone()))
    }));

  // Body: real rows then ghost rows (empty grid) so the viewport fills like the
  // open flow. FIXED heights (from the layout's estimate) — no auto-height, so
  // cells never feed measured layout back into the render.
  let row_count = layout.total_rows().min(PREVIEW_MAX_ROWS);
  let body = div().flex().flex_col().children((0..row_count).map(|row_ix| {
    let is_ghost = row_ix >= layout.real_rows;
    let row_height = px(layout.row_height(row_ix) * zoom);
    div()
      .flex()
      .flex_none()
      .h(row_height)
      .child(
        div()
          .flex_none()
          .w(gutter_width)
          .h(row_height)
          .flex()
          .items_center()
          .justify_center()
          .border_b(px(1.0))
          .border_r(px(1.0))
          .border_color(gutter_line)
          .bg(chrome_bg)
          .text_size(px(10.5 * zoom))
          .text_color(flow_theme.muted_text.opacity(if is_ghost { 0.45 } else { 0.9 }))
          .child(SharedString::from(format!("{}", row_ix + 1))),
      )
      .children((0..column_count).map(|column_ix| {
        let side = flow_theme.side(sheet.columns[column_ix].side);
        let mut slot = div()
          .flex_none()
          .w(column_width(column_ix))
          .h(row_height)
          .overflow_hidden()
          .border_r(px(1.0))
          .border_b(px(1.0))
          .border_color(grid_line)
          .bg(flow_cell_fill(&flow_theme, side.base, 0.0));
        if let Some(cell) = sheet.slot(row_ix, column_ix).filter(|cell| !cell.summary.is_empty) {
          // Projections are pre-themed; the render just wraps them (one clone,
          // no per-frame theme work).
          if let Some(document) = preview.cell_documents.get(&cell.id).cloned() {
            slot = slot.child(
              div().w_full().p(content_padding).child(
                RichTextDocumentElement::new(document).with_invisibility_mode(cell.summary.uses_summary_projection),
              ),
            );
          }
        }
        slot
      }))
  }));

  div()
    .flex()
    .flex_col()
    .bg(flow_theme.surface)
    .child(header)
    .child(body)
    .into_any_element()
}
