use std::ops::Range;

use gpui::{App, Background, Bounds, Hsla, Pixels, Point, ScrollHandle, Window, black, fill, hsla, point, px, rgb, size};

use flowstate_fidelity::{self as fidelity, FidelityClass};
use gpui_component::ActiveTheme as _;

use super::*;

#[hotpath::measure]
pub(super) fn paint_layout(
  layout: &LayoutState,
  bounds: Bounds<Pixels>,
  selection: Option<&EditorSelection>,
  drag_selection: Option<&EditorSelection>,
  show_caret: bool,
  caret_width: Pixels,
  caret_color_rgb: Option<u32>,
  // Caret color to use when `caret_color_rgb` is `None` (solo editing, no
  // collaboration-caret color). MUST be the theme's default text color so the
  // caret contrasts with the background — a hardcoded black caret is invisible
  // on a dark theme (the "invisible caret" bug).
  default_caret_color: Hsla,
  external_carets: &[ExternalCaret],
  external_selections: &[ExternalSelection],
  annotation_selections: &[(ExternalSelection, bool)],
  jump_flash: Option<&ExternalSelection>,
  search_highlights: &[Range<DocumentOffset>],
  active_search_highlight: Option<usize>,
  window: &mut Window,
  cx: &mut App,
) {
  let timing = Instant::now();
  let content_mask = window.content_mask().bounds;
  let visible_range = visible_paragraph_range(layout, bounds.origin, content_mask);
  let visible_count = visible_range.end.saturating_sub(visible_range.start);
  // Fidelity: record the rendered paragraph set for this paint so a layout that
  // trails the document (missing paragraphs, wrong index range) is visible in
  // the firehose. The document/layout generation counters are not threaded into
  // paint, so the caret-vs-laid-out check below is the staleness assertion.
  if fidelity::enabled() {
    let first_index = layout.paragraphs.first().map(|paragraph| paragraph.index);
    let last_index = layout.paragraphs.last().map(|paragraph| paragraph.index);
    fidelity::event(FidelityClass::Structure, "layout-paragraph-set", || {
      format!(
        "laid_out={} index_range={first_index:?}..={last_index:?} visible={}..{} blocks={}",
        layout.paragraphs.len(),
        visible_range.start,
        visible_range.end,
        layout.block_count(),
      )
    });
  }
  for paragraph in &layout.paragraphs[visible_range.clone()] {
    if !paragraph_intersects_mask(paragraph, bounds.origin, content_mask) {
      continue;
    }
    for border in &paragraph.borders {
      let border_bounds = snap_rule_bounds(border.bounds.shift(bounds.origin), border.snap, window);
      window.paint_quad(fill(border_bounds, Background::from(border.color)));
    }
  }
  for paragraph in &layout.paragraphs[visible_range.clone()] {
    if !paragraph_intersects_mask(paragraph, bounds.origin, content_mask) {
      continue;
    }
    for line in &paragraph.lines {
      if !line_intersects_mask(line, bounds.origin, content_mask) {
        continue;
      }
      for rect in &line.rects {
        let rect_bounds = snap_rule_bounds(rect.bounds.shift(bounds.origin + line.origin), rect.snap, window);
        window.paint_quad(fill(rect_bounds, Background::from(rect.color)));
      }
    }
  }
  // Selection is painted before text so the semi-transparent highlight sits
  // behind glyphs rather than covering them.
  paint_search_highlights(
    layout,
    search_highlights,
    active_search_highlight,
    bounds.origin,
    content_mask,
    visible_range.clone(),
    window,
  );
  if let Some(selection) = selection {
    paint_selection(layout, selection, bounds.origin, content_mask, visible_range.clone(), window);
  }
  if let Some(selection) = drag_selection {
    paint_selection(layout, selection, bounds.origin, content_mask, visible_range.clone(), window);
  }
  // Remote peers' selection spans, each in that peer's presence color (the same
  // hue as their caret, softened for a behind-the-glyphs highlight).
  for external in external_selections {
    paint_text_range_fill(
      layout,
      &external.selection,
      bounds.origin,
      content_mask,
      visible_range.clone(),
      remote_selection_color(external.color_rgb),
      window,
    );
  }
  // Durable annotations use their own overlay layer so presence refreshes do
  // not make comment anchors blink or disappear. Review dress (C-S4/M3): a
  // dashed underline, not a fill — the text stays exactly as it will read, the
  // mark sits under it. The hovered span paints thicker and brighter.
  for (annotation, hovered) in annotation_selections {
    paint_text_range_dashed_underline(
      layout,
      &annotation.selection,
      bounds.origin,
      content_mask,
      visible_range.clone(),
      annotation_underline_color(annotation.color_rgb, *hovered),
      if *hovered { px(2.0) } else { px(1.0) },
      window,
    );
  }
  // The transient navigation flash paints above annotations, stronger, so a
  // jump target reads unmistakably even inside an existing annotation span.
  if let Some(flash) = jump_flash {
    paint_text_range_fill(
      layout,
      &flash.selection,
      bounds.origin,
      content_mask,
      visible_range.clone(),
      jump_flash_color(flash.color_rgb),
      window,
    );
  }
  for paragraph in &layout.paragraphs[visible_range.clone()] {
    if !paragraph_intersects_mask(paragraph, bounds.origin, content_mask) {
      continue;
    }
    for line in &paragraph.lines {
      if line_intersects_mask(line, bounds.origin, content_mask) {
        paint_line_text(line, bounds.origin + line.origin, content_mask, window, cx);
      }
    }
  }
  for paragraph in &layout.paragraphs[visible_range.clone()] {
    if !paragraph_intersects_mask(paragraph, bounds.origin, content_mask) {
      continue;
    }
    for line in &paragraph.lines {
      if !line_intersects_mask(line, bounds.origin, content_mask) {
        continue;
      }
      for underline in &line.underlines {
        let mut underline_bounds = underline.bounds.shift(bounds.origin + line.origin);
        if layout.snap_underline_rules_to_pixels {
          underline_bounds = snap_horizontal_rule_to_device_pixels(underline_bounds, window);
        }
        window.paint_quad(fill(underline_bounds, Background::from(underline.color)));
      }
      for strikethrough in &line.strikethroughs {
        let mut strikethrough_bounds = strikethrough.bounds.shift(bounds.origin + line.origin);
        if layout.snap_underline_rules_to_pixels {
          strikethrough_bounds = snap_horizontal_rule_to_device_pixels(strikethrough_bounds, window);
        }
        window.paint_quad(fill(strikethrough_bounds, Background::from(strikethrough.color)));
      }
    }
  }
  // Fidelity: before painting the local caret, verify it resolves against a
  // layout that still matches the model caret. `show_caret` is only set for the
  // chunk that owns the caret (see element.rs `caret_offset_belongs_to_chunk`),
  // so the located paragraph's document index MUST equal the model caret's
  // paragraph; a mismatch (or the model caret pointing past the last laid-out
  // paragraph) means the painted caret is being resolved against a stale layout
  // — render lag where the model caret is correct but the paint trails.
  if fidelity::enabled()
    && let Some(selection) = selection
    && selection.is_caret()
    && show_caret
  {
    let painted_rect = caret_bounds(layout, selection.head, selection.head_gravity, bounds.origin);
    let painted_paragraph = locate_line(layout, selection.head, selection.head_gravity).map(|(p_ix, _)| layout.paragraphs[p_ix].index);
    let max_paragraph = layout.paragraphs.last().map(|paragraph| paragraph.index);
    fidelity::event(FidelityClass::Caret, "paint", || {
      format!(
        "model_caret={:?} gravity={:?} painted_rect={painted_rect:?} painted_paragraph={painted_paragraph:?} laid_out={} max_paragraph={max_paragraph:?}",
        selection.head,
        selection.head_gravity,
        layout.paragraphs.len(),
      )
    });
    fidelity::check(
      painted_paragraph.is_none_or(|index| index == selection.head.paragraph),
      FidelityClass::Caret,
      "caret-render-stale",
      || {
        format!(
          "model_paragraph={} painted_paragraph={painted_paragraph:?} model_byte={} laid_out={}",
          selection.head.paragraph,
          selection.head.byte,
          layout.paragraphs.len(),
        )
      },
    );
    fidelity::check(
      max_paragraph.is_none_or(|max| selection.head.paragraph <= max),
      FidelityClass::Structure,
      "layout-generation-lag",
      || {
        format!(
          "model_paragraph={} max_laid_out_paragraph={max_paragraph:?} laid_out={}",
          selection.head.paragraph,
          layout.paragraphs.len(),
        )
      },
    );
  }
  if let Some(selection) = selection
    && selection.is_caret()
    && show_caret
    && let Some(mut caret) = caret_bounds(layout, selection.head, selection.head_gravity, bounds.origin)
    && caret.intersects(&content_mask)
  {
    caret.size.width = caret_width;
    let caret_color = caret_color_rgb.map_or_else(|| Background::from(default_caret_color), |color_rgb| Background::from(rgb(color_rgb)));
    window.paint_quad(fill(snap_vertical_rule_to_device_pixels(caret, window), caret_color));
  }
  for external_caret in external_carets {
    // Fidelity: remote carets are pre-filtered to this chunk (element.rs), so a
    // located paragraph that differs from the remote offset's paragraph means
    // the remote caret is painted against a stale layout.
    if fidelity::enabled() {
      let painted_paragraph =
        locate_line(layout, external_caret.offset, external_caret.visual_gravity).map(|(p_ix, _)| layout.paragraphs[p_ix].index);
      fidelity::event(FidelityClass::Caret, "paint-external", || {
        format!(
          "offset={:?} gravity={:?} painted_paragraph={painted_paragraph:?} color=#{:06x}",
          external_caret.offset, external_caret.visual_gravity, external_caret.color_rgb,
        )
      });
      fidelity::check(
        painted_paragraph.is_none_or(|index| index == external_caret.offset.paragraph),
        FidelityClass::Caret,
        "caret-render-stale",
        || format!("external offset={:?} painted_paragraph={painted_paragraph:?}", external_caret.offset),
      );
    }
    if let Some(mut caret) = caret_bounds(layout, external_caret.offset, external_caret.visual_gravity, bounds.origin)
      && caret.intersects(&content_mask)
    {
      caret.size.width = caret_width;
      window.paint_quad(fill(
        snap_vertical_rule_to_device_pixels(caret, window),
        Background::from(rgb(external_caret.color_rgb)),
      ));
    }
  }
  log_timing_lazy("paint layout", timing, || {
    format!("blocks={} visible_paragraphs={visible_count}", layout.block_count())
  });
}

