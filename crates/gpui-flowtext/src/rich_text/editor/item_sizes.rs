type ParagraphRangeItemSizes = (Vec<VirtualItem>, Vec<Range<usize>>, Vec<Pixels>, Vec<Size<Pixels>>);
type FullItemSizes = (Rc<Vec<VirtualItem>>, Vec<Range<usize>>, Vec<Pixels>, Rc<Vec<Size<Pixels>>>);

#[hotpath::measure_all]
impl RichTextEditor {
  fn paragraph_item_sizes(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Rc<Vec<Size<Pixels>>> {
    self
      .paragraph_height_cache
      .resize(self.document.paragraphs.len(), None);
    self
      .paragraph_chunk_layout_cache
      .resize(self.document.paragraphs.len(), None);
    let viewport_width = self.scroll_handle.bounds().size.width;
    let has_measured_viewport = viewport_width > px(1.0);
    if !has_measured_viewport {
      self.schedule_viewport_size_refresh(window, cx);
    }
    let width = self
      .measured_item_width
      .unwrap_or(if has_measured_viewport { viewport_width } else { px(900.0) });
    self.prepare_pending_zoom_anchor(width, window, cx);
    if has_measured_viewport && self.initial_layout_hidden {
      self.ensure_exact_initial_viewport_chunks(width, window, cx);
    }
    if let Some(cache) = &self.item_sizes_cache
      && cache.width == width
      && cache.block_count == self.document.blocks.len()
      && cache.invisibility_mode == self.invisibility_mode
      && cache.height_revision == self.paragraph_height_cache_revision
      && self.height_prefix_index.len() == cache.item_count
    {
      let sizes = cache.sizes.clone();
      self.maybe_resume_chunk_prefetch_after_typing(width, window, cx);
      return sizes;
    }
    let scroll_anchor = self.capture_scroll_anchor();
    self.ensure_exact_interaction_chunks(width, window, cx);
    if let Some(cache) = &self.item_sizes_cache
      && cache.width == width
      && cache.block_count == self.document.blocks.len()
      && cache.invisibility_mode == self.invisibility_mode
      && cache.height_revision == self.paragraph_height_cache_revision
      && self.height_prefix_index.len() == cache.item_count
    {
      let sizes = cache.sizes.clone();
      self.maybe_resume_chunk_prefetch_after_typing(width, window, cx);
      return sizes;
    }
    if let Some(sizes) = self.try_patch_item_sizes_cache(width, scroll_anchor.clone(), window, cx) {
      return sizes;
    }
    self.rebuild_item_sizes_cache(width, scroll_anchor, window, cx)
  }

  fn rebuild_item_sizes_cache(
    &mut self,
    width: Pixels,
    scroll_anchor: Option<ScrollAnchorSnapshot>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Rc<Vec<Size<Pixels>>> {
    self.rebuild_item_sizes_cache_with_prefetch(width, scroll_anchor, true, window, cx)
  }

  fn rebuild_item_sizes_cache_with_prefetch(
    &mut self,
    width: Pixels,
    scroll_anchor: Option<ScrollAnchorSnapshot>,
    schedule_prefetch: bool,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Rc<Vec<Size<Pixels>>> {
    let zoom_anchor = self.active_zoom_anchor();
    let old_cache = self.item_sizes_cache.take();
    let (items, block_item_ranges, block_heights, sizes) = self.virtual_item_sizes(width, old_cache, window, cx);
    let (paragraph_chunk_item_ranges, paragraph_remainder_items) = item_lookup_for_virtual_items(items.as_ref(), self.document.paragraphs.len());
    self.height_prefix_index.rebuild(sizes.as_ref());
    let item_count = sizes.len();
    self.pending_item_sizes_patch_range = None;
    self.item_sizes_cache = Some(ItemSizesCache {
      width,
      block_count: self.document.blocks.len(),
      item_count,
      invisibility_mode: self.invisibility_mode,
      height_revision: self.paragraph_height_cache_revision,
      items,
      block_item_ranges,
      block_heights,
      paragraph_chunk_item_ranges,
      paragraph_remainder_items,
      sizes: sizes.clone(),
    });
    self.restore_scroll_anchor(scroll_anchor);
    self.restore_preserved_zoom_anchor(zoom_anchor);
    if schedule_prefetch {
      self.schedule_chunk_prefetch(width, window, cx);
    }
    sizes
  }

  fn try_patch_item_sizes_cache(
    &mut self,
    width: Pixels,
    scroll_anchor: Option<ScrollAnchorSnapshot>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<Rc<Vec<Size<Pixels>>>> {
    let zoom_anchor = self.active_zoom_anchor();
    let range = self.pending_item_sizes_patch_range.clone()?;
    let paragraph_count = self.document.paragraphs.len();
    if range.start > range.end || range.end > paragraph_count || self.document_has_object_blocks() {
      return None;
    }

    let cache = self.item_sizes_cache.as_ref()?;
    if cache.width != width
      || cache.block_count != self.document.blocks.len()
      || cache.invisibility_mode != self.invisibility_mode
      || cache.paragraph_chunk_item_ranges.len() != paragraph_count
      || cache.paragraph_remainder_items.len() != paragraph_count
      || cache.block_item_ranges.len() != self.document.blocks.len()
      || cache.block_heights.len() != self.document.blocks.len()
      || self.height_prefix_index.len() != cache.item_count
    {
      return None;
    }

    let replace_start = cache
      .block_item_ranges
      .get(range.start)
      .map_or(cache.item_count, |range| range.start);
    let replace_end = if range.end == 0 {
      0
    } else {
      cache.block_item_ranges.get(range.end - 1)?.end
    };
    if replace_start > replace_end || replace_end > cache.item_count {
      return None;
    }

    let (replacement_items, replacement_block_ranges, replacement_block_heights, replacement_sizes) =
      self.virtual_item_sizes_for_paragraph_range(range.clone(), width, window, cx)?;
    let old_len = replace_end - replace_start;
    let new_len = replacement_items.len();
    let item_delta = new_len as isize - old_len as isize;

    let patched_sizes = {
      let cache = self.item_sizes_cache.as_mut()?;
      let items = Rc::make_mut(&mut cache.items);
      let sizes = Rc::make_mut(&mut cache.sizes);
      items.splice(replace_start..replace_end, replacement_items);
      sizes.splice(replace_start..replace_end, replacement_sizes.clone());

      for block_ix in range.clone() {
        let relative = &replacement_block_ranges[block_ix - range.start];
        cache.block_item_ranges[block_ix] = replace_start + relative.start..replace_start + relative.end;
        cache.block_heights[block_ix] = replacement_block_heights[block_ix - range.start];
      }
      if item_delta != 0 {
        for block_range in cache.block_item_ranges.iter_mut().skip(range.end) {
          block_range.start = block_range.start.checked_add_signed(item_delta)?;
          block_range.end = block_range.end.checked_add_signed(item_delta)?;
        }
      }

      patch_item_lookup_for_paragraph_range(
        &mut cache.paragraph_chunk_item_ranges,
        &mut cache.paragraph_remainder_items,
        &items[..],
        replace_start,
        new_len,
        range.clone(),
        item_delta,
      )?;
      cache.item_count = sizes.len();
      cache.height_revision = self.paragraph_height_cache_revision;
      cache.sizes.clone()
    };
    if !self
      .height_prefix_index
      .replace_range(replace_start..replace_end, &replacement_sizes)
    {
      return None;
    }
    self.pending_item_sizes_patch_range = None;
    self.restore_scroll_anchor(scroll_anchor);
    self.restore_preserved_zoom_anchor(zoom_anchor);
    self.schedule_chunk_prefetch(width, window, cx);
    Some(patched_sizes)
  }

  fn virtual_item_sizes_for_paragraph_range(
    &mut self,
    range: Range<usize>,
    width: Pixels,
    _window: &mut Window,
    _cx: &mut Context<Self>,
  ) -> Option<ParagraphRangeItemSizes> {
    let mut items = Vec::with_capacity(range.len());
    let mut sizes = Vec::with_capacity(range.len());
    let mut block_item_ranges = Vec::with_capacity(range.len());
    let mut block_heights = Vec::with_capacity(range.len());

    for paragraph_ix in range {
      let paragraph = self.document.paragraphs.get(paragraph_ix)?;
      if !matches!(self.document.blocks.get(paragraph_ix), Some(Block::Paragraph(_))) {
        return None;
      }

      let block_start = items.len();
      let mut block_height = px(0.0);
      if self.paragraph_hidden_by_collapsed_section(paragraph_ix) {
        block_item_ranges.push(block_start..items.len());
        block_heights.push(px(0.0));
        continue;
      }
      if self.invisibility_mode && !paragraph_is_visible(&self.document, paragraph) {
        block_item_ranges.push(block_start..items.len());
        block_heights.push(px(0.0));
        continue;
      }

      let complete = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .map(|entry| {
          for (chunk_ix, chunk) in entry.chunks.iter().enumerate() {
            items.push(VirtualItem::ParagraphChunk {
              block_ix: paragraph_ix,
              paragraph_ix,
              chunk_ix,
            });
            sizes.push(size(width, chunk.height));
            block_height += chunk.height;
          }
          entry.complete
        })
        .unwrap_or(false);

      if !complete {
        let estimate = self.paragraph_remainder_estimate(paragraph_ix, width);
        items.push(VirtualItem::ParagraphRemainder {
          block_ix: paragraph_ix,
          paragraph_ix,
        });
        sizes.push(size(width, estimate));
        block_height += estimate;
      }

      block_item_ranges.push(block_start..items.len());
      block_heights.push(block_height);
    }

    Some((items, block_item_ranges, block_heights, sizes))
  }

  pub(crate) fn benchmark_paragraph_item_sizes(
    &mut self,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> ItemSizeBenchmarkResult {
    self.measured_item_width = Some(width);
    let cache_hit = self.item_sizes_cache.as_ref().is_some_and(|cache| {
      cache.width == width
        && cache.block_count == self.document.blocks.len()
        && cache.invisibility_mode == self.invisibility_mode
        && cache.height_revision == self.paragraph_height_cache_revision
    });
    let prep_before = self.layout_prep_metrics;
    let runtime_before = self.layout_runtime_metrics;
    let start = Instant::now();
    let sizes = self.paragraph_item_sizes(window, cx);
    let elapsed = start.elapsed();
    let prep_after = self.layout_prep_metrics;
    let runtime_after = self.layout_runtime_metrics;
    let exact_height_count = self
      .paragraph_chunk_layout_cache
      .iter()
      .enumerate()
      .filter_map(|(paragraph_ix, _)| self.valid_chunk_cache_entry(paragraph_ix, width))
      .map(|entry| entry.chunks.len())
      .sum();
    let total_height = sizes
      .iter()
      .map(|size| {
        let height: f32 = size.height.into();
        height
      })
      .sum();
    ItemSizeBenchmarkResult {
      elapsed,
      cache_hit,
      item_count: sizes.len(),
      exact_height_count,
      total_height,
      prep_requested: prep_after.requested.saturating_sub(prep_before.requested),
      prep_completed: prep_after.completed.saturating_sub(prep_before.completed),
      prep_installed: prep_after.installed.saturating_sub(prep_before.installed),
      prep_stale: prep_after.stale.saturating_sub(prep_before.stale),
      prep_batches: prep_after.batches.saturating_sub(prep_before.batches),
      prep_text_bytes: prep_after.text_bytes.saturating_sub(prep_before.text_bytes),
      ui_chunk_builds: runtime_after.ui_chunk_builds.saturating_sub(runtime_before.ui_chunk_builds),
      ui_chunk_build_time: runtime_after
        .ui_chunk_build_time
        .checked_sub(runtime_before.ui_chunk_build_time)
        .unwrap_or_default(),
      prefetch_budget_overruns: runtime_after
        .prefetch_budget_overruns
        .saturating_sub(runtime_before.prefetch_budget_overruns),
      scroll_budget_overruns: runtime_after
        .scroll_budget_overruns
        .saturating_sub(runtime_before.scroll_budget_overruns),
    }
  }

  pub(crate) fn benchmark_invalidate_document_layout_caches(&mut self) {
    self.invalidate_document_layout_caches();
  }

  fn virtual_item_sizes(
    &mut self,
    width: Pixels,
    old_cache: Option<ItemSizesCache>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> FullItemSizes {
    let block_count = self.document.blocks.len();
    let (mut items, mut block_item_ranges, mut block_heights, mut sizes) = reusable_virtual_item_buffers(old_cache, block_count);
    let mut paragraph_ix = 0usize;

    for block_ix in 0..block_count {
      let block_start = items.len();
      let mut block_height = px(0.0);

      match self.document.blocks.get(block_ix) {
        Some(Block::Paragraph(_paragraph)) => {
          let current_paragraph_ix = paragraph_ix;
          paragraph_ix += 1;
          if self.paragraph_hidden_by_collapsed_section(current_paragraph_ix) {
            block_item_ranges.push(block_start..items.len());
            block_heights.push(px(0.0));
            continue;
          }
          if self.invisibility_mode && !self.paragraph_materialized_in_current_mode(current_paragraph_ix) {
            block_item_ranges.push(block_start..items.len());
            block_heights.push(px(0.0));
            continue;
          }
          let complete = self
            .valid_chunk_cache_entry(current_paragraph_ix, width)
            .map(|entry| {
              for (chunk_ix, chunk) in entry.chunks.iter().enumerate() {
                items.push(VirtualItem::ParagraphChunk {
                  block_ix,
                  paragraph_ix: current_paragraph_ix,
                  chunk_ix,
                });
                sizes.push(size(width, chunk.height));
                block_height += chunk.height;
              }
              entry.complete
            })
            .unwrap_or(false);

          if !complete {
            let estimate = self.paragraph_remainder_estimate(current_paragraph_ix, width);
            items.push(VirtualItem::ParagraphRemainder {
              block_ix,
              paragraph_ix: current_paragraph_ix,
            });
            sizes.push(size(width, estimate));
            block_height += estimate;
          }
        },
        Some(Block::Image(_) | Block::Equation(_) | Block::Table(_)) => {
          if self.invisibility_mode {
            block_item_ranges.push(block_start..items.len());
            block_heights.push(px(0.0));
            continue;
          }
          let height = layout_structural_block_at(&self.document, block_ix, width, px(0.0), window, cx)
            .as_ref()
            .map(structural_block_height)
            .unwrap_or_else(|| estimate_structural_block_item_height(&self.document, block_ix, width))
            + self.document.theme.paragraph_after;
          items.push(VirtualItem::StructuralBlock { block_ix });
          sizes.push(size(width, height));
          block_height += height;
        },
        None => {
          items.push(VirtualItem::HiddenBlock { block_ix });
          sizes.push(size(width, px(0.0)));
        },
      }
      block_item_ranges.push(block_start..items.len());
      block_heights.push(block_height);
    }

    (Rc::new(items), block_item_ranges, block_heights, Rc::new(sizes))
  }

  fn paragraph_hidden_by_collapsed_section(&self, paragraph_ix: usize) -> bool {
    if self.collapsed_section_ids.is_empty() {
      return false;
    }
    self.document.sections.iter().any(|section| {
      if !self.collapsed_section_ids.contains(&section.id) {
        return false;
      }
      let Some(start) = paragraph_index_for_id(&self.document, section.start_paragraph) else {
        return false;
      };
      if paragraph_ix == start {
        return false;
      }
      let end = section
        .end_paragraph_exclusive
        .and_then(|id| paragraph_index_for_id(&self.document, id))
        .unwrap_or(self.document.paragraphs.len());
      start < paragraph_ix && paragraph_ix < end
    })
  }

}

#[hotpath::measure]
fn reusable_virtual_item_buffers(
  old_cache: Option<ItemSizesCache>,
  block_count: usize,
) -> ParagraphRangeItemSizes {
  let Some(cache) = old_cache else {
    return (
      Vec::with_capacity(block_count),
      Vec::with_capacity(block_count),
      Vec::with_capacity(block_count),
      Vec::with_capacity(block_count),
    );
  };

  let mut items = match Rc::try_unwrap(cache.items) {
    Ok(items) => items,
    Err(items) => Vec::with_capacity(items.len().max(block_count)),
  };
  let mut sizes = match Rc::try_unwrap(cache.sizes) {
    Ok(sizes) => sizes,
    Err(sizes) => Vec::with_capacity(sizes.len().max(block_count)),
  };
  let mut block_item_ranges = cache.block_item_ranges;
  let mut block_heights = cache.block_heights;

  items.clear();
  sizes.clear();
  block_item_ranges.clear();
  block_heights.clear();
  items.reserve(block_count);
  sizes.reserve(block_count);
  block_item_ranges.reserve(block_count);
  block_heights.reserve(block_count);

  (items, block_item_ranges, block_heights, sizes)
}
