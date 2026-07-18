//! A static, non-interactive preview of a flow board — the faithful "starting
//! viewport" a recent-file card shows, the flow analogue of the `.db8` card's
//! `RichTextDocumentElement`. It reuses the REAL cell machinery — `flow_cell_fill`
//! for the side wash, `apply_flow_cell_theme` + `RichTextDocumentElement` for the
//! actual rich cell text, the side-palette header, and `GridLayout` geometry —
//! so it renders like the top-left of the open flow (grid extending into empty
//! ghost rows below the content), not a bespoke reconstruction. No runtime, no
//! I/O thread: it draws from an already-loaded `FlowDocument`'s projections.

use std::collections::HashMap;

use flowstate_document::DocumentProjection;
use flowstate_flow::{CellId, FlowBoardProjection};
use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, div, px};
use gpui_component::ActiveTheme as _;

use crate::app_settings::load_document_theme;
use crate::flow::cell_theme::{apply_flow_cell_theme, flow_cell_fill};
use crate::flow::flow_side_palette;
use crate::rich_text_element::RichTextDocumentElement;

use super::grid_layout::{self, CellMeasurement, GridLayout};

/// Board data a card preview needs, materialized off-thread from the loaded
/// `FlowDocument`: the board projection plus each occupied cell's rich-text
/// projection (the same `DocumentProjection` the live editor renders).
pub(crate) struct FlowPreview {
  pub board: FlowBoardProjection,
  pub cell_documents: HashMap<CellId, DocumentProjection>,
}

/// Card viewport zoom: small enough to show a few speech columns and the grid
/// falling away below the content, large enough to stay legible. Tunable.
const PREVIEW_ZOOM: f32 = 0.62;
/// Cap on rendered rows (real + ghost); the card's `overflow_hidden` crops the
/// rest. Generous so a tall card still fills with grid.
const PREVIEW_MAX_ROWS: usize = 40;

/// Render the first sheet of `preview` as a static top-left board viewport.
pub(crate) fn render_flow_board_preview(preview: &FlowPreview, cx: &App) -> AnyElement {
  let Some(sheet) = preview.board.sheets.first() else {
    return div()
      .size_full()
      .flex()
      .items_center()
      .justify_center()
      .text_sm()
      .text_color(cx.theme().secondary_foreground)
      .child("Empty flow")
      .into_any_element();
  };

  let zoom = PREVIEW_ZOOM;
  // Empty measurements → estimated row heights (the same fallback the live
  // editor uses before its first paint); good enough for a static viewport.
  let measurements: HashMap<CellId, CellMeasurement> = HashMap::new();
  let layout = GridLayout::compute(sheet, &measurements);
  let client_theme = load_document_theme();
  let grid_line = cx.theme().border.opacity(0.85);
  let header_height = px(grid_layout::HEADER_HEIGHT * zoom);
  let content_padding = px(grid_layout::CELL_CONTENT_PADDING * zoom);

  // Header strip: one side-colored label per column, matching the live header.
  let header = div().flex().flex_none().children(sheet.columns.iter().enumerate().map(|(column_ix, column)| {
    let side = flow_side_palette(column.side, cx);
    div()
      .w(px(layout.column_widths[column_ix] * zoom))
      .h(header_height)
      .flex()
      .items_center()
      .px(px(6.0 * zoom))
      .overflow_hidden()
      .whitespace_nowrap()
      .bg(side.base.opacity(0.04))
      .border_b(px(2.0 * zoom))
      .border_color(side.base)
      .font_weight(FontWeight::BOLD)
      .text_size(px(13.0 * zoom))
      .text_color(side.base)
      .child(SharedString::from(column.label.clone()))
  }));

  // Body: real rows then ghost rows (empty grid) so the viewport fills like the
  // open flow. Occupied cells carry the real side wash + rich text.
  let row_count = layout.total_rows().min(PREVIEW_MAX_ROWS);
  let body = div().flex().flex_col().children((0..row_count).map(|row_ix| {
    let row_height = px(layout.row_height(row_ix) * zoom);
    div().flex().flex_none().children(sheet.columns.iter().enumerate().map(|(column_ix, column)| {
      let side = flow_side_palette(column.side, cx);
      let mut slot = div()
        .w(px(layout.column_widths[column_ix] * zoom))
        .h(row_height)
        .overflow_hidden()
        .border_r(px(1.0))
        .border_b(px(1.0))
        .border_color(grid_line);
      if let Some(cell) = sheet.slot(row_ix, column_ix).filter(|cell| !cell.summary.is_empty) {
        slot = slot.bg(flow_cell_fill(
          side.base,
          cx.theme().background,
          cx.theme().foreground,
          cx.theme().is_dark(),
          0.0,
        ));
        if let Some(document) = preview.cell_documents.get(&cell.id).cloned() {
          let mut document = document;
          apply_flow_cell_theme(&mut document, &client_theme, side.base, cx.theme().background, zoom);
          slot = slot.child(
            div().size_full().p(content_padding).child(
              RichTextDocumentElement::new(document).with_invisibility_mode(cell.summary.uses_summary_projection),
            ),
          );
        }
      }
      slot
    }))
  }));

  div()
    .size_full()
    .overflow_hidden()
    .bg(cx.theme().background)
    .child(div().flex().flex_col().child(header).child(body))
    .into_any_element()
}
