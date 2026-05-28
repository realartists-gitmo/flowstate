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
    if RETAIN_OFFSCREEN_PARAGRAPH_LAYOUT_CACHE {
      return;
    }
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 || self.paragraph_chunk_layout_cache.is_empty() {
      return;
    }

    let visible = self.paragraph_range_for_item_range(item_range);
    if visible.is_empty() {
      return;
    }
    let active = self.active_height_range();
    let keep_start = visible.start.min(active.start).saturating_sub(2);
    let keep_end = visible
      .end
      .max(active.end)
      .saturating_add(2)
      .min(paragraph_count);

    self
      .paragraph_chunk_layout_cache
      .resize(paragraph_count, None);
    self
      .paragraph_shaping_cache
      .resize_with(paragraph_count, || None);
    for (paragraph_ix, entry) in self.paragraph_chunk_layout_cache.iter_mut().enumerate() {
      if paragraph_ix < keep_start || paragraph_ix >= keep_end {
        *entry = None;
        if let Some(shape_cache) = self.paragraph_shaping_cache.get_mut(paragraph_ix) {
          *shape_cache = None;
        }
      }
    }
    self
      .chunk_prefetch_queue
      .retain(|paragraph_ix| *paragraph_ix >= keep_start && *paragraph_ix < keep_end);
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

    self.recovery_write_in_progress = true;
    cx.spawn(async move |editor, cx| {
      Timer::after(Duration::from_millis(750)).await;
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
          } else if !editor.has_unsaved_changes() || editor.last_recovery_generation == editor.edit_generation {
            editor.recovery_write_in_progress = false;
            RecoveryWriteDecision::Idle
          } else {
            RecoveryWriteDecision::Write {
              generation: editor.edit_generation,
              document: Box::new(editor.document.clone()),
            }
          }
        })
        .ok();
      log_timing("recovery snapshot", snapshot_timing, "");
      let Some(RecoveryWriteDecision::Write { generation, document }) = decision else {
        return;
      };
      let write_timing = Instant::now();
      let paragraph_count = document.paragraphs.len();
      let write_result = cx
        .background_executor()
        .spawn(async move {
          let document = detach_document_for_background_write(&document);
          write_db8(path, &document)
        })
        .await;
      log_timing_lazy("recovery write", write_timing, || format!("paragraphs={paragraph_count}"));
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

}
