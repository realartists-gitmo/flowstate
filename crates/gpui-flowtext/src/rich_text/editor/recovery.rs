#[hotpath::measure_all]
impl RichTextEditor {
  fn begin_visible_layout(&mut self, range: Range<usize>) -> u64 {
    if self.initial_layout_hidden
      && range.start == 0
      && range.end == 1
      && self.document.paragraphs.len() > 1
      && self.scroll_handle.bounds().size.height <= px(1.0)
    {
      // gpui-component's VirtualList measures item 0 in request_layout before
      // prepaint computes the real visible range. Do not let that measurement
      // pass stand in for the startup viewport, or the document can reveal
      // while most visible rows still use estimated heights.
      return self.visible_layout_generation;
    }

    self.visible_layout_generation = self.visible_layout_generation.wrapping_add(1);
    self.visible_layout_range = range.clone();
    self.visible_chunk_anchors.clear();
    self.evict_offscreen_paragraph_layouts_for_visible_items(range);
    self.visible_layout_generation
  }

  fn evict_offscreen_paragraph_layouts_for_visible_items(&mut self, item_range: Range<usize>) {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return;
    }

    let visible = self.paragraph_range_for_item_range(item_range);
    if visible.is_empty() {
      return;
    }
    let active = self.active_height_range();
    let required_ranges = ParagraphCacheRetainRanges {
      visible: expand_paragraph_range(visible.clone(), paragraph_count, 2),
      active: expand_paragraph_range(active.clone(), paragraph_count, 2),
    };
    if self.layout_cache_retain_ranges.covers(&required_ranges) && self.prep_cache_retain_ranges.covers(&required_ranges) {
      return;
    }

    // Retain viewport and caret neighborhoods independently. Bridging them
    // into one range can pin nearly the whole cache while scrolling far from
    // the active paragraph.
    let layout_keep_ranges = ParagraphCacheRetainRanges {
      visible: expand_paragraph_range(
        visible.clone(),
        paragraph_count,
        OFFSCREEN_LAYOUT_CACHE_OVERSCAN_PARAGRAPHS,
      ),
      active: expand_paragraph_range(
        active.clone(),
        paragraph_count,
        OFFSCREEN_LAYOUT_CACHE_OVERSCAN_PARAGRAPHS,
      ),
    };
    let prep_keep_ranges = ParagraphCacheRetainRanges {
      visible: expand_paragraph_range(
        visible,
        paragraph_count,
        OFFSCREEN_PREP_CACHE_OVERSCAN_PARAGRAPHS,
      ),
      active: expand_paragraph_range(
        active,
        paragraph_count,
        OFFSCREEN_PREP_CACHE_OVERSCAN_PARAGRAPHS,
      ),
    };

    self
      .paragraph_chunk_layout_cache
      .resize(paragraph_count, None);
    self
      .paragraph_shaping_cache
      .resize_with(paragraph_count, || None);
    self.paragraph_prep_cache.resize_with(paragraph_count, ParagraphPrepSlot::default);

