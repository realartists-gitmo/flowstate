use std::ops::Range;

use gpui::{App, Background, Bounds, Pixels, Point, ScrollHandle, Window, black, fill, hsla, point, px, rgb, size};

use super::*;

#[hotpath::measure]
pub(super) fn paint_layout(
  layout: &LayoutState,
  bounds: Bounds<Pixels>,
  selection: Option<&EditorSelection>,
  drag_selection: Option<&EditorSelection>,
  show_caret: bool,
  caret_width: Pixels,
  window: &mut Window,
  cx: &mut App,
) {
  let timing = Instant::now();
  let content_mask = window.content_mask().bounds;
  let visible_range = visible_paragraph_range(layout, bounds.origin, content_mask);
  let visible_count = visible_range.end.saturating_sub(visible_range.start);
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
  if let Some(selection) = selection {
    paint_selection(layout, selection, bounds.origin, content_mask, visible_range.clone(), window);
  }
  if let Some(selection) = drag_selection {
    paint_selection(layout, selection, bounds.origin, content_mask, visible_range.clone(), window);
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
  if let Some(selection) = selection
    && selection.is_caret()
    && show_caret
    && let Some(mut caret) = caret_bounds(layout, selection.head, bounds.origin)
    && caret.intersects(&content_mask)
  {
    caret.size.width = caret_width;
    window.paint_quad(fill(snap_vertical_rule_to_device_pixels(caret, window), black()));
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
    LaidOutBlock::Image(object) => paint_object_block(object, "Image", selected_block, origin, content_mask, window),
    LaidOutBlock::Equation(object) => paint_object_block(object, "Equation", selected_block, origin, content_mask, window),
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
) {
  let bounds = object.bounds.shift(origin);
  if !bounds.intersects(&content_mask) {
    return;
  }
  let selected = matches!(
    selected_block,
    Some(BlockSelection::Image(ix) | BlockSelection::Equation(ix)) if ix == object.block_ix
  );
  window.paint_quad(fill(bounds, Background::from(rgb(0xffffff))));
  window.paint_quad(fill(
    snap_rule_bounds(Bounds::new(bounds.origin, size(bounds.size.width, px(1.0))), RuleSnap::Horizontal, window),
    Background::from(if selected { rgb(0x0969da) } else { rgb(0xb7b7b7) }),
  ));
  window.paint_quad(fill(
    snap_rule_bounds(
      Bounds::new(point(bounds.origin.x, bounds.bottom() - px(1.0)), size(bounds.size.width, px(1.0))),
      RuleSnap::Horizontal,
      window,
    ),
    Background::from(if selected { rgb(0x0969da) } else { rgb(0xb7b7b7) }),
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
        Some(BlockSelection::TableCell { block_ix, row_ix: selected_row, cell_ix: selected_cell })
          if block_ix == table.block_ix && selected_row == row_ix && selected_cell == cell_ix
      );
      window.paint_quad(fill(
        cell_bounds,
        Background::from(if cell_selected { rgb(0xeaf4ff) } else { rgb(0xffffff) }),
      ));
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
          LaidOutBlock::Image(object) => paint_object_block(object, "Image", None, origin, content_mask, window),
          LaidOutBlock::Equation(object) => paint_object_block(object, "Equation", None, origin, content_mask, window),
        }
      }
    }
  }
  paint_table_grid_rules(table, table_selected, origin, window);
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
fn paint_table_grid_rules(table: &LaidOutTable, selected: bool, origin: Point<Pixels>, window: &mut Window) {
  let color = if selected { rgb(0x0969da) } else { rgb(0x808080) };
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
pub(super) fn paint_selection(
  layout: &LayoutState,
  selection: &EditorSelection,
  origin: Point<Pixels>,
  content_mask: Bounds<Pixels>,
  visible_range: Range<usize>,
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
        hsla(0.0, 0.0, 0.0, 0.22),
      ));
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
    for run in &segment.shaped.runs {
      let run_bounds = Bounds::new(
        point(segment_origin.x, origin.y + baseline - segment.ascent),
        size(segment.width.max(px(1.0)), segment.ascent + segment.descent),
      );
      if !run_bounds.intersects(&content_mask) {
        continue;
      }
      for glyph in &run.glyphs {
        let glyph_origin = segment_origin + point(glyph.position.x, px(0.0));
        let result = if glyph.is_emoji {
          window.paint_emoji(glyph_origin, run.font_id, glyph.id, segment.font_size)
        } else {
          window.paint_glyph(glyph_origin, run.font_id, glyph.id, segment.font_size, segment.format.color)
        };
        if let Err(error) = result {
          eprintln!("failed to paint glyph: {error}");
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