#[hotpath::measure]
pub(super) fn paint_structural_block(
  block: &LaidOutBlock,
  selected_block: Option<BlockSelection>,
  table_cell_caret: Option<TableCellCaret>,
  text_selected: bool,
  origin: Point<Pixels>,
  window: &mut Window,
  cx: &mut App,
) {
  let content_mask = window.content_mask().bounds;
  match block {
    LaidOutBlock::Paragraph(paragraph) => paint_table_paragraph(paragraph, origin, content_mask, window, cx),
    LaidOutBlock::Image(object) => paint_object_block(object, "Image", selected_block, origin, content_mask, window, cx),
    LaidOutBlock::Equation(object) => paint_object_block(object, "Equation", selected_block, origin, content_mask, window, cx),
    LaidOutBlock::Table(table) => paint_table_block(table, selected_block, table_cell_caret, text_selected, origin, content_mask, window, cx),
  }
}

#[hotpath::measure]
fn paint_object_block(
  object: &LaidOutObjectBlock,
  _label: &str,
  selected_block: Option<BlockSelection>,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  window: &mut Window,
  cx: &App,
) {
  let bounds = object.bounds.shift(origin);
  if !bounds.intersects(&content_mask) {
    return;
  }
  let selected = matches!(
    selected_block,
    Some(BlockSelection::Image(ix) | BlockSelection::Equation(ix)) if ix == object.block_ix
  );
  // B-S1 theme-is-law: object frames draw from theme slots — the hardcoded
  // white fill was a light-theme assumption baked into every dark theme.
  let frame_rule = if selected { cx.theme().primary } else { cx.theme().border };
  window.paint_quad(fill(bounds, Background::from(cx.theme().background)));
  window.paint_quad(fill(
    snap_rule_bounds(Bounds::new(bounds.origin, size(bounds.size.width, px(1.0))), RuleSnap::Horizontal, window),
    Background::from(frame_rule),
  ));
  window.paint_quad(fill(
    snap_rule_bounds(
      Bounds::new(point(bounds.origin.x, bounds.bottom() - px(1.0)), size(bounds.size.width, px(1.0))),
      RuleSnap::Horizontal,
      window,
    ),
    Background::from(frame_rule),
  ));
}

