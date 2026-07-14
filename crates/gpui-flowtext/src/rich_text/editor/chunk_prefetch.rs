#[hotpath::measure_all]
impl RichTextEditor {
  fn schedule_chunk_prefetch(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    if self.disposed {
      self.pending_chunk_prefetch = false;
      self.resume_chunk_prefetch_after_typing = false;
      self.chunk_prefetch_queue.clear();
      return;
    }
    if self.recently_typed() {
      self.resume_chunk_prefetch_after_typing = true;
      self.chunk_prefetch_queue.clear();
      self.schedule_typing_prefetch_resume(cx);
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
    let mut prep_queue = Vec::new();
    let active = self.active_height_range();
    let predicted = self.predicted_visible_height_range(width);
    let mut candidates = Vec::with_capacity(predicted.len().saturating_add(active.len()).saturating_add(16));
    for range in [
      expand_paragraph_range(predicted.clone(), paragraph_count, 4),
      expand_paragraph_range(active, paragraph_count, 2),
    ] {
      candidates.extend(range);
    }
    candidates.sort_unstable();
    candidates.dedup();
    for paragraph_ix in candidates {
      if paragraph_ix < paragraph_count && self.paragraph_needs_chunk_prefetch(paragraph_ix, width) {
        if self.valid_paragraph_prep(paragraph_ix).is_some() {
          queue.push_back(paragraph_ix);
        } else {
          prep_queue.push(paragraph_ix);
        }
      }
    }
    if !prep_queue.is_empty() {
      self.request_layout_prep(width, prep_queue, cx);
    }
    if queue.is_empty() {
      if self.pending_layout_prep_task.is_none() {
        self.resume_chunk_prefetch_after_typing = false;
      }
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
    if self.is_interacting() {
      self.chunk_prefetch_queue.clear();
      return;
    }
    let start = Instant::now();
    let budget = Duration::from_millis(6);
    let scroll_anchor = self.capture_scroll_anchor();
    let mut changed = false;
    let mut missing_prep = Vec::new();
    while let Some(paragraph_ix) = self.chunk_prefetch_queue.pop_front() {
      if !self.paragraph_needs_chunk_prefetch(paragraph_ix, width) {
        continue;
      }
      if self.valid_paragraph_prep(paragraph_ix).is_none() {
        missing_prep.push(paragraph_ix);
        continue;
      }
      let before = self
        .valid_chunk_cache_entry(paragraph_ix, width)
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
          .valid_chunk_cache_entry(paragraph_ix, width)
          .map(|entry| entry.chunks.len())
          .unwrap_or(before);
        let progressed = after != before;
        changed |= progressed;
        if progressed {
          self.note_item_sizes_patch_paragraph(paragraph_ix);
        }
        if self.paragraph_needs_chunk_prefetch(paragraph_ix, width) {
          if progressed {
            self.chunk_prefetch_queue.push_back(paragraph_ix);
          } else {
            // The paragraph still reports "needs prefetch" but ensuring the next
            // chunk added NOTHING (`after == before`) — the chunker cannot advance
            // it to `complete`. Re-queuing it here would keep the queue non-empty,
            // which reschedules the `on_next_frame` prefetch below every frame:
            // a continuous re-render with no state change that FREEZES the window
            // (observed on large documents with object blocks). Drop it instead so
            // the queue can drain; the paragraph renders with the chunks it has.
            flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Structure, "prefetch-no-progress", || {
              format!("paragraph {paragraph_ix} needs prefetch but chunking made no progress ({before} chunks); dropped to break the render loop")
            });
          }
        }
      }
      if start.elapsed() >= budget {
        self.layout_runtime_metrics.prefetch_budget_overruns = self.layout_runtime_metrics.prefetch_budget_overruns.saturating_add(1);
        break;
      }
    }
    if !missing_prep.is_empty() {
      self.request_layout_prep(width, missing_prep, cx);
    }
    if changed {
      self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
      // Patch, don't nuke: the loop above recorded every refined paragraph in
      // `pending_item_sizes_patch_range` (see `note_item_sizes_patch_paragraph`);
      // a full O(blocks) rebuild only runs when the patch is inapplicable.
      if self.try_patch_item_sizes_cache(width, scroll_anchor.clone(), window, cx).is_none() {
        self.item_sizes_cache = None;
        let _ = self.rebuild_item_sizes_cache_with_prefetch(width, scroll_anchor, false, window, cx);
      }
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
    if self.document.paragraphs.get(paragraph_ix).is_none() {
      return false;
    }
    self
      .valid_chunk_cache_entry(paragraph_ix, width)
      .is_none_or(|entry| !entry.complete)
  }

  fn maybe_resume_chunk_prefetch_after_typing(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    if !self.resume_chunk_prefetch_after_typing {
      return;
    }
    if self.recently_typed() {
      self.schedule_typing_prefetch_resume(cx);
      return;
    }
    if self.is_interacting() {
      return;
    }
    self.schedule_chunk_prefetch(width, window, cx);
  }

  fn is_interacting(&self) -> bool {
    self.recently_typed()
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

  fn recently_typed(&self) -> bool {
    self
      .last_text_input_at
      .is_some_and(|last_input| last_input.elapsed() < TYPING_PREFETCH_SUPPRESSION_WINDOW)
  }

  fn typing_prefetch_resume_delay(&self) -> Duration {
    self
      .last_text_input_at
      .and_then(|last_input| TYPING_PREFETCH_SUPPRESSION_WINDOW.checked_sub(last_input.elapsed()))
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

}
