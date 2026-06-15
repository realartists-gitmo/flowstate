const MIN_ZOOM_PERCENT: f32 = 25.0;
const MAX_ZOOM_PERCENT: f32 = 400.0;
const ZOOM_STEP_PERCENT: f32 = 5.0;

#[derive(Clone, Copy)]
struct ZoomAnchorSnapshot {
  target: ZoomAnchorTarget,
  viewport_y_ratio: f32,
  viewport_size: Size<Pixels>,
  scroll_y: Pixels,
  edit_generation: u64,
  invisibility_mode: bool,
}

#[derive(Clone, Copy)]
enum ZoomAnchorTarget {
  Text {
    offset: DocumentOffset,
    line_y_ratio: f32,
  },
  Block {
    block_ix: usize,
    block_y_ratio: f32,
  },
  Document {
    content_y_ratio: f32,
  },
}

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn zoom_percent(&self) -> f32 {
    self.zoom_percent
  }

  pub fn set_zoom_percent(&mut self, percent: f32, cx: &mut Context<Self>) {
    let percent = ((percent / ZOOM_STEP_PERCENT).round() * ZOOM_STEP_PERCENT).clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT);
    if (self.zoom_percent - percent).abs() < f32::EPSILON {
      return;
    }
    if self.zoom_anchor.is_none_or(|anchor| !self.zoom_anchor_is_current(anchor)) {
      self.zoom_anchor = self.capture_zoom_anchor();
    }
    self.zoom_percent = percent;
    self.document.theme.zoom_factor = percent / 100.0;
    self.zoom_anchor_apply_pending = true;
    self.invalidate_document_layout_caches();
    cx.notify();
  }

  pub(super) fn prepare_pending_zoom_anchor(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    if !self.zoom_anchor_apply_pending {
      return;
    }
    let Some(ZoomAnchorSnapshot {
      target: ZoomAnchorTarget::Text { offset, .. },
      ..
    }) = self.zoom_anchor
    else {
      return;
    };
    let _ = self.ensure_paragraph_chunk_containing_byte(offset.paragraph, offset.byte, width, window, cx);
  }

  pub(super) fn apply_pending_zoom_anchor(&mut self) {
    if !self.zoom_anchor_apply_pending {
      return;
    }
    let Some(anchor) = self.zoom_anchor else {
      self.zoom_anchor_apply_pending = false;
      return;
    };
    self.restore_preserved_zoom_anchor(Some(anchor));
  }

  pub(super) fn reset_zoom_anchor(&mut self) {
    self.zoom_anchor = None;
    self.zoom_anchor_apply_pending = false;
  }

  fn active_zoom_anchor(&self) -> Option<ZoomAnchorSnapshot> {
    self.zoom_anchor.filter(|anchor| self.zoom_anchor_is_current(*anchor))
  }

  fn restore_preserved_zoom_anchor(&mut self, anchor: Option<ZoomAnchorSnapshot>) {
    let Some(mut anchor) = anchor else {
      return;
    };
    if self.restore_zoom_anchor(anchor) {
      anchor.scroll_y = self.scroll_handle.offset().y;
      self.zoom_anchor = Some(anchor);
      self.zoom_anchor_apply_pending = false;
    }
  }

  fn zoom_anchor_is_current(&self, anchor: ZoomAnchorSnapshot) -> bool {
    anchor.edit_generation == self.edit_generation
      && anchor.invisibility_mode == self.invisibility_mode
      && sizes_are_close(anchor.viewport_size, self.scroll_handle.bounds().size, px(0.5))
      && (anchor.scroll_y - self.scroll_handle.offset().y).abs() <= px(0.5)
  }

  fn capture_zoom_anchor(&self) -> Option<ZoomAnchorSnapshot> {
    let viewport = self.scroll_handle.bounds();
    if viewport.size.height <= px(1.0) {
      return None;
    }
    let viewport_y_ratio = 0.5;
    let view_y = viewport.size.height * viewport_y_ratio;
    let content_y = (-self.scroll_handle.offset().y + view_y).max(px(0.0));
    let valid_cache = self
      .item_sizes_cache
      .as_ref()
      .filter(|cache| cache.item_count > 0 && self.height_prefix_index.len() == cache.item_count);
    let total_height = valid_cache
      .map_or_else(|| self.scroll_handle.content_size().height, |_| px(self.height_prefix_index.total_height()))
      .max(px(1.0));
    let document_target = ZoomAnchorTarget::Document {
      content_y_ratio: (content_y / total_height).clamp(0.0, 1.0),
    };
    let Some(cache) = valid_cache else {
      return Some(ZoomAnchorSnapshot {
        target: document_target,
        viewport_y_ratio,
        viewport_size: viewport.size,
        scroll_y: self.scroll_handle.offset().y,
        edit_generation: self.edit_generation,
        invisibility_mode: self.invisibility_mode,
      });
    };
    let item_ix = self.height_prefix_index.lower_bound(content_y);
    let target = match cache.items.get(item_ix) {
      Some(VirtualItem::ParagraphChunk { .. } | VirtualItem::ParagraphRemainder { .. }) => {
        let position = point(viewport.left() + viewport.size.width / 2.0, viewport.top() + view_y);
        self.hit_test_cached_position(position).map_or(document_target, |offset| ZoomAnchorTarget::Text {
          offset,
          line_y_ratio: self
            .zoom_text_line_y_ratio(offset, content_y)
            .unwrap_or(0.5),
        })
      },
      Some(VirtualItem::StructuralBlock { block_ix }) => {
        let item_top = self.height_prefix_index.item_top(item_ix);
        let item_height = cache
          .sizes
          .get(item_ix)
          .map(|size| size.height)
          .unwrap_or(px(1.0))
          .max(px(1.0));
        ZoomAnchorTarget::Block {
          block_ix: *block_ix,
          block_y_ratio: ((content_y - item_top) / item_height).clamp(0.0, 1.0),
        }
      },
      Some(VirtualItem::HiddenBlock { .. }) | None => document_target,
    };
    Some(ZoomAnchorSnapshot {
      target,
      viewport_y_ratio,
      viewport_size: viewport.size,
      scroll_y: self.scroll_handle.offset().y,
      edit_generation: self.edit_generation,
      invisibility_mode: self.invisibility_mode,
    })
  }

  fn zoom_text_line_y_ratio(&self, offset: DocumentOffset, content_y: Pixels) -> Option<f32> {
    let width = self.current_layout_width();
    let (chunk_ix, layout) = self.paragraph_chunk_containing_byte(offset.paragraph, offset.byte, width)?;
    let item_top = self.item_top_for_paragraph_chunk(offset.paragraph, chunk_ix)?;
    let caret = caret_bounds(&layout, offset, point(px(0.0), item_top))?;
    Some(relative_line_y(content_y, caret.top(), caret.size.height))
  }

  fn zoom_text_anchor_content_y(&self, offset: DocumentOffset, line_y_ratio: f32) -> Option<Pixels> {
    let width = self.current_layout_width();
    let (chunk_ix, layout) = self.paragraph_chunk_containing_byte(offset.paragraph, offset.byte, width)?;
    let item_top = self.item_top_for_paragraph_chunk(offset.paragraph, chunk_ix)?;
    let caret = caret_bounds(&layout, offset, point(px(0.0), item_top))?;
    Some(line_y_from_relative(caret.top(), caret.size.height, line_y_ratio))
  }

  fn restore_zoom_anchor(&mut self, anchor: ZoomAnchorSnapshot) -> bool {
    let viewport = self.scroll_handle.bounds();
    if viewport.size.height <= px(1.0) || self.height_prefix_index.len() == 0 {
      return false;
    }
    let total_height = px(self.height_prefix_index.total_height());
    let target_y = match anchor.target {
      ZoomAnchorTarget::Text { offset, line_y_ratio } => {
        let Some(target_y) = self.zoom_text_anchor_content_y(offset, line_y_ratio) else {
          return false;
        };
        target_y
      },
      ZoomAnchorTarget::Block {
        block_ix,
        block_y_ratio,
      } => {
        let Some(cache) = &self.item_sizes_cache else {
          return false;
        };
        let Some(block_top) = self.block_top_for_index(block_ix) else {
          return false;
        };
        let Some(block_height) = cache.block_heights.get(block_ix).copied() else {
          return false;
        };
        block_top + block_height * block_y_ratio
      },
      ZoomAnchorTarget::Document { content_y_ratio } => total_height * content_y_ratio,
    };
    let mut offset = self.scroll_handle.offset();
    offset.y = -anchored_scroll_top(
      target_y,
      viewport.size.height * anchor.viewport_y_ratio,
      total_height,
      viewport.size.height,
    );
    self.scroll_handle.set_offset(offset);
    true
  }

  fn zoom_by(&mut self, delta_percent: f32, cx: &mut Context<Self>) {
    self.set_zoom_percent(self.zoom_percent + delta_percent, cx);
  }

  pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
    self.zoom_by(ZOOM_STEP_PERCENT, cx);
  }

  pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
    self.zoom_by(-ZOOM_STEP_PERCENT, cx);
  }
}