#[hotpath::measure]
fn paint_table_block(
  table: &LaidOutTable,
  selected_block: Option<BlockSelection>,
  table_cell_caret: Option<TableCellCaret>,
  text_selected: bool,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  window: &mut Window,
  cx: &mut App,
) {
  let table_selected = matches!(
    selected_block,
    Some(BlockSelection::Table(block_ix)) if block_ix == table.block_ix
  );
  let table_bounds = table.bounds.shift(origin);
  if !table_bounds.intersects(&content_mask) {
    return;
  }
  for (row_ix, row) in table.rows.iter().enumerate() {
    for (cell_ix, cell) in row.cells.iter().enumerate() {
      let cell_bounds = cell.bounds.shift(origin);
      if !cell_bounds.intersects(&content_mask) {
        continue;
      }
      let cell_selected = matches!(
        selected_block,
        Some(BlockSelection::TableCell { block_ix, row_ix: selected_row, cell_ix: selected_cell, .. })
          if block_ix == table.block_ix && selected_row == row_ix && selected_cell == cell_ix
      );
      // B-S1 theme-is-law + the header row finally renders: row 0 of a
      // header table gets a muted band instead of being indistinguishable.
      let cell_fill = if cell_selected {
        cx.theme().primary.opacity(0.14)
      } else if table.header_row && row_ix == 0 {
        cx.theme().muted
      } else {
        cx.theme().background
      };
      window.paint_quad(fill(cell_bounds, Background::from(cell_fill)));
      for block in &cell.blocks {
        match block {
          LaidOutBlock::Paragraph(paragraph) => {
            paint_table_paragraph_backgrounds(paragraph, origin, content_mask, window);
            if text_selected {
              paint_table_text_selection(paragraph, 0, paragraph.len, origin, content_mask, window);
            }
            if let Some(caret) = table_cell_caret
              && caret.block_ix == table.block_ix
              && caret.row_ix == row_ix
              && caret.cell_ix == cell_ix
              && caret.paragraph_block_ix == paragraph.index
            {
              paint_table_text_selection(paragraph, caret.anchor, caret.byte, origin, content_mask, window);
            }
            paint_table_paragraph(paragraph, origin, content_mask, window, cx);
            // Fidelity: a table-cell caret whose byte exceeds the laid-out
            // paragraph length is being resolved against a stale cell layout.
            if fidelity::enabled()
              && let Some(caret) = table_cell_caret
              && caret.block_ix == table.block_ix
              && caret.row_ix == row_ix
              && caret.cell_ix == cell_ix
              && caret.paragraph_block_ix == paragraph.index
              && caret.caret_visible
            {
              let resolved = caret_bounds_in_paragraph(paragraph, caret.byte, origin);
              fidelity::event(FidelityClass::Caret, "paint-table-cell", || {
                format!(
                  "block={} row={} cell={} paragraph={} byte={} paragraph_len={} rect={resolved:?}",
                  caret.block_ix, caret.row_ix, caret.cell_ix, paragraph.index, caret.byte, paragraph.len,
                )
              });
              fidelity::check(caret.byte <= paragraph.len, FidelityClass::Caret, "caret-render-stale", || {
                format!(
                  "table caret byte={} exceeds paragraph_len={} (block={} paragraph={})",
                  caret.byte, paragraph.len, caret.block_ix, paragraph.index,
                )
              });
            }
            if let Some(caret) = table_cell_caret
              && caret.block_ix == table.block_ix
              && caret.row_ix == row_ix
              && caret.cell_ix == cell_ix
              && caret.paragraph_block_ix == paragraph.index
              && caret.caret_visible
              && let Some(mut bounds) = caret_bounds_in_paragraph(paragraph, caret.byte, origin)
              && bounds.intersects(&content_mask)
            {
              bounds.size.width = px(1.0);
              window.paint_quad(fill(snap_vertical_rule_to_device_pixels(bounds, window), black()));
            }
          },
          LaidOutBlock::Table(table) => paint_table_block(table, None, None, text_selected, origin, content_mask, window, cx),
          LaidOutBlock::Image(object) => paint_object_block(object, "Image", None, origin, content_mask, window, cx),
          LaidOutBlock::Equation(object) => paint_object_block(object, "Equation", None, origin, content_mask, window, cx),
        }
      }
    }
  }
  paint_table_grid_rules(table, table_selected, origin, window, cx);
}

