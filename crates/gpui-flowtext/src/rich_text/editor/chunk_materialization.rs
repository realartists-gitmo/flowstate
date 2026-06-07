#[hotpath::measure_all]
impl RichTextEditor {
  fn materialize_visible_remainders_for_scroll(
    &mut self,
    width: Pixels,
    scroll_anchor: Option<ScrollAnchorSnapshot>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> bool {
    let scroll_anchor = scroll_anchor.or_else(|| self.capture_scroll_anchor());
    let overscan = px(SCROLL_FOREGROUND_OVERSCAN_PX);
    let Some(visible_range) = self.visible_item_range_for_current_scroll(overscan) else {
      return false;
    };
    let Some(cache) = self.item_sizes_cache.as_ref() else {
      return false;
    };
    let viewport = self.scroll_handle.bounds();
    let scroll_bottom = (-self.scroll_handle.offset().y).max(px(0.0)) + viewport.size.height.max(px(700.0)) + overscan;
    let mut remainders = Vec::with_capacity(visible_range.len());
    for item_ix in visible_range {
      if let Some(VirtualItem::ParagraphRemainder { paragraph_ix, .. }) = cache.items.get(item_ix) {
        let row_top = self.height_prefix_index.item_top(item_ix);
        let target = (scroll_bottom - row_top).max(px(0.0));
        if target > px(0.0) {
          remainders.push((*paragraph_ix, self.paragraph_remainder_start_byte(*paragraph_ix), target));
        }
      }
    }
    if remainders.is_empty() {
      return false;
    }
    let missing_prep = remainders
      .iter()
      .filter_map(|(paragraph_ix, _, _)| self.valid_paragraph_prep(*paragraph_ix).is_none().then_some(*paragraph_ix))
      .collect::<Vec<_>>();
    if !missing_prep.is_empty() {
      self.request_layout_prep(width, missing_prep, cx);
    }

    let started = Instant::now();
    let budget = Duration::from_millis(SCROLL_FOREGROUND_MATERIALIZE_BUDGET_MS);
    let mut changed = false;
    for (paragraph_ix, start_byte, target) in remainders {
      changed |= self.materialize_paragraph_remainder_until(paragraph_ix, width, start_byte, target, started, budget, window, cx);
      if !DISABLE_SCROLL_LIMITING_FUNCTIONS && started.elapsed() >= budget {
        self.layout_runtime_metrics.scroll_budget_overruns = self.layout_runtime_metrics.scroll_budget_overruns.saturating_add(1);
        break;
      }
    }

    if changed {
      let _ = self.rebuild_item_sizes_cache(width, scroll_anchor, window, cx);
      cx.notify();
    }
    changed
  }

  fn visible_item_range_for_current_scroll(&self, overscan: Pixels) -> Option<Range<usize>> {
    let cache = self.item_sizes_cache.as_ref()?;
    if cache.item_count == 0 || self.height_prefix_index.len() != cache.item_count {
      return None;
    }
    let viewport = self.scroll_handle.bounds();
    let viewport_height = viewport.size.height.max(px(700.0));
    let scroll_top = (-self.scroll_handle.offset().y).max(px(0.0));
    let scroll_bottom = scroll_top + viewport_height + overscan;
    let start = self
      .height_prefix_index
      .lower_bound((scroll_top - overscan).max(px(0.0)))
      .min(cache.item_count);
    let end = (self.height_prefix_index.lower_bound(scroll_bottom) + 1).min(cache.item_count);
    Some(start..end.max(start + usize::from(start < cache.item_count)))
  }

  fn materialize_paragraph_remainder_until(
    &mut self,
    paragraph_ix: usize,
    width: Pixels,
    start_byte: usize,
    target: Pixels,
    started: Instant,
    budget: Duration,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> bool {
    let mut changed = false;
    loop {
      if !DISABLE_SCROLL_LIMITING_FUNCTIONS && started.elapsed() >= budget {
        break;
      }
      let (exact_after_start, complete) = self.paragraph_exact_height_after_byte(paragraph_ix, start_byte, width);
      if complete || exact_after_start >= target {
        break;
      }
      let target_lines = self.catch_up_chunk_target_lines(target - exact_after_start);
      let before = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .map(|entry| entry.chunks.len())
        .unwrap_or(0);
      if !self.ensure_next_paragraph_chunk_with_target_lines(paragraph_ix, width, target_lines, window, cx) {
        break;
      }
      let after = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .map(|entry| entry.chunks.len())
        .unwrap_or(before);
      if after == before {
        break;
      }
      changed = true;
    }
    changed
  }

  fn paragraph_exact_height_after_byte(&self, paragraph_ix: usize, start_byte: usize, width: Pixels) -> (Pixels, bool) {
    let Some(entry) = self
      .valid_chunk_cache_entry(paragraph_ix, width)
    else {
      return (px(0.0), false);
    };
    let mut start_ix = 0usize;
    let mut end_ix = entry.chunks.len();
    while start_ix < end_ix {
      let mid = start_ix + (end_ix - start_ix) / 2;
      if entry.chunks[mid].end_byte > start_byte {
        end_ix = mid;
      } else {
        start_ix = mid + 1;
      }
    }
    let mut height = px(0.0);
    for chunk in &entry.chunks[start_ix..] {
      height += chunk.height;
    }
    (height, entry.complete)
  }

  fn catch_up_chunk_target_lines(&self, remaining: Pixels) -> usize {
    let line_height =
      (self.document.theme.body_font_size * self.document.theme.zoom_factor.max(0.01) * self.document.theme.line_spacing * 1.35)
        .max(px(12.0));
    let remaining_px: f32 = remaining.into();
    let line_height_px: f32 = line_height.into();
    let approximate_lines = (remaining_px / line_height_px).ceil() as usize;
    approximate_lines.clamp(DEFAULT_PARAGRAPH_CHUNK_TARGET_LINES, SCROLL_FOREGROUND_MAX_CHUNK_LINES)
  }

}