fn anchored_scroll_top(target_y: Pixels, viewport_y: Pixels, total_height: Pixels, viewport_height: Pixels) -> Pixels {
  let max_scroll_top = (total_height - viewport_height).max(px(0.0));
  (target_y - viewport_y).max(px(0.0)).min(max_scroll_top)
}

fn relative_line_y(content_y: Pixels, line_top: Pixels, line_height: Pixels) -> f32 {
  (content_y - line_top) / line_height.max(px(1.0))
}

fn line_y_from_relative(line_top: Pixels, line_height: Pixels, relative_y: f32) -> Pixels {
  line_top + line_height.max(px(1.0)) * relative_y
}

fn sizes_are_close(left: Size<Pixels>, right: Size<Pixels>, tolerance: Pixels) -> bool {
  (left.width - right.width).abs() <= tolerance && (left.height - right.height).abs() <= tolerance
}

#[cfg(test)]
mod zoom_tests {
  use super::*;

  #[test]
  fn semantic_anchor_round_trips_through_short_intermediate_layouts() {
    let viewport_height = px(600.0);
    let viewport_y = viewport_height / 2.0;
    let original_target = px(1500.0);
    let original_scroll = anchored_scroll_top(original_target, viewport_y, px(3000.0), viewport_height);

    assert_eq!(original_scroll, px(1200.0));
    assert_eq!(
      anchored_scroll_top(px(150.0), viewport_y, px(300.0), viewport_height),
      px(0.0)
    );
    assert_eq!(
      anchored_scroll_top(original_target, viewport_y, px(3000.0), viewport_height),
      original_scroll
    );
  }

  #[test]
  fn anchor_clamps_only_against_the_new_layout_extent() {
    assert_eq!(
      anchored_scroll_top(px(1900.0), px(300.0), px(2000.0), px(600.0)),
      px(1400.0)
    );
  }

  #[test]
  fn semantic_line_anchor_preserves_positions_outside_the_line_box() {
    let line_top = px(100.0);
    let line_height = px(20.0);

    for content_y in [px(92.0), px(110.0), px(129.0)] {
      let relative_y = relative_line_y(content_y, line_top, line_height);
      assert_eq!(line_y_from_relative(line_top, line_height, relative_y), content_y);
    }
  }

  #[test]
  fn semantic_anchor_is_invalidated_when_viewport_geometry_changes() {
    assert!(sizes_are_close(
      size(px(900.0), px(600.0)),
      size(px(900.25), px(599.75)),
      px(0.5),
    ));
    assert!(!sizes_are_close(
      size(px(900.0), px(600.0)),
      size(px(1100.0), px(600.0)),
      px(0.5),
    ));
  }
}