#[hotpath::measure]
fn paint_table_paragraph_backgrounds(paragraph: &LaidOutParagraph, origin: Point<Pixels>, content_mask: Bounds<Pixels>, window: &mut Window) {
  if !paragraph_intersects_mask(paragraph, origin, content_mask) {
    return;
  }
  for border in &paragraph.borders {
    let border_bounds = snap_rule_bounds(border.bounds.shift(origin), border.snap, window);
    window.paint_quad(fill(border_bounds, Background::from(border.color)));
  }
  for line in &paragraph.lines {
    if !line_intersects_mask(line, origin, content_mask) {
      continue;
    }
    for rect in &line.rects {
      let rect_bounds = snap_rule_bounds(rect.bounds.shift(origin + line.origin), rect.snap, window);
      window.paint_quad(fill(rect_bounds, Background::from(rect.color)));
    }
  }
}

#[hotpath::measure]
fn paint_table_grid_rules(table: &LaidOutTable, selected: bool, origin: Point<Pixels>, window: &mut Window, cx: &App) {
  // B-S1 theme-is-law: grid hairlines from theme slots.
  let color = if selected { cx.theme().primary } else { cx.theme().border };
  let background = Background::from(color);
  let mut horizontal = Vec::new();
  let mut vertical = Vec::new();
  for row in &table.rows {
    let top: f32 = row.top.into();
    let bottom: f32 = row.bottom.into();
    horizontal.push(top);
    horizontal.push(bottom);
    for cell in &row.cells {
      let left: f32 = cell.bounds.left().into();
      let right: f32 = cell.bounds.right().into();
      vertical.push(left);
      vertical.push(right);
    }
  }
  horizontal.sort_by(f32::total_cmp);
  vertical.sort_by(f32::total_cmp);
  horizontal.dedup_by(|a, b| (*a - *b).abs() < 0.5);
  vertical.dedup_by(|a, b| (*a - *b).abs() < 0.5);

  for y in horizontal {
    window.paint_quad(fill(
      snap_rule_bounds(
        Bounds::new(origin + point(table.bounds.left(), px(y)), size(table.bounds.size.width, px(1.0))),
        RuleSnap::Horizontal,
        window,
      ),
      background,
    ));
  }
  for x in vertical {
    window.paint_quad(fill(
      snap_rule_bounds(
        Bounds::new(origin + point(px(x), table.bounds.top()), size(px(1.0), table.bounds.size.height)),
        RuleSnap::Vertical,
        window,
      ),
      background,
    ));
  }
}

