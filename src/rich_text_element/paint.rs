use std::ops::Range;

use gpui::{App, Background, Bounds, Pixels, Point, ScrollHandle, Window, black, fill, hsla, point, px, size};

use super::*;

pub(super) fn paint_layout(layout: &LayoutState, selection: Option<&EditorSelection>, show_caret: bool, window: &mut Window, cx: &mut App) {
  let timing = Instant::now();
  let Some(bounds) = layout.bounds else {
    return;
  };
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
    }
  }
  if let Some(selection) = selection {
    paint_selection(layout, selection, bounds.origin, content_mask, visible_range, window);
  }
  if let Some(selection) = selection
    && selection.is_caret()
    && show_caret
    && let Some(caret) = caret_bounds(layout, selection.head, bounds.origin)
    && caret.intersects(&content_mask)
  {
    window.paint_quad(fill(snap_vertical_rule_to_device_pixels(caret, window), black()));
  }
  log_timing("paint layout", timing, format!("visible_paragraphs={visible_count}"));
}

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

pub(super) fn clamp_scroll_offset(scroll_handle: &ScrollHandle, mut offset: Point<Pixels>) -> Point<Pixels> {
  let max = scroll_handle.max_offset();
  offset.x = offset.x.min(px(0.0)).max(-max.width);
  offset.y = offset.y.min(px(0.0)).max(-max.height);
  offset
}

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

pub(super) fn paragraph_intersects_mask(paragraph: &LaidOutParagraph, origin: Point<Pixels>, mask: Bounds<Pixels>) -> bool {
  vertical_range_intersects(origin.y + paragraph.top, origin.y + paragraph.bottom, mask)
}

pub(super) fn line_intersects_mask(line: &LaidOutLine, origin: Point<Pixels>, mask: Bounds<Pixels>) -> bool {
  vertical_range_intersects(origin.y + line.origin.y, origin.y + line.origin.y + line.line_height, mask)
}

pub(super) fn vertical_range_intersects(top: Pixels, bottom: Pixels, mask: Bounds<Pixels>) -> bool {
  let mask_top = mask.origin.y;
  let mask_bottom = mask.origin.y + mask.size.height;
  bottom >= mask_top && top <= mask_bottom
}

pub(super) fn snap_horizontal_rule_to_device_pixels(mut bounds: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
  let scale = window.scale_factor();
  bounds.origin.y = snap_pixel_to_device_grid(bounds.origin.y, scale);
  bounds.size.height = snap_rule_thickness_to_device_grid(bounds.size.height, scale);
  bounds
}

pub(super) fn snap_rule_bounds(bounds: Bounds<Pixels>, snap: RuleSnap, window: &Window) -> Bounds<Pixels> {
  match snap {
    RuleSnap::None => bounds,
    RuleSnap::Horizontal => snap_horizontal_rule_to_device_pixels(bounds, window),
    RuleSnap::Vertical => snap_vertical_rule_to_device_pixels(bounds, window),
  }
}

pub(super) fn snap_vertical_rule_to_device_pixels(mut bounds: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
  let scale = window.scale_factor();
  bounds.origin.x = snap_pixel_to_device_grid(bounds.origin.x, scale);
  bounds.size.width = snap_rule_thickness_to_device_grid(bounds.size.width, scale);
  bounds
}

pub(super) fn snap_pixel_to_device_grid(value: Pixels, scale: f32) -> Pixels {
  let value: f32 = value.into();
  px((value * scale).round() / scale)
}

pub(super) fn snap_rule_thickness_to_device_grid(value: Pixels, scale: f32) -> Pixels {
  let value: f32 = value.into();
  px(((value * scale).round().max(1.0)) / scale)
}

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
}

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

impl ShiftBounds for Bounds<Pixels> {
  fn shift(self, by: Point<Pixels>) -> Self {
    Bounds::new(self.origin + by, self.size)
  }
}
