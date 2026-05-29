#[hotpath::measure_all]
impl RichTextEditor {
  fn paragraph_chunk_containing_byte(&self, paragraph_ix: usize, byte: usize, width: Pixels) -> Option<(usize, Rc<LayoutState>)> {
    let paragraph_len = self.document.paragraphs.get(paragraph_ix).map(paragraph_text_len)?;
    let entry = self.valid_chunk_cache_entry(paragraph_ix, width)?;
    let chunks = &entry.chunks;
    let mut low = 0usize;
    let mut high = chunks.len();
    while low < high {
      let mid = low + (high - low) / 2;
      let chunk = &chunks[mid];
      if byte < chunk.start_byte {
        high = mid;
      } else if byte < chunk.end_byte || (byte == chunk.end_byte && chunk.end_byte == paragraph_len) {
        return Some((mid, chunk.layout.clone()));
      } else {
        low = mid + 1;
      }
    }
    None
  }

  fn ensure_paragraph_chunk_containing_byte(
    &mut self,
    paragraph_ix: usize,
    byte: usize,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<usize> {
    loop {
      if let Some((chunk_ix, _)) = self.paragraph_chunk_containing_byte(paragraph_ix, byte, width) {
        return Some(chunk_ix);
      }
      let before_len = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .map(|entry| entry.chunks.len())
        .unwrap_or(0);
      if !self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx) {
        return None;
      }
      let after = self
        .valid_chunk_cache_entry(paragraph_ix, width)?;
      if after.complete && after.chunks.len() == before_len {
        return self
          .paragraph_chunk_containing_byte(paragraph_ix, byte, width)
          .map(|(chunk_ix, _)| chunk_ix);
      }
    }
  }

  fn ensure_vertical_navigation_chunks(&mut self, head: DocumentOffset, dir: VDir, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let Some(chunk_ix) = self.ensure_paragraph_chunk_containing_byte(head.paragraph, head.byte, width, window, cx) else {
      return;
    };
    match dir {
      VDir::Down => {
        let needs_next_chunk = self
          .valid_chunk_cache_entry(head.paragraph, width)
          .is_some_and(|entry| chunk_ix + 1 >= entry.chunks.len() && !entry.complete);
        if needs_next_chunk {
          self.ensure_next_paragraph_chunk(head.paragraph, width, window, cx);
        }
      },
      VDir::Up => {
        if chunk_ix == 0
          && let Some(prev) = head.paragraph.checked_sub(1)
        {
          self.ensure_next_paragraph_chunk(prev, width, window, cx);
        }
      },
    }
  }

  fn paragraph_remainder_estimate(&mut self, paragraph_ix: usize, width: Pixels) -> Pixels {
    let (estimated_total, text_len) = self
      .paragraph_estimated_total_height(paragraph_ix, width)
      .unwrap_or((self.document.theme.body_font_size * self.document.theme.line_spacing, 0));
    let exact_height = self.valid_chunk_cache_entry(paragraph_ix, width).map_or(px(0.0), |entry| entry.exact_height);
    let remaining = (estimated_total - exact_height).max(self.document.theme.body_font_size * self.document.theme.line_spacing);
    if text_len > 16 * 1024 || estimated_total > self.scroll_handle.bounds().size.height.max(px(700.0)) * 1.5 {
      remaining.max(self.scroll_handle.bounds().size.height.max(px(700.0)) + px(1024.0))
    } else {
      remaining
    }
  }

  fn paragraph_estimated_total_height(&mut self, paragraph_ix: usize, width: Pixels) -> Option<(Pixels, usize)> {
    self.resize_layout_aux_caches();
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let key = paragraph_cache_key(&self.document, paragraph);
    let expected = ParagraphEstimateHeightCacheEntry {
      key,
      width,
      invisibility_mode: self.invisibility_mode,
      edit_generation: self.edit_generation,
      layout_generation: self.layout_generation,
      height: px(0.0),
      source_len: 0,
    };
    if let Some(entry) = self.paragraph_estimate_height_cache.get(paragraph_ix).and_then(|entry| *entry)
      && entry.key == expected.key
      && entry.width == expected.width
      && entry.invisibility_mode == expected.invisibility_mode
      && entry.edit_generation == expected.edit_generation
      && entry.layout_generation == expected.layout_generation
    {
      return Some((entry.height, entry.source_len));
    }

    let prep = self.valid_paragraph_prep(paragraph_ix);
    let (height, source_len) = match prep.as_deref() {
      Some(prep) => (estimate_paragraph_prep_item_height(&self.document, prep, width), prep.source_len),
      None => (
        estimate_paragraph_item_height_with_visibility(&self.document, paragraph_ix, width, self.invisibility_mode),
        paragraph_text_len(paragraph),
      ),
    };
    if let Some(slot) = self.paragraph_estimate_height_cache.get_mut(paragraph_ix) {
      *slot = Some(ParagraphEstimateHeightCacheEntry {
        height,
        source_len,
        ..expected
      });
    }
    Some((height, source_len))
  }

  fn ensure_exact_interaction_chunks(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return;
    }

    let mut ranges = vec![self.predicted_visible_height_range(width), self.active_height_range()];
    if !self.visible_layout_range.is_empty() {
      let visible_paragraph_range = self.paragraph_range_for_item_range(self.visible_layout_range.clone());
      ranges.push(expand_paragraph_range(visible_paragraph_range, paragraph_count, 2));
    }

    let active = self.active_height_range();
    let visible = if self.visible_layout_range.is_empty() {
      0..0
    } else {
      self.paragraph_range_for_item_range(self.visible_layout_range.clone())
    };
    let candidate_capacity = ranges.iter().map(|range| range.len()).sum();
    let mut candidates = Vec::with_capacity(candidate_capacity);
    for range in ranges {
      candidates.extend(range);
    }
    candidates.sort_unstable();
    candidates.dedup();
    let mut prep_queue = Vec::new();
    for paragraph_ix in candidates {
      if paragraph_ix >= paragraph_count || !self.paragraph_visible_in_current_mode(paragraph_ix) {
        continue;
      }
      let urgent = active.contains(&paragraph_ix) || visible.contains(&paragraph_ix);
      if !urgent && self.valid_paragraph_prep(paragraph_ix).is_none() {
        prep_queue.push(paragraph_ix);
        continue;
      }
      self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx);
    }
    if !prep_queue.is_empty() {
      self.request_layout_prep(width, prep_queue, cx);
    }
  }

  fn ensure_exact_initial_viewport_chunks(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return;
    }

    let viewport_height = self.scroll_handle.bounds().size.height.max(px(700.0));
    let target_height = viewport_height + px(512.0);
    let mut accumulated = px(0.0);

    for paragraph_ix in 0..paragraph_count {
      if !self.paragraph_visible_in_current_mode(paragraph_ix) {
        continue;
      }
      loop {
        let before = self
          .valid_chunk_cache_entry(paragraph_ix, width)
          .map(|entry| entry.chunks.len())
          .unwrap_or(0);
        if !self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx) {
          break;
        }
        let Some(entry) = self
          .valid_chunk_cache_entry(paragraph_ix, width)
        else {
          break;
        };
        if let Some(chunk) = entry.chunks.get(before) {
          accumulated += chunk.height;
        }
        if accumulated >= target_height || entry.complete {
          break;
        }
      }
      if accumulated >= target_height {
        break;
      }
    }
  }

}
