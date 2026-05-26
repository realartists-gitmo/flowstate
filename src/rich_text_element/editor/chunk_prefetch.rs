impl RichTextEditor {
  fn schedule_chunk_prefetch(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    if self.disposed {
      self.pending_chunk_prefetch = false;
      self.resume_chunk_prefetch_after_typing = false;
      self.resume_chunk_prefetch_after_scroll = false;
      self.chunk_prefetch_queue.clear();
      return;
    }
    if self.recently_typed() {
      self.resume_chunk_prefetch_after_typing = true;
      self.chunk_prefetch_queue.clear();
      self.schedule_typing_prefetch_resume(cx);
      return;
    }
    if self.recently_scrolled() {
      self.resume_chunk_prefetch_after_scroll = true;
      self.chunk_prefetch_queue.clear();
      self.schedule_scroll_prefetch_resume(cx);
      return;
    }
    if self.is_interacting() {
      self.chunk_prefetch_queue.clear();
      return;
    }
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      self.resume_chunk_prefetch_after_typing = false;
      return;
    }

    let mut queue = VecDeque::new();
    let mut queued = vec![false; paragraph_count];
    let active = self.active_height_range();
    let predicted = self.predicted_visible_height_range(width);
    for range in [
      expand_paragraph_range(predicted.clone(), paragraph_count, 4),
      expand_paragraph_range(active, paragraph_count, 2),
    ] {
      for paragraph_ix in range {
        if !queued[paragraph_ix] && self.paragraph_needs_chunk_prefetch(paragraph_ix, width) {
          queued[paragraph_ix] = true;
          queue.push_back(paragraph_ix);
        }
      }
    }
    if queue.is_empty() {
      self.resume_chunk_prefetch_after_typing = false;
      return;
    }
    self.resume_chunk_prefetch_after_typing = false;
    self.chunk_prefetch_queue = queue;
    if self.pending_chunk_prefetch {
      return;
    }
    self.pending_chunk_prefetch = true;
    cx.on_next_frame(window, move |editor, window, cx| {
      if editor.disposed {
        editor.pending_chunk_prefetch = false;
        editor.chunk_prefetch_queue.clear();
        return;
      }
      editor.pending_chunk_prefetch = false;
      editor.run_chunk_prefetch_budget(width, window, cx);
    });
  }

  fn run_chunk_prefetch_budget(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    if self.disposed {
      self.pending_chunk_prefetch = false;
      self.resume_chunk_prefetch_after_typing = false;
      self.resume_chunk_prefetch_after_scroll = false;
      self.chunk_prefetch_queue.clear();
      return;
    }
    if self.current_layout_width() != width {
      self.chunk_prefetch_queue.clear();
      return;
    }
    if self.recently_typed() {
      self.resume_chunk_prefetch_after_typing = true;
      self.chunk_prefetch_queue.clear();
      self.schedule_typing_prefetch_resume(cx);
      return;
    }
    if self.recently_scrolled() {
      self.resume_chunk_prefetch_after_scroll = true;
      self.chunk_prefetch_queue.clear();
      self.schedule_scroll_prefetch_resume(cx);
      return;
    }
    if self.is_interacting() {
      self.chunk_prefetch_queue.clear();
      return;
    }
    let start = Instant::now();
    let budget = Duration::from_millis(6);
    let scroll_anchor = self.capture_scroll_anchor();
    let mut changed = false;
    while let Some(paragraph_ix) = self.chunk_prefetch_queue.pop_front() {
      if !self.paragraph_needs_chunk_prefetch(paragraph_ix, width) {
        continue;
      }
      let before = self
        .paragraph_chunk_layout_cache
        .get(paragraph_ix)
        .and_then(|entry| entry.as_ref())
        .map(|entry| entry.chunks.len())
        .unwrap_or(0);
      if self.ensure_next_paragraph_chunk_with_target_lines_internal(
        paragraph_ix,
        width,
        DEFAULT_PARAGRAPH_CHUNK_TARGET_LINES,
        false,
        window,
        cx,
      ) {
        let after = self
          .paragraph_chunk_layout_cache
          .get(paragraph_ix)
          .and_then(|entry| entry.as_ref())
          .map(|entry| entry.chunks.len())
          .unwrap_or(before);
        changed |= after != before;
        if self.paragraph_needs_chunk_prefetch(paragraph_ix, width) {
          self.chunk_prefetch_queue.push_back(paragraph_ix);
        }
      }
      if start.elapsed() >= budget {
        break;
      }
    }
    if changed {
      self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
      self.item_sizes_cache = None;
      let _ = self.rebuild_item_sizes_cache_with_prefetch(width, scroll_anchor, false, window, cx);
      cx.notify();
    }
    if !self.chunk_prefetch_queue.is_empty() && !self.pending_chunk_prefetch {
      self.pending_chunk_prefetch = true;
      cx.on_next_frame(window, move |editor, window, cx| {
        if editor.disposed {
          editor.pending_chunk_prefetch = false;
          editor.chunk_prefetch_queue.clear();
          return;
        }
        editor.pending_chunk_prefetch = false;
        editor.run_chunk_prefetch_budget(width, window, cx);
      });
    }
  }

  fn paragraph_needs_chunk_prefetch(&self, paragraph_ix: usize, width: Pixels) -> bool {
    if !self.paragraph_visible_in_current_mode(paragraph_ix) {
      return false;
    }
    let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
      return false;
    };
    let key = paragraph_cache_key(&self.document, paragraph);
    self
      .paragraph_chunk_layout_cache
      .get(paragraph_ix)
      .and_then(|entry| entry.as_ref())
      .filter(|entry| entry.key == key && entry.width == width && entry.invisibility_mode == self.invisibility_mode)
      .is_none_or(|entry| !entry.complete)
  }

  fn maybe_resume_chunk_prefetch_after_typing(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    if !self.resume_chunk_prefetch_after_typing && !self.resume_chunk_prefetch_after_scroll {
      return;
    }
    if self.recently_typed() {
      self.schedule_typing_prefetch_resume(cx);
      return;
    }
    if self.recently_scrolled() {
      self.schedule_scroll_prefetch_resume(cx);
      return;
    }
    if self.is_interacting() {
      return;
    }
    self.schedule_chunk_prefetch(width, window, cx);
  }

  fn is_interacting(&self) -> bool {
    self.recently_typed()
      || self.recently_scrolled()
      || self.selecting
      || self.pending_text_drag.is_some()
      || self.active_text_drag.is_some()
      || self.image_resize_drag.is_some()
      || self.table_column_resize_drag.is_some()
      || self.autoscroll_active
  }

  fn mark_text_input_interaction(&mut self) {
    self.last_text_input_at = Some(Instant::now());
  }

  fn note_scroll_position_for_prefetch(&mut self, cx: &mut Context<Self>) {
    let scroll_y = self.scroll_handle.offset().y;
    if self
      .last_observed_scroll_y
      .is_some_and(|last_scroll_y| (last_scroll_y - scroll_y).abs() > px(0.5))
    {
      self.last_scroll_input_at = Some(Instant::now());
      self.resume_chunk_prefetch_after_scroll = true;
      self.chunk_prefetch_queue.clear();
      self.schedule_scroll_prefetch_resume(cx);
    }
    self.last_observed_scroll_y = Some(scroll_y);
  }

  fn recently_typed(&self) -> bool {
    self
      .last_text_input_at
      .is_some_and(|last_input| last_input.elapsed() < TYPING_PREFETCH_SUPPRESSION_WINDOW)
  }

  fn recently_scrolled(&self) -> bool {
    self
      .last_scroll_input_at
      .is_some_and(|last_input| last_input.elapsed() < SCROLL_PREFETCH_SUPPRESSION_WINDOW)
  }

  fn typing_prefetch_resume_delay(&self) -> Duration {
    self
      .last_text_input_at
      .and_then(|last_input| TYPING_PREFETCH_SUPPRESSION_WINDOW.checked_sub(last_input.elapsed()))
      .unwrap_or(Duration::ZERO)
  }

  fn scroll_prefetch_resume_delay(&self) -> Duration {
    self
      .last_scroll_input_at
      .and_then(|last_input| SCROLL_PREFETCH_SUPPRESSION_WINDOW.checked_sub(last_input.elapsed()))
      .unwrap_or(Duration::ZERO)
  }

  fn schedule_typing_prefetch_resume(&mut self, cx: &mut Context<Self>) {
    if self.disposed || self.pending_typing_prefetch_resume {
      return;
    }
    self.pending_typing_prefetch_resume = true;
    let delay = self.typing_prefetch_resume_delay();
    cx.spawn(async move |editor, cx| {
      Timer::after(delay).await;
      let _ = editor.update(cx, |editor, cx| {
        editor.pending_typing_prefetch_resume = false;
        if editor.disposed {
          return;
        }
        if editor.recently_typed() {
          editor.schedule_typing_prefetch_resume(cx);
        } else {
          editor.resume_chunk_prefetch_after_typing = true;
          cx.notify();
        }
      });
    })
    .detach();
  }

  fn schedule_scroll_prefetch_resume(&mut self, cx: &mut Context<Self>) {
    if self.disposed || self.pending_scroll_prefetch_resume {
      return;
    }
    self.pending_scroll_prefetch_resume = true;
    let delay = self.scroll_prefetch_resume_delay();
    cx.spawn(async move |editor, cx| {
      Timer::after(delay).await;
      let _ = editor.update(cx, |editor, cx| {
        editor.pending_scroll_prefetch_resume = false;
        if editor.disposed {
          return;
        }
        if editor.recently_scrolled() {
          editor.schedule_scroll_prefetch_resume(cx);
        } else {
          editor.resume_chunk_prefetch_after_scroll = true;
          cx.notify();
        }
      });
    })
    .detach();
  }

}
