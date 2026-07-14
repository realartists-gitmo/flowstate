#[hotpath::measure_all]
impl RichTextEditor {
  fn capture_scroll_anchor(&mut self) -> Option<ScrollAnchorSnapshot> {
    if let Some(anchor) = self.capture_locked_scroll_anchor() {
      return Some(anchor);
    }
    if let Some(anchor) = self.capture_live_scroll_anchor() {
      self.remember_scroll_anchor(anchor.clone());
      return Some(anchor);
    }
    if self.viewport_can_anchor_live_scroll() {
      None
    } else {
      self.last_scroll_anchor.clone()
    }
  }

  fn capture_locked_scroll_anchor(&mut self) -> Option<ScrollAnchorSnapshot> {
    if !self.viewport_can_anchor_live_scroll() {
      return None;
    }
    let Some(lock) = &self.scroll_anchor_lock else {
      return None;
    };
    if (lock.offset_y - self.scroll_handle.offset().y).abs() > px(0.001) {
      self.scroll_anchor_lock = None;
      return None;
    }
    Some(lock.anchor.clone())
  }

  fn capture_live_scroll_anchor(&self) -> Option<ScrollAnchorSnapshot> {
    if !self.viewport_can_anchor_live_scroll() {
      return None;
    }
    if let Some(anchor) = self.capture_visible_chunk_scroll_anchor() {
      return Some(anchor);
    }
    let cache = self.item_sizes_cache.as_ref()?;
    if self.height_prefix_index.len() != cache.item_count || cache.item_count == 0 {
      return None;
    }
    let content_y = (-self.scroll_handle.offset().y).max(px(0.0));
    let item_ix = self.height_prefix_index.lower_bound(content_y);
    let item = cache.items.get(item_ix)?.clone();
    let item_top = self.height_prefix_index.item_top(item_ix);
    let delta = (content_y - item_top).max(px(0.0));
    match item {
      VirtualItem::ParagraphRemainder { paragraph_ix, .. } => {
        let block_ix = self.block_ix_for_paragraph(paragraph_ix)?;
        let paragraph_start_item = self.item_sizes_cache.as_ref()?.block_item_ranges.get(block_ix)?.start;
        let paragraph_top = self.height_prefix_index.item_top(paragraph_start_item);
        Some(ScrollAnchorSnapshot::ParagraphRemainder {
          paragraph_ix,
          start_byte: 0,
          delta: (content_y - paragraph_top).max(px(0.0)),
        })
      },
      item => Some(ScrollAnchorSnapshot::Item { item, delta }),
    }
  }

  fn capture_visible_chunk_scroll_anchor(&self) -> Option<ScrollAnchorSnapshot> {
    let viewport = self.scroll_handle.bounds();
    let scroll_y = self.scroll_handle.offset().y;
    let mut containing_top = None;
    let mut below_top = None;

    for anchor in &self.visible_chunk_anchors {
      if (anchor.scroll_y - scroll_y).abs() > px(0.1) {
        continue;
      }
      if anchor.bounds.bottom() <= viewport.top() || anchor.bounds.top() >= viewport.bottom() {
        continue;
      }
      if anchor.bounds.top() <= viewport.top() {
        if containing_top.is_none_or(|best: &VisibleChunkAnchor| anchor.bounds.top() > best.bounds.top()) {
          containing_top = Some(anchor);
        }
      } else if below_top.is_none_or(|best: &VisibleChunkAnchor| anchor.bounds.top() < best.bounds.top()) {
        below_top = Some(anchor);
      }
    }

    let anchor = containing_top.or(below_top)?;
    let block_ix = self.block_ix_for_paragraph(anchor.paragraph_ix)?;
    Some(ScrollAnchorSnapshot::Item {
      item: VirtualItem::ParagraphChunk {
        block_ix,
        paragraph_ix: anchor.paragraph_ix,
        chunk_ix: anchor.chunk_ix,
      },
      delta: viewport.top() - anchor.bounds.top(),
    })
  }

  fn viewport_can_anchor_live_scroll(&self) -> bool {
    let viewport = self.scroll_handle.bounds();
    viewport.size.height > px(1.0)
  }

  fn remember_scroll_anchor(&mut self, anchor: ScrollAnchorSnapshot) {
    self.last_scroll_anchor = Some(anchor.clone());
    if self.viewport_can_anchor_live_scroll() {
      self.scroll_anchor_lock = Some(ScrollAnchorLock {
        anchor,
        offset_y: self.scroll_handle.offset().y,
      });
    }
  }