#[hotpath::measure]
fn paint_table_text_selection(
  paragraph: &LaidOutParagraph,
  anchor: usize,
  head: usize,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  window: &mut Window,
) {
  if anchor == head || !paragraph_intersects_mask(paragraph, origin, content_mask) {
    return;
  }
  let start = anchor.min(head);
  let end = anchor.max(head);
  for line in &paragraph.lines {
    if !line_intersects_mask(line, origin, content_mask) {
      continue;
    }
    let line_start = start.max(line.start_byte);
    let line_end = end.min(line.end_byte);
    if line_start >= line_end {
      continue;
    }
    let x1 = x_for_byte(line, line_start);
    let x2 = x_for_byte(line, line_end);
    window.paint_quad(fill(
      Bounds::new(origin + line.origin + point(x1, px(0.0)), size((x2 - x1).max(px(1.0)), line.line_height)),
      hsla(0.0, 0.0, 0.0, 0.22),
    ));
  }
}

#[hotpath::measure]
fn paint_table_paragraph(paragraph: &LaidOutParagraph, origin: Point<Pixels>, content_mask: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
  for line in &paragraph.lines {
    if line_intersects_mask(line, origin, content_mask) {
      paint_line_text(line, origin + line.origin, content_mask, window, cx);
    }
  }
  for line in &paragraph.lines {
    if !line_intersects_mask(line, origin, content_mask) {
      continue;
    }
    for underline in &line.underlines {
      window.paint_quad(fill(
        snap_horizontal_rule_to_device_pixels(underline.bounds.shift(origin + line.origin), window),
        Background::from(underline.color),
      ));
    }
    for strikethrough in &line.strikethroughs {
      window.paint_quad(fill(
        snap_horizontal_rule_to_device_pixels(strikethrough.bounds.shift(origin + line.origin), window),
        Background::from(strikethrough.color),
      ));
    }
  }
}