    for paragraph_ix in 0..paragraph_count {
      if !layout_keep_ranges.contains(paragraph_ix) {
        let entry = &mut self.paragraph_chunk_layout_cache[paragraph_ix];
        *entry = None;
        if let Some(shape_cache) = self.paragraph_shaping_cache.get_mut(paragraph_ix) {
          *shape_cache = None;
        }
      }
      if !prep_keep_ranges.contains(paragraph_ix)
        && let Some(slot) = self.paragraph_prep_cache.get_mut(paragraph_ix)
      {
        slot.clear();
      }
    }
    self
      .chunk_prefetch_queue
      .retain(|paragraph_ix| layout_keep_ranges.contains(*paragraph_ix));
    self.layout_cache_retain_ranges = layout_keep_ranges;
    self.prep_cache_retain_ranges = prep_keep_ranges;
  }

  pub(super) fn store_visible_paragraph_chunk_layout(
    &mut self,
    generation: u64,
    item_ix: usize,
    chunk_ix: usize,
    layout: &LayoutState,
    bounds: Bounds<Pixels>,
  ) {
    if generation != self.visible_layout_generation || !self.visible_layout_range.contains(&item_ix) {
      return;
    }
    let Some(paragraph) = layout.paragraphs.first() else {
      return;
    };
    self.visible_chunk_anchors.push(VisibleChunkAnchor {
      paragraph_ix: paragraph.index,
      chunk_ix,
      bounds,
      scroll_y: self.scroll_handle.offset().y,
    });
  }

  fn refresh_save_status(&mut self) {
    if self
      .last_send_document_generation
      .is_some_and(|generation| self.saved_generation > generation)
    {
      self.last_send_document_generation = None;
    }
    if self
      .last_format_export_generation
      .is_some_and(|generation| self.saved_generation > generation)
    {
      self.last_format_export_generation = None;
    }
    self.save_status = if self.has_unsaved_changes() {
      SaveStatus::Dirty
    } else {
      SaveStatus::Saved
    };
  }

  fn schedule_recovery_write(&mut self, cx: &mut Context<Self>) {
    if self.disposed {
      self.recovery_write_in_progress = false;
      self.recovery_write_pending = false;
      return;
    }
    let Some(path) = self.recovery_path.clone() else {
      return;
    };
    let source_path = self.document_path.clone();
    if !self.has_unsaved_changes() {
      return;
    }
    if self.last_recovery_generation == self.edit_generation {
      return;
    }
    if self.recovery_write_in_progress {
      self.recovery_write_pending = true;
      return;
    }

    let delay = self.recovery_write_delay();
    self.recovery_write_in_progress = true;
    cx.spawn(async move |editor, cx| {
      Timer::after(delay).await;
      let snapshot_timing = Instant::now();
      let decision = editor
        .update(cx, |editor, cx| {
          if editor.disposed {
            editor.recovery_write_pending = false;
            editor.recovery_write_in_progress = false;
            RecoveryWriteDecision::Idle
          } else if editor.recovery_write_pending {
            editor.recovery_write_pending = false;
            editor.recovery_write_in_progress = false;
            editor.schedule_recovery_write(cx);
            RecoveryWriteDecision::Rescheduled
          } else if editor.last_text_input_at.is_some_and(|last_input| last_input.elapsed() < RECOVERY_TYPING_IDLE_WINDOW) {
            editor.recovery_write_in_progress = false;
            editor.schedule_recovery_write(cx);
            RecoveryWriteDecision::Rescheduled
          } else if !editor.has_unsaved_changes() || editor.last_recovery_generation == editor.edit_generation {
            editor.recovery_write_in_progress = false;
            RecoveryWriteDecision::Idle
          } else if let Some(hook) = editor.native_recovery_hook.clone() {
            RecoveryWriteDecision::WriteRuntime {
              generation: editor.edit_generation,
              hook,
            }
          } else {
            RecoveryWriteDecision::Write {
              generation: editor.edit_generation,
              document: Box::new(editor.document.clone()),
            }
          }
        })
        .ok();
      log_timing("recovery snapshot", snapshot_timing, "");
      let (generation, write_result, detail) = match decision {
        Some(RecoveryWriteDecision::Write { generation, document }) => {
          let write_timing = Instant::now();
          let paragraph_count = document.paragraphs.len();
          let result = cx
            .background_executor()
            .spawn(async move {
              let document = detach_document_for_background_write(&document);
              write_recovery_snapshot(&path, source_path.as_deref(), &document)
            })
            .await;
          log_timing_lazy("recovery write", write_timing, || format!("paragraphs={paragraph_count}"));
          (generation, result, format!("paragraphs={paragraph_count}"))
        },
        Some(RecoveryWriteDecision::WriteRuntime { generation, hook }) => {
          let write_timing = Instant::now();
          let result = hook(path).await;
          log_timing("runtime recovery write", write_timing, "");
          (generation, result, "runtime".to_string())
        },
        _ => return,
      };
      let _ = detail;
      match write_result {
        Ok(()) => {
          let _ = editor.update(cx, |editor, _| {
            editor.last_recovery_generation = editor.last_recovery_generation.max(generation);
          });
        },
        Err(error) => {
          eprintln!("failed to write recovery file: {error}");
        },
      }
      let _ = editor.update(cx, |editor, cx| {
        if editor.disposed {
          editor.recovery_write_pending = false;
          editor.recovery_write_in_progress = false;
          return;
        }
        editor.recovery_write_in_progress = false;
        if editor.recovery_write_pending {
          editor.schedule_recovery_write(cx);
        }
      });
    })
    .detach();
  }

  fn recovery_write_delay(&self) -> Duration {
    self
      .last_text_input_at
      .and_then(|last_input| RECOVERY_TYPING_IDLE_WINDOW.checked_sub(last_input.elapsed()))
      .filter(|delay| *delay > RECOVERY_WRITE_DEBOUNCE)
      .unwrap_or(RECOVERY_WRITE_DEBOUNCE)
  }

}