  fn restore_scroll_anchor(&mut self, anchor: Option<ScrollAnchorSnapshot>) {
    let Some(anchor) = anchor else {
      return;
    };
    let Some((item_ix, delta)) = self.scroll_anchor_item_and_delta(&anchor) else {
      self.scroll_anchor_lock = None;
      return;
    };
    if self.height_prefix_index.len() == 0 {
      self.scroll_anchor_lock = None;
      return;
    }
    let viewport_height = self.scroll_handle.bounds().size.height.max(px(0.0));
    let max_scroll_top = (px(self.height_prefix_index.total_height()) - viewport_height).max(px(0.0));
    let scroll_top = (self.height_prefix_index.item_top(item_ix) + delta)
      .max(px(0.0))
      .min(max_scroll_top);
    let mut offset = self.scroll_handle.offset();
    let new_y = -scroll_top;
    if offset.y != new_y {
      offset.y = new_y;
      self.scroll_handle.set_offset(offset);
    }
    self.last_scroll_anchor = Some(anchor.clone());
    if self.viewport_can_anchor_live_scroll() {
      self.scroll_anchor_lock = Some(ScrollAnchorLock { anchor, offset_y: new_y });
    }
  }

  fn scroll_anchor_item_and_delta(&self, anchor: &ScrollAnchorSnapshot) -> Option<(usize, Pixels)> {
    let cache = self.item_sizes_cache.as_ref()?;
    if self.height_prefix_index.len() != cache.item_count {
      return None;
    }
    match anchor {
      ScrollAnchorSnapshot::Item { item, .. } => match item {
        VirtualItem::ParagraphChunk { paragraph_ix, chunk_ix, .. } => self
          .paragraph_chunk_item_ix(*paragraph_ix, *chunk_ix)
          .map(|item_ix| (item_ix, anchor.delta())),
        VirtualItem::ParagraphRemainder { paragraph_ix, .. } => cache
          .paragraph_remainder_items
          .get(*paragraph_ix)
          .copied()
          .and_then(decode_remainder_item_ix)
          .map(|item_ix| (item_ix, anchor.delta())),
        VirtualItem::StructuralBlock { block_ix } | VirtualItem::HiddenBlock { block_ix } => cache
          .block_item_ranges
          .get(*block_ix)
          .and_then(|range| (range.start < range.end).then_some((range.start, anchor.delta()))),
      },
      ScrollAnchorSnapshot::ParagraphRemainder {
        paragraph_ix,
        start_byte,
        delta,
      } => {
        let width = self.current_layout_width();
        let entry = self
          .valid_chunk_cache_entry(*paragraph_ix, width);
        if let Some(entry) = entry {
          let mut consumed = px(0.0);
          for (chunk_ix, chunk) in entry
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| chunk.end_byte > *start_byte)
          {
            if *delta <= consumed + chunk.height {
              let chunk_delta = (*delta - consumed)
                .max(px(0.0))
                .min(chunk.height.max(px(0.0)));
              return self
                .paragraph_chunk_item_ix(*paragraph_ix, chunk_ix)
                .map(|item_ix| (item_ix, chunk_delta));
            }
            consumed += chunk.height;
          }
          if let Some(item_ix) = cache
            .paragraph_remainder_items
            .get(*paragraph_ix)
            .copied()
            .and_then(decode_remainder_item_ix)
          {
            return Some((item_ix, (*delta - consumed).max(px(0.0))));
          }
          return entry.chunks.last().and_then(|chunk| {
            let chunk_ix = entry.chunks.len().checked_sub(1)?;
            let chunk_delta = (*delta - consumed)
              .max(px(0.0))
              .min(chunk.height.max(px(0.0)));
            self
              .paragraph_chunk_item_ix(*paragraph_ix, chunk_ix)
              .map(|item_ix| (item_ix, chunk_delta))
          });
        }
        cache
          .paragraph_remainder_items
          .get(*paragraph_ix)
          .copied()
          .and_then(decode_remainder_item_ix)
          .map(|item_ix| (item_ix, *delta))
      },
    }
  }

  fn paragraph_chunk_item_ix(&self, paragraph_ix: usize, chunk_ix: usize) -> Option<usize> {
    let cache = self.item_sizes_cache.as_ref()?;
    let range = cache.paragraph_chunk_item_ranges.get(paragraph_ix)?;
    let item_ix = range.start.checked_add(chunk_ix)?;
    (item_ix < range.end).then_some(item_ix)
  }

  fn paragraph_remainder_start_byte(&self, paragraph_ix: usize) -> usize {
    let width = self.current_layout_width();
    self
      .valid_chunk_cache_entry(paragraph_ix, width)
      .and_then(|entry| entry.chunks.last())
      .map(|chunk| chunk.end_byte)
      .unwrap_or(0)
  }

  fn paragraph_start_anchor(&self, paragraph_ix: usize) -> Option<ScrollAnchorSnapshot> {
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)?;
    Some(ScrollAnchorSnapshot::Item {
      item: VirtualItem::ParagraphChunk {
        block_ix,
        paragraph_ix,
        chunk_ix: 0,
      },
      delta: px(0.0),
    })
  }

}