#[hotpath::measure]
pub(super) fn visible_paragraph_range(layout: &LayoutState, origin: Point<Pixels>, mask: Bounds<Pixels>) -> Range<usize> {
  if layout.paragraphs.is_empty() {
    return 0..0;
  }

  // Keep a little slack around the viewport so rules and selection edges do
  // not pop at the mask boundary while scrolling.
  let overscan = px(64.0);
  let top = mask.origin.y - origin.y - overscan;
  let bottom = mask.origin.y + mask.size.height - origin.y + overscan;
  let start = first_paragraph_with_bottom_at_or_after(&layout.paragraphs, top);
  let end = first_paragraph_with_top_after(&layout.paragraphs, bottom);
  start..end.max(start)
}
#[hotpath::measure]
pub(super) fn scroll_rect_into_view(scroll_handle: &ScrollHandle, rect: Bounds<Pixels>, margin: Pixels) {
  let viewport = scroll_handle.bounds();
  if viewport.size.height <= px(0.0) {
    return;
  }

  let top = rect.top() - margin;
  let bottom = rect.bottom() + margin;
  let mut offset = scroll_handle.offset();
  if top < viewport.top() {
    offset.y += viewport.top() - top;
  } else if bottom > viewport.bottom() {
    offset.y -= bottom - viewport.bottom();
  } else {
    return;
  }
  scroll_handle.set_offset(clamp_scroll_offset(scroll_handle, offset));
}

#[hotpath::measure]
pub(super) fn scroll_by(scroll_handle: &ScrollHandle, delta_y: Pixels) -> bool {
  if delta_y == px(0.0) {
    return false;
  }
  let old_offset = scroll_handle.offset();
  let mut offset = old_offset;
  offset.y -= delta_y;
  let offset = clamp_scroll_offset(scroll_handle, offset);
  if offset == old_offset {
    return false;
  }
  scroll_handle.set_offset(offset);
  true
}

#[hotpath::measure]
pub(super) fn clamp_scroll_offset(scroll_handle: &ScrollHandle, mut offset: Point<Pixels>) -> Point<Pixels> {
  let max = scroll_handle.max_offset();
  offset.x = offset.x.min(px(0.0)).max(-max.width);
  offset.y = offset.y.min(px(0.0)).max(-max.height);
  offset
}

#[hotpath::measure]
pub(super) fn drag_autoscroll_step(viewport: Bounds<Pixels>, position: Point<Pixels>) -> Pixels {
  if viewport.size.height <= px(0.0) {
    return px(0.0);
  }

  let edge = px(36.0);
  let max_step = px(28.0);
  if position.y < viewport.top() {
    -(edge + viewport.top() - position.y).min(max_step)
  } else if position.y < viewport.top() + edge {
    -(viewport.top() + edge - position.y).min(max_step)
  } else if position.y > viewport.bottom() {
    (edge + position.y - viewport.bottom()).min(max_step)
  } else if position.y > viewport.bottom() - edge {
    (position.y - (viewport.bottom() - edge)).min(max_step)
  } else {
    px(0.0)
  }
}

#[hotpath::measure]
pub(super) fn paragraph_intersects_mask(paragraph: &LaidOutParagraph, origin: Point<Pixels>, mask: Bounds<Pixels>) -> bool {
  vertical_range_intersects(origin.y + paragraph.top, origin.y + paragraph.bottom, mask)
}

#[hotpath::measure]
pub(super) fn line_intersects_mask(line: &LaidOutLine, origin: Point<Pixels>, mask: Bounds<Pixels>) -> bool {
  vertical_range_intersects(origin.y + line.origin.y, origin.y + line.origin.y + line.line_height, mask)
}

#[hotpath::measure]
pub(super) fn vertical_range_intersects(top: Pixels, bottom: Pixels, mask: Bounds<Pixels>) -> bool {
  let mask_top = mask.origin.y;
  let mask_bottom = mask.origin.y + mask.size.height;
  bottom >= mask_top && top <= mask_bottom
}

#[hotpath::measure]
pub(super) fn snap_horizontal_rule_to_device_pixels(mut bounds: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
  let scale = window.scale_factor();
  bounds.origin.y = snap_pixel_to_device_grid(bounds.origin.y, scale);
  bounds.size.height = snap_rule_thickness_to_device_grid(bounds.size.height, scale);
  bounds
}

#[hotpath::measure]
pub(super) fn snap_rule_bounds(bounds: Bounds<Pixels>, snap: RuleSnap, window: &Window) -> Bounds<Pixels> {
  match snap {
    RuleSnap::None => bounds,
    RuleSnap::Horizontal => snap_horizontal_rule_to_device_pixels(bounds, window),
    RuleSnap::Vertical => snap_vertical_rule_to_device_pixels(bounds, window),
  }
}

