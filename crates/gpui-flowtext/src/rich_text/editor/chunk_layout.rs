#[hotpath::measure_all]
impl RichTextEditor {
  fn valid_chunk_cache_entry(&self, paragraph_ix: usize, width: Pixels) -> Option<&ParagraphChunkLayoutCacheEntry> {
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let key = paragraph_cache_key(&self.document, paragraph);
    self
      .paragraph_chunk_layout_cache
      .get(paragraph_ix)
      .and_then(|entry| entry.as_ref())
      .filter(|entry| {
        entry.key == key
          && entry.width == width
          && entry.invisibility_mode == self.invisibility_mode
          && entry.edit_generation == self.edit_generation
          && entry.layout_generation == self.layout_generation
      })
  }

  fn ensure_current_chunk_cache_entry(&mut self, paragraph_ix: usize, width: Pixels) -> bool {
    let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
      return false;
    };
    self
      .paragraph_chunk_layout_cache
      .resize(self.document.paragraphs.len(), None);
    let key = paragraph_cache_key(&self.document, paragraph);
    let reset = self
      .paragraph_chunk_layout_cache
      .get(paragraph_ix)
      .and_then(|entry| entry.as_ref())
      .is_none_or(|entry| {
        entry.key != key
          || entry.width != width
          || entry.invisibility_mode != self.invisibility_mode
          || entry.edit_generation != self.edit_generation
          || entry.layout_generation != self.layout_generation
      });
    if reset {
      let Some(prep) = self.ensure_paragraph_prep_sync(paragraph_ix) else {
        return false;
      };
      if !prep.visible {
        self.paragraph_chunk_layout_cache[paragraph_ix] = None;
        return false;
      }
      self.paragraph_chunk_layout_cache[paragraph_ix] = Some(ParagraphChunkLayoutCacheEntry {
        key,
        width,
        invisibility_mode: self.invisibility_mode,
        edit_generation: self.edit_generation,
        layout_generation: self.layout_generation,
        prep,
        chunks: Vec::new(),
        complete: false,
        exact_height: px(0.0),
      });
    }
    true
  }

  fn ensure_next_paragraph_chunk(&mut self, paragraph_ix: usize, width: Pixels, window: &mut Window, cx: &mut Context<Self>) -> bool {
    self.ensure_next_paragraph_chunk_with_target_lines(paragraph_ix, width, DEFAULT_PARAGRAPH_CHUNK_TARGET_LINES, window, cx)
  }

  fn ensure_next_paragraph_chunk_with_target_lines(
    &mut self,
    paragraph_ix: usize,
    width: Pixels,
    target_lines: usize,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> bool {
    self.ensure_next_paragraph_chunk_with_target_lines_internal(paragraph_ix, width, target_lines, true, window, cx)
  }

  fn ensure_next_paragraph_chunk_with_target_lines_internal(
    &mut self,
    paragraph_ix: usize,
    width: Pixels,
    target_lines: usize,
    bump_revision: bool,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> bool {
    if !self.ensure_current_chunk_cache_entry(paragraph_ix, width) {
      return false;
    }
    let (start_byte, already_complete, prep) = {
      let Some(entry) = self
        .paragraph_chunk_layout_cache
        .get(paragraph_ix)
        .and_then(|entry| entry.as_ref())
      else {
        return false;
      };
      (
        entry.chunks.last().map(|chunk| chunk.end_byte).unwrap_or(0),
        entry.complete,
        entry.prep.clone(),
      )
    };
    if already_complete {
      return true;
    }

    let work_key = self.paragraph_work_key(prep.as_ref(), width);
    let mut shape_cache = self.take_paragraph_shape_cache(paragraph_ix, work_key);
    let timing = Instant::now();
    let Some(result) = build_paragraph_chunk_layout_with_visibility(
      &self.document,
      paragraph_ix,
      width,
      start_byte,
      target_lines,
      self.invisibility_mode,
      Some(prep.as_ref()),
      &mut shape_cache,
      window,
      cx,
    ) else {
      self.store_paragraph_shape_cache(paragraph_ix, work_key, shape_cache);
      return false;
    };
    self.layout_runtime_metrics.ui_chunk_builds = self.layout_runtime_metrics.ui_chunk_builds.saturating_add(1);
    self.layout_runtime_metrics.ui_chunk_build_time += timing.elapsed();
    self.store_paragraph_shape_cache(paragraph_ix, work_key, shape_cache);
    let height = result.layout.size.height;
    let layout = Rc::new(result.layout);
    let Some(entry) = self
      .paragraph_chunk_layout_cache
      .get_mut(paragraph_ix)
      .and_then(|entry| entry.as_mut())
    else {
      return false;
    };
    if entry
      .chunks
      .last()
      .is_some_and(|chunk| chunk.end_byte == result.next_byte && result.next_byte == result.start_byte)
    {
      entry.complete = true;
      return true;
    }
    entry.exact_height += height;
    entry.chunks.push(ParagraphChunkLayout {
      start_byte: result.start_byte,
      end_byte: result.next_byte,
      height,
      layout,
    });
    entry.complete = result.complete;
    if entry.complete {
      self
        .paragraph_height_cache
        .resize(self.document.paragraphs.len(), None);
      self.paragraph_height_cache[paragraph_ix] = Some(ParagraphHeightCacheEntry {
        key: entry.key,
        width,
        invisibility_mode: self.invisibility_mode,
        edit_generation: self.edit_generation,
        height: entry.exact_height,
      });
    }
    if bump_revision {
      self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
      // Patch, don't nuke: only THIS paragraph's items changed (see
      // `note_item_sizes_patch_paragraph`).
      self.note_item_sizes_patch_paragraph(paragraph_ix);
    }
    true
  }

  fn ensure_paragraph_chunk(
    &mut self,
    paragraph_ix: usize,
    chunk_ix: usize,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> bool {
    loop {
      let ready = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .is_some_and(|entry| entry.chunks.get(chunk_ix).is_some() || entry.complete);
      if ready {
        return true;
      }
      if !self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx) {
        return false;
      }
    }
  }

  fn paragraph_chunk_layout_state(&self, paragraph_ix: usize, chunk_ix: usize, width: Pixels) -> Option<Rc<LayoutState>> {
    self
      .valid_chunk_cache_entry(paragraph_ix, width)
      .and_then(|entry| entry.chunks.get(chunk_ix))
      .map(|chunk| chunk.layout.clone())
  }

  pub(super) fn layout_paragraph_chunk_for_element(
    &mut self,
    paragraph_ix: usize,
    chunk_ix: usize,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<Rc<LayoutState>> {
    self.note_measured_item_width(width, cx);
    self.ensure_paragraph_chunk(paragraph_ix, chunk_ix, width, window, cx);
    self.paragraph_chunk_layout_state(paragraph_ix, chunk_ix, width)
  }

  pub(super) fn materialize_paragraph_remainder_for_render(
    &mut self,
    paragraph_ix: usize,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<usize> {
    self.note_measured_item_width(width, cx);
    let previous_chunk_count = self
      .valid_chunk_cache_entry(paragraph_ix, width)
      .map(|entry| entry.chunks.len())
      .unwrap_or(0);
    if self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx) {
      let after = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .map(|entry| entry.chunks.len())
        .unwrap_or(previous_chunk_count);
      if after != previous_chunk_count {
        // This can be called from VirtualList item construction during
        // prepaint. Do not rebuild item sizes or restore scroll here; the
        // current VirtualList pass is already using a local scroll offset and
        // old origins. The next render pass will rebuild from the new chunk.
        cx.notify();
      }
    }
    self
      .valid_chunk_cache_entry(paragraph_ix, width)
      .and_then(|entry| {
        if entry.chunks.len() > previous_chunk_count {
          Some(previous_chunk_count)
        } else {
          entry.chunks.len().checked_sub(1)
        }
      })
  }

}
