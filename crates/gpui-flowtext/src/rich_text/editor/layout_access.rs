#[hotpath::measure_all]
impl RichTextEditor {
  fn paragraph_visible_in_current_mode(&self, paragraph_ix: usize) -> bool {
    !self.invisibility_mode || self.paragraph_materialized_in_current_mode(paragraph_ix)
  }

  fn paragraph_materialized_in_current_mode(&self, paragraph_ix: usize) -> bool {
    let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
      return false;
    };
    paragraph_is_visible(&self.document, paragraph)
      || (self.invisibility_mode
        && self.selected_block.is_none()
        && self.selection.is_caret()
        && self.selection.head.paragraph == paragraph_ix
        && matches!(paragraph.style, ParagraphStyle::Normal))
  }

  fn schedule_viewport_size_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.disposed {
      self.pending_viewport_size_refresh = false;
      return;
    }
    if self.pending_viewport_size_refresh {
      return;
    }
    self.pending_viewport_size_refresh = true;
    cx.on_next_frame(window, |editor, _, cx| {
      if editor.disposed {
        editor.pending_viewport_size_refresh = false;
        return;
      }
      editor.pending_viewport_size_refresh = false;
      editor.item_sizes_cache = None;
      cx.notify();
    });
  }

  fn document_layout_for_offset(&self, offset: DocumentOffset, width: Pixels) -> Option<LayoutState> {
    let (chunk_ix, _chunk_layout) = self.paragraph_chunk_containing_byte(offset.paragraph, offset.byte, width)?;
    let viewport = self.scroll_handle.bounds();
    let entry = self
      .valid_chunk_cache_entry(offset.paragraph, width)?;
    let start = chunk_ix.saturating_sub(1);
    let end = (chunk_ix + 2).min(entry.chunks.len());
    let mut paragraphs = Vec::new();
    for ix in start..end {
      let chunk = &entry.chunks[ix];
      let mut paragraph = chunk.layout.paragraphs.first()?.clone();
      let row_top = self
        .item_top_for_paragraph_chunk(offset.paragraph, ix)
        .unwrap_or(px(0.0));
      paragraph.shift_y(viewport.top() + self.scroll_handle.offset().y + row_top + paragraph.top);
      paragraphs.push(paragraph);
    }
    if paragraphs.is_empty() {
      return None;
    }
    let mut paragraph_to_block = vec![usize::MAX; self.document.paragraphs.len()];
    let block_to_paragraph = paragraphs
      .iter()
      .enumerate()
      .map(|(layout_ix, paragraph)| {
        if paragraph.index < paragraph_to_block.len() && paragraph_to_block[paragraph.index] == usize::MAX {
          paragraph_to_block[paragraph.index] = layout_ix;
        }
        Some(paragraph.index)
      })
      .collect::<Vec<_>>();
    Some(LayoutState {
      blocks: paragraphs
        .iter()
        .cloned()
        .map(LaidOutBlock::Paragraph)
        .collect(),
      paragraph_to_block,
      block_to_paragraph,
      paragraphs,
      bounds: Some(Bounds::new(point(viewport.left(), px(0.0)), self.scroll_handle.content_size())),
      size: self.scroll_handle.content_size(),
      width,
      snap_underline_rules_to_pixels: self.document.theme.snap_underline_rules_to_pixels,
    })
  }

  fn layout_for_offset(&self, offset: DocumentOffset) -> Option<LayoutState> {
    let width = self.current_layout_width();
    self.document_layout_for_offset(offset, width)
  }

  fn hit_test_cached_position(&self, position: Point<Pixels>) -> Option<DocumentOffset> {
    let paragraph_count = self.document.paragraphs.len();
    let Some(cache) = &self.item_sizes_cache else {
      return None;
    };
    if paragraph_count == 0 || self.height_prefix_index.len() != cache.item_count {
      return None;
    }
    let viewport = self.scroll_handle.bounds();
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    let item_ix = self.height_prefix_index.lower_bound(content_y);
    match cache.items.get(item_ix) {
      Some(VirtualItem::ParagraphChunk { paragraph_ix, chunk_ix, .. }) => {
        let width = self.current_layout_width();
        let layout = self.paragraph_chunk_layout_state(*paragraph_ix, *chunk_ix, width)?;
        let row_top = self.height_prefix_index.item_top(item_ix);
        let bounds = Bounds::new(
          point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + row_top),
          size(width, layout.size.height),
        );
        // CT-S1: display→doc at the geometry boundary (invisibility remap).
        Some(self.doc_offset_from_display(layout.hit_test_at_bounds(position, bounds)))
      },
      Some(VirtualItem::ParagraphRemainder { paragraph_ix, .. }) => {
        let paragraph_len = self
          .document
          .paragraphs
          .get(*paragraph_ix)
          .map(paragraph_text_len)
          .unwrap_or(0);
        let start_byte = self
          .valid_chunk_cache_entry(*paragraph_ix, self.current_layout_width())
          .and_then(|entry| entry.chunks.last())
          .map(|chunk| chunk.end_byte)
          .unwrap_or(0);
        let row_top = self.height_prefix_index.item_top(item_ix);
        let row_height = cache
          .sizes
          .get(item_ix)
          .map(|size| size.height)
          .unwrap_or(px(1.0))
          .max(px(1.0));
        let ratio = ((content_y - row_top).max(px(0.0)) / row_height).clamp(0.0, 1.0);
        let byte = byte_at_ratio_in_paragraph(&self.document, *paragraph_ix, start_byte, paragraph_len, ratio);
        Some(DocumentOffset {
          paragraph: *paragraph_ix,
          byte,
        })
      },
      Some(VirtualItem::HiddenBlock { block_ix } | VirtualItem::StructuralBlock { block_ix }) => self
        .paragraph_ix_for_block(*block_ix)
        .map(|paragraph| DocumentOffset { paragraph, byte: 0 }),
      None => None,
    }
  }

  fn current_layout_width(&self) -> Pixels {
    if let Some(width) = self.measured_item_width {
      return width;
    }
    let viewport_width = self.scroll_handle.bounds().size.width;
    if viewport_width > px(1.0) { viewport_width } else { px(900.0) }
  }

  pub fn prepare_for_workspace_width_delta(&mut self, delta: Pixels, cx: &mut Context<Self>) {
    let width = (self.current_layout_width() + delta).max(px(1.0));
    self.note_measured_item_width(width, cx);
  }

  fn block_ix_for_paragraph(&self, target_paragraph_ix: usize) -> Option<usize> {
    super::block_ix_for_paragraph(&self.document, target_paragraph_ix)
  }

  fn item_top_for_paragraph_chunk(&self, paragraph_ix: usize, chunk_ix: usize) -> Option<Pixels> {
    let cache = self.item_sizes_cache.as_ref()?;
    if self.height_prefix_index.len() != cache.item_count {
      return None;
    }
    self
      .paragraph_chunk_item_ix(paragraph_ix, chunk_ix)
      .map(|item_ix| self.height_prefix_index.item_top(item_ix))
  }

  fn selection_for_object_block(&self, block_ix: usize) -> Option<BlockSelection> {
    match self.document.blocks.get(block_ix) {
      Some(Block::Image(_)) => Some(BlockSelection::Image(block_ix)),
      Some(Block::Equation(_)) => Some(BlockSelection::Equation(block_ix)),
      Some(Block::Table(_)) => Some(BlockSelection::Table(block_ix)),
      Some(Block::Paragraph(_)) | None => None,
    }
  }

  fn immediate_object_after_paragraph(&self, paragraph_ix: usize) -> Option<BlockSelection> {
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)? + 1;
    self.selection_for_object_block(block_ix)
  }

  fn immediate_object_before_paragraph(&self, paragraph_ix: usize) -> Option<BlockSelection> {
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)?.checked_sub(1)?;
    self.selection_for_object_block(block_ix)
  }

  fn paragraph_before_block(&self, target_block_ix: usize) -> Option<usize> {
    // §act-ten A10.8: O(log N) tree rank instead of the per-call block walk.
    self
      .document
      .blocks
      .paragraphs_before_row(target_block_ix)
      .checked_sub(1)
  }

  fn paragraph_after_block(&self, target_block_ix: usize) -> Option<usize> {
    // §act-ten A10.8: rank of the first paragraph past `target_block_ix`.
    let rank = self.document.blocks.paragraphs_before_row(target_block_ix.saturating_add(1));
    (rank < self.document.paragraphs.len()).then_some(rank)
  }

  fn collapse_object_selection(&mut self, dir: HDir, cx: &mut Context<Self>) -> bool {
    let Some(selection) = self.selected_block.take() else {
      return false;
    };
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    let offset = match dir {
      HDir::Left => self
        .paragraph_before_block(block_ix)
        .map(|paragraph| DocumentOffset {
          paragraph,
          byte: paragraph_text_len(&self.document.paragraphs[paragraph]),
        }),
      HDir::Right => self
        .paragraph_after_block(block_ix)
        .map(|paragraph| DocumentOffset { paragraph, byte: 0 }),
    };
    if let Some(offset) = offset {
      // §16 object boundary: collapsing leftward lands after the preceding
      // paragraph's content (After); rightward lands before the following
      // paragraph's content (Before).
      let affinity = match dir {
        HDir::Left => SelectionAffinity::After,
        HDir::Right => SelectionAffinity::Before,
      };
      self.note_explicit_selection_movement();
      self.selection = EditorSelection::collapsed_with(offset, affinity, VisualGravity::Neutral);
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    } else {
      self.selected_block = Some(selection);
    }
    true
  }

  fn paragraph_ix_for_block(&self, target_block_ix: usize) -> Option<usize> {
    // §act-ten A10.8: O(log N) tree rank (was a per-call full block walk —
    // O(visible items x blocks) per layout pass on object docs via
    // `paragraph_range_for_item_range`). Same semantics: `Some` iff the row is
    // a paragraph block.
    self.document.blocks.paragraph_ix_for_block_row(target_block_ix)
  }

  fn document_has_object_blocks(&self) -> bool {
    self.document.blocks.len() != self.document.paragraphs.len()
  }

  fn paragraph_range_for_item_range(&self, item_range: Range<usize>) -> Range<usize> {
    let Some(cache) = &self.item_sizes_cache else {
      return 0..0;
    };
    let mut first = None;
    let mut last = None;
    for item_ix in item_range {
      let paragraph_ix = match cache.items.get(item_ix) {
        Some(VirtualItem::ParagraphChunk { paragraph_ix, .. } | VirtualItem::ParagraphRemainder { paragraph_ix, .. }) => Some(*paragraph_ix),
        Some(VirtualItem::HiddenBlock { block_ix } | VirtualItem::StructuralBlock { block_ix }) => self.paragraph_ix_for_block(*block_ix),
        None => None,
      };
      if let Some(paragraph_ix) = paragraph_ix {
        first.get_or_insert(paragraph_ix);
        last = Some(paragraph_ix);
      }
    }
    match (first, last) {
      (Some(start), Some(end)) => start..end + 1,
      _ => 0..0,
    }
  }

  fn predicted_visible_height_range(&self, width: Pixels) -> Range<usize> {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return 0..0;
    }

    let viewport = self.scroll_handle.bounds();
    let viewport_height = if viewport.size.height > px(1.0) {
      viewport.size.height
    } else {
      px(1000.0)
    };
    let scroll_top = -self.scroll_handle.offset().y;
    let scroll_bottom = scroll_top + viewport_height + px(256.0);
    if let Some(cache) = &self.item_sizes_cache
      && self.height_prefix_index.len() == cache.item_count
    {
      let start_item = self
        .height_prefix_index
        .lower_bound((scroll_top - px(256.0)).max(px(0.0)));
      let end_item = (self.height_prefix_index.lower_bound(scroll_bottom) + 1).min(cache.item_count);
      let paragraph_range = self.paragraph_range_for_item_range(start_item..end_item.max(start_item + 1));
      if !paragraph_range.is_empty() {
        return expand_paragraph_range(paragraph_range, paragraph_count, 2);
      }
    }
    let mut y = px(0.0);
    let mut start = 0;
    let mut found_start = false;

    for paragraph_ix in 0..paragraph_count {
      if self.document.paragraphs.get(paragraph_ix).is_none() {
        break;
      }
      let height = self
        .valid_paragraph_height(paragraph_ix, width)
        .unwrap_or_else(|| {
          self
            .valid_paragraph_prep(paragraph_ix)
            .as_deref()
            .map(|prep| estimate_paragraph_prep_item_height(&self.document, prep, paragraph_ix, width))
            .unwrap_or_else(|| estimate_paragraph_item_height_with_visibility(&self.document, paragraph_ix, width, self.invisibility_mode))
        });
      let next_y = y + height;
      if !found_start && next_y >= scroll_top - px(256.0) {
        start = paragraph_ix;
        found_start = true;
      }
      if found_start && y > scroll_bottom {
        return expand_paragraph_range(start..paragraph_ix + 1, paragraph_count, 2);
      }
      y = next_y;
    }

    expand_paragraph_range(start..paragraph_count, paragraph_count, 2)
  }

  fn apply_pending_paragraph_snap(&mut self, cx: &mut Context<Self>) {
    let Some((paragraph_ix, remaining)) = self.pending_snap_to_paragraph else {
      return;
    };
    let Some(block_ix) = self.block_ix_for_paragraph(paragraph_ix) else {
      self.pending_snap_to_paragraph = None;
      return;
    };
    let Some(item_top) = self
      .item_top_for_paragraph_chunk(paragraph_ix, 0)
      .or_else(|| self.block_top_for_index(block_ix))
    else {
      self.pending_snap_to_paragraph = None;
      return;
    };

    let mut offset = self.scroll_handle.offset();
    offset.y = -item_top;
    self.scroll_handle.set_offset(offset);

    if remaining > 1 {
      self.pending_snap_to_paragraph = Some((paragraph_ix, remaining - 1));
      cx.notify();
    } else {
      self.pending_snap_to_paragraph = None;
    }
  }

  fn prepare_pending_head_scroll_after_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<bool> {
    if !self.pending_scroll_head_after_layout {
      return None;
    }
    self.pending_scroll_head_after_layout = false;
    let head = self.selection.head;
    if head.paragraph >= self.document.paragraphs.len() {
      return Some(false);
    }
    let width = self.current_layout_width();
    let before_revision = self.paragraph_height_cache_revision;
    let _ = self.ensure_paragraph_chunk_containing_byte(head.paragraph, head.byte, width, window, cx);
    Some(
      self.item_sizes_cache.is_none()
        || self
          .item_sizes_cache
          .as_ref()
          .is_some_and(|cache| cache.height_revision != self.paragraph_height_cache_revision || cache.width != width)
        || before_revision != self.paragraph_height_cache_revision,
    )
  }

  fn prepare_render_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> RenderLayoutSnapshot {
    // §hang-watchdog: time the whole render. The freeze shows as multi-second
    // gaps between render passes; timing the render tells us whether the stall is
    // INSIDE the render (this logs a `slow-render`) or between renders (collab /
    // projection apply — then this stays quiet and the collab path is the cause).
    // Logged always (not gated on `enabled()`), at WARN, so it survives even with
    // fidelity tracing off and can't be lost to a dropped-under-load JSONL buffer.
    let render_started = std::time::Instant::now();
    // §hang-probe: one line per render-layout pass capturing the virtualization
    // state that drives the scroll-materialization loop. In a freeze this fires
    // continuously; diffing consecutive lines shows WHICH field changes each
    // iteration (the invalidation source): a drifting `scroll_y` means the stall
    // guard's signature never settles (scroll-anchor restoration), a bumping
    // `layout_gen`/`height_rev` means a per-frame cache clear, etc.
    if flowstate_fidelity::enabled() {
      let scroll_px: f32 = (-self.scroll_handle.offset().y).max(px(0.0)).into();
      flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Structure, "render-layout-pass", || {
        format!(
          "scroll_y={scroll_px:.1} edit_gen={} layout_gen={} height_rev={} measured_w={:?} item_cache={} stall={}",
          self.edit_generation,
          self.layout_generation,
          self.paragraph_height_cache_revision,
          self.measured_item_width,
          self.item_sizes_cache.is_some(),
          self.scroll_materialize_stall_frames,
        )
      });
    }
    let hide_until_viewport_measured = self.scroll_handle.bounds().size.width <= px(1.0);
    let mut item_sizes = self.paragraph_item_sizes(window, cx);
    self.apply_pending_zoom_anchor();
    let has_startup_layout_width = self.measured_item_width.is_some() || self.document.paragraphs.is_empty();
    if !hide_until_viewport_measured && self.initial_layout_hidden && has_startup_layout_width {
      self.initial_layout_hidden = false;
    }

    self.apply_pending_paragraph_snap(cx);
    if let Some(needs_item_sizes) = self.prepare_pending_head_scroll_after_layout(window, cx) {
      if needs_item_sizes {
        item_sizes = self.paragraph_item_sizes(window, cx);
      }
      self.scroll_head_into_view();
    }

    let width = self.current_layout_width();
    if self.materialize_visible_remainders_for_scroll(width, None, window, cx)
      && let Some(cache) = &self.item_sizes_cache
    {
      item_sizes = cache.sizes.clone();
      self.apply_pending_zoom_anchor();
    }
    let base_items = self
      .item_sizes_cache
      .as_ref()
      .map(|cache| cache.items.clone())
      .unwrap_or_else(|| Rc::new(Vec::new()));
    let (items, item_sizes) = self.render_items_with_drop_preview(base_items, item_sizes, width, window, cx);
    let render_ms = render_started.elapsed().as_millis();
    if render_ms > 150 {
      let paragraphs = self.document.paragraphs.len();
      tracing::warn!("slow editor render pass (hang watchdog): {render_ms}ms, paragraphs={paragraphs}");
      flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Structure, "slow-render", || {
        format!("render took {render_ms}ms (paragraphs={paragraphs})")
      });
    }
    RenderLayoutSnapshot {
      width,
      item_sizes,
      items,
      hide_initial_layout: hide_until_viewport_measured || self.initial_layout_hidden,
    }
  }

  fn active_height_range(&self) -> Range<usize> {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return 0..0;
    }
    let active_paragraph = self.selection.head.paragraph.min(paragraph_count - 1);
    let start = active_paragraph.saturating_sub(1);
    let end = (active_paragraph + 2).min(paragraph_count).max(start + 1);
    start..end
  }

  pub(super) fn note_measured_item_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
    if self.measured_item_width == Some(width) {
      return;
    }
    self.measured_item_width = Some(width);
    self.clear_layout_work_caches();
    self.paragraph_chunk_layout_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.pending_item_sizes_patch_range = None;
    self.height_prefix_index = HeightPrefixIndex::default();
    cx.notify();
  }

}