#[hotpath::measure]
pub(super) fn snap_vertical_rule_to_device_pixels(mut bounds: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
  let scale = window.scale_factor();
  bounds.origin.x = snap_pixel_to_device_grid(bounds.origin.x, scale);
  bounds.size.width = snap_rule_thickness_to_device_grid(bounds.size.width, scale);
  bounds
}

#[hotpath::measure]
pub(super) fn snap_pixel_to_device_grid(value: Pixels, scale: f32) -> Pixels {
  let value: f32 = value.into();
  px((value * scale).round() / scale)
}

#[hotpath::measure]
pub(super) fn snap_rule_thickness_to_device_grid(value: Pixels, scale: f32) -> Pixels {
  let value: f32 = value.into();
  px(((value * scale).round().max(1.0)) / scale)
}

#[hotpath::measure]
fn paint_search_highlights(
  layout: &LayoutState,
  highlights: &[Range<DocumentOffset>],
  active: Option<usize>,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  visible_range: Range<usize>,
  window: &mut Window,
) {
  if highlights.is_empty() {
    return;
  }
  let visible_start = layout
    .paragraphs
    .get(visible_range.start)
    .map_or(usize::MAX, |paragraph| paragraph.index);
  let visible_end = layout
    .paragraphs
    .get(visible_range.end.saturating_sub(1))
    .map_or(0, |paragraph| paragraph.index);
  for (ix, highlight) in highlights.iter().enumerate() {
    if highlight.end.paragraph < visible_start || highlight.start.paragraph > visible_end {
      continue;
    }
    let selection = EditorSelection::range(highlight.start, highlight.end);
    paint_text_range_fill(
      layout,
      &selection,
      origin,
      content_mask,
      visible_range.clone(),
      if Some(ix) == active {
        hsla(48.0 / 360.0, 1.0, 0.55, 0.95)
      } else {
        hsla(55.0 / 360.0, 1.0, 0.72, 0.86)
      },
      window,
    );
  }
}

pub(super) fn paint_selection(
  layout: &LayoutState,
  selection: &EditorSelection,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  visible_range: Range<usize>,
  window: &mut Window,
) {
  paint_text_range_fill(layout, selection, origin, content_mask, visible_range, hsla(0.0, 0.0, 0.0, 0.22), window);
}

/// A peer's presence color softened for use as a selection highlight: the caret
/// paints at full opacity, but a span behind the glyphs must stay readable, so
/// drop it to a translucent fill.
fn remote_selection_color(color_rgb: u32) -> Hsla {
  Hsla::from(rgb(color_rgb)).opacity(0.30)
}

/// The comment-mark underline: strong enough to find, quiet enough to read
/// over. Hover lifts it to full strength.
fn annotation_underline_color(color_rgb: u32, hovered: bool) -> Hsla {
  gpui::Hsla::from(rgb(color_rgb)).opacity(if hovered { 0.95 } else { 0.6 })
}

/// Stronger than the annotation fill: the flash must register as an event.
fn jump_flash_color(color_rgb: u32) -> Hsla {
  gpui::Hsla::from(rgb(color_rgb)).opacity(0.38)
}

fn paint_text_range_fill(
  layout: &LayoutState,
  selection: &EditorSelection,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  visible_range: Range<usize>,
  color: impl Into<Background> + Clone,
  window: &mut Window,
) {
  if selection.is_caret() {
    return;
  }
  let range = selection.normalized();
  for paragraph in &layout.paragraphs[visible_range] {
    if paragraph.index < range.start.paragraph || paragraph.index > range.end.paragraph {
      continue;
    }
    if !paragraph_intersects_mask(paragraph, origin, content_mask) {
      continue;
    }
    let start = if paragraph.index == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph.index == range.end.paragraph {
      range.end.byte
    } else {
      paragraph.len
    };
    for line in &paragraph.lines {
      if !line_intersects_mask(line, origin, content_mask) {
        continue;
      }
      let line_start = start.max(line.start_byte);
      let line_end = end.min(line.end_byte);
      if line_start > line_end || (line_start == line_end && !(line.start_byte == line.end_byte && start <= line_start && end >= line_end)) {
        continue;
      }
      let x1 = x_for_byte(line, line_start);
      let x2 = if line_start == line_end {
        x1 + px(8.0)
      } else {
        x_for_byte(line, line_end)
      };
      window.paint_quad(fill(
        Bounds::new(origin + line.origin + point(x1, px(0.0)), size((x2 - x1).max(px(1.0)), line.line_height)),
        color.clone(),
      ));
    }
  }
}

