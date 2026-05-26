impl RichTextEditor {
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
      .is_none_or(|entry| entry.key != key || entry.width != width || entry.invisibility_mode != self.invisibility_mode);
    if reset {
      let (paragraph_text, wrap_break_ends) = if self.invisibility_mode {
        (None, None)
      } else {
        let paragraph_text = Rc::<str>::from(paragraph_text(&self.document, paragraph_ix));
        let wrap_break_ends = Rc::new(wrap_break_ends(&paragraph_text));
        (Some(paragraph_text), Some(wrap_break_ends))
      };
      self.paragraph_chunk_layout_cache[paragraph_ix] = Some(ParagraphChunkLayoutCacheEntry {
        key,
        width,
        invisibility_mode: self.invisibility_mode,
        paragraph_text,
        wrap_break_ends,
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
    let (start_byte, already_complete, paragraph_text, wrap_break_ends) = {
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
        entry.paragraph_text.clone(),
        entry.wrap_break_ends.clone(),
      )
    };
    if already_complete {
      return true;
    }

    let Some(result) = build_paragraph_chunk_layout_with_visibility(
      &self.document,
      paragraph_ix,
      width,
      start_byte,
      target_lines,
      self.invisibility_mode,
      paragraph_text.as_deref(),
      wrap_break_ends.as_deref().map(Vec::as_slice),
      window,
      cx,
    ) else {
      return false;
    };
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
        height: entry.exact_height,
      });
    }
    if bump_revision {
      self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
      self.item_sizes_cache = None;
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
        .paragraph_chunk_layout_cache
        .get(paragraph_ix)
        .and_then(|entry| entry.as_ref())
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
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let key = paragraph_cache_key(&self.document, paragraph);
    self
      .paragraph_chunk_layout_cache
      .get(paragraph_ix)
      .and_then(|entry| entry.as_ref())
      .filter(|entry| entry.key == key && entry.width == width && entry.invisibility_mode == self.invisibility_mode)
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
    _paragraph_ix: usize,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<usize> {
    self.note_measured_item_width(width, cx);
    self.schedule_chunk_prefetch(width, window, cx);
    None
  }

}