/// The C-S4 review mark: a dashed underline along the baseline of every line
/// segment a range covers. Same range→segment geometry as
/// [`paint_text_range_fill`]; only the ink differs — short quads with gaps
/// instead of a translucent block, so marked text reads exactly as it will
/// read with the panel closed.
#[allow(clippy::too_many_arguments, reason = "mirrors paint_text_range_fill, which shares this shape")]
fn paint_text_range_dashed_underline(
  layout: &LayoutState,
  selection: &EditorSelection,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  visible_range: Range<usize>,
  color: Hsla,
  thickness: Pixels,
  window: &mut Window,
) {
  const DASH: f32 = 4.0;
  const GAP: f32 = 3.0;
  if selection.is_caret() {
    return;
  }
  let range = selection.normalized();
  for paragraph in &layout.paragraphs[visible_range] {
    if paragraph.index < range.start.paragraph || paragraph.index > range.end.paragraph {
      continue;
    }
    if !paragraph_intersects_mask(paragraph, origin, content_mask) {
      continue;
    }
    let start = if paragraph.index == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph.index == range.end.paragraph {
      range.end.byte
    } else {
      paragraph.len
    };
    for line in &paragraph.lines {
      if !line_intersects_mask(line, origin, content_mask) {
        continue;
      }
      let line_start = start.max(line.start_byte);
      let line_end = end.min(line.end_byte);
      if line_start > line_end || (line_start == line_end && !(line.start_byte == line.end_byte && start <= line_start && end >= line_end)) {
        continue;
      }
      let x1 = x_for_byte(line, line_start);
      let x2 = if line_start == line_end {
        x1 + px(8.0)
      } else {
        x_for_byte(line, line_end)
      };
      // Sit just under the baseline, clamped inside the line box so tight
      // line heights never spill the mark into the next line.
      let y = (line.baseline_y() + px(2.0)).min(line.line_height - thickness);
      let mut x = x1;
      let x2 = x2.max(x1 + px(DASH));
      while x < x2 {
        let dash_end = (x + px(DASH)).min(x2);
        window.paint_quad(fill(
          Bounds::new(origin + line.origin + point(x, y), size(dash_end - x, thickness)),
          color,
        ));
        x = dash_end + px(GAP);
      }
    }
  }
}

#[hotpath::measure]
pub(super) fn paint_line_text(line: &LaidOutLine, origin: Point<Pixels>, content_mask: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
  let _ = cx;
  let baseline = line.baseline_y();
  let line_bounds = Bounds::new(origin, size(px(f32::MAX / 4.0), line.line_height));
  if !line_bounds.intersects(&content_mask) {
    return;
  }
  for segment in &line.segments {
    let segment_origin = origin + point(segment.x, baseline);
    // §perf: run_bounds is a function of the segment only (not the run), so compute
    // it and the mask test once per segment rather than once per shaped run; an
    // off-mask segment now skips all its runs in a single test.
    let run_bounds = Bounds::new(
      point(segment_origin.x, origin.y + baseline - segment.ascent),
      size(segment.width.max(px(1.0)), segment.ascent + segment.descent),
    );
    if !run_bounds.intersects(&content_mask) {
      continue;
    }
    for run in &segment.shaped.runs {
      for glyph in &run.glyphs {
        let glyph_origin = segment_origin + point(glyph.position.x, px(0.0));
        let result = if glyph.is_emoji {
          window.paint_emoji(glyph_origin, run.font_id, glyph.id, segment.font_size)
        } else {
          window.paint_glyph(glyph_origin, run.font_id, glyph.id, segment.font_size, segment.format.color)
        };
        if let Err(error) = result {
          tracing::warn!(%error, "failed to paint glyph");
        }
      }
    }
  }
}
trait ShiftBounds {
  fn shift(self, by: Point<Pixels>) -> Self;
}

#[hotpath::measure_all]
impl ShiftBounds for Bounds<Pixels> {
  fn shift(self, by: Point<Pixels>) -> Self {
    Bounds::new(self.origin + by, self.size)
  }
}
