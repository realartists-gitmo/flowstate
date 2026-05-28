const LAYOUT_PREP_MAX_PARAGRAPHS_PER_BATCH: usize = 96;
const LAYOUT_PREP_MAX_TEXT_BYTES_PER_BATCH: usize = 512 * 1024;

#[hotpath::measure_all]
impl RichTextEditor {
  fn resize_layout_aux_caches(&mut self) {
    let paragraph_count = self.document.paragraphs.len();
    self.paragraph_prep_cache.resize_with(paragraph_count, ParagraphPrepSlot::default);
    self
      .paragraph_shaping_cache
      .resize_with(paragraph_count, || None);
  }

  fn valid_paragraph_prep(&self, paragraph_ix: usize) -> Option<Arc<ParagraphPrep>> {
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let expected_key = ParagraphPrepKey {
      paragraph_key: paragraph_cache_key(&self.document, paragraph),
      invisibility_mode: self.invisibility_mode,
      edit_generation: self.edit_generation,
    };
    self
      .paragraph_prep_cache
      .get(paragraph_ix)
      .and_then(|slot| slot.get(self.invisibility_mode))
      .filter(|prep| prep.paragraph_ix == paragraph_ix && prep.key == expected_key)
      .cloned()
  }

  fn paragraph_needs_layout_prep(&self, paragraph_ix: usize) -> bool {
    if self.invisibility_mode
      && self
        .document
        .paragraphs
        .get(paragraph_ix)
        .is_some_and(|paragraph| !paragraph_is_visible(paragraph))
    {
      return false;
    }
    self.valid_paragraph_prep(paragraph_ix).is_none()
  }

  fn ensure_paragraph_prep_sync(&mut self, paragraph_ix: usize) -> Option<Arc<ParagraphPrep>> {
    if let Some(prep) = self.valid_paragraph_prep(paragraph_ix) {
      return Some(prep);
    }
    let prep = Arc::new(build_paragraph_prep(
      &self.document,
      paragraph_ix,
      self.edit_generation,
      self.invisibility_mode,
    )?);
    self.resize_layout_aux_caches();
    if let Some(slot) = self.paragraph_prep_cache.get_mut(paragraph_ix) {
      slot.set(prep.clone());
    }
    Some(prep)
  }

  fn request_layout_prep(&mut self, width: Pixels, mut paragraphs: Vec<usize>, cx: &mut Context<Self>) {
    if self.disposed || paragraphs.is_empty() {
      return;
    }
    paragraphs.retain(|paragraph_ix| {
      *paragraph_ix < self.document.paragraphs.len() && self.paragraph_needs_layout_prep(*paragraph_ix)
    });
    paragraphs.sort_unstable();
    paragraphs.dedup();
    if paragraphs.is_empty() {
      return;
    }
    let request = LayoutPrepRequest {
      width,
      edit_generation: self.edit_generation,
      invisibility_mode: self.invisibility_mode,
      paragraphs,
    };
    if self.pending_layout_prep_task.is_some() {
      self.merge_pending_layout_prep_request(request);
      return;
    }
    self.start_layout_prep_task(request, cx);
  }

  fn merge_pending_layout_prep_request(&mut self, request: LayoutPrepRequest) {
    let Some(pending) = self.pending_layout_prep_request.as_mut() else {
      self.pending_layout_prep_request = Some(request);
      return;
    };
    if pending.edit_generation != request.edit_generation || pending.invisibility_mode != request.invisibility_mode {
      *pending = request;
      return;
    }
    pending.width = request.width;
    pending.paragraphs.extend(request.paragraphs);
    pending.paragraphs.sort_unstable();
    pending.paragraphs.dedup();
  }

  fn start_layout_prep_task(&mut self, request: LayoutPrepRequest, cx: &mut Context<Self>) {
    let mut request = request;
    let overflow = if request.paragraphs.len() > LAYOUT_PREP_MAX_PARAGRAPHS_PER_BATCH {
      request.paragraphs.split_off(LAYOUT_PREP_MAX_PARAGRAPHS_PER_BATCH)
    } else {
      Vec::new()
    };
    if !overflow.is_empty() {
      self.merge_pending_layout_prep_request(LayoutPrepRequest {
        width: request.width,
        edit_generation: request.edit_generation,
        invisibility_mode: request.invisibility_mode,
        paragraphs: overflow,
      });
    }
    let width = request.width;
    let batch = ParagraphPrepBatchRequest {
      document: detach_document_for_background_write(&self.document),
      edit_generation: request.edit_generation,
      invisibility_mode: request.invisibility_mode,
      paragraphs: request.paragraphs,
      max_paragraphs: LAYOUT_PREP_MAX_PARAGRAPHS_PER_BATCH,
      max_text_bytes: LAYOUT_PREP_MAX_TEXT_BYTES_PER_BATCH,
    };
    self.pending_layout_prep_task = Some(
      cx.spawn(async move |editor, cx| {
        let timing = Instant::now();
        let result = cx
          .background_executor()
          .spawn(async move { build_paragraph_prep_batch(batch) })
          .await;
        log_timing_lazy("layout prep batch", timing, || {
          format!(
            "requested={} completed={} bytes={}",
            result.requested, result.completed, result.text_bytes
          )
        });
        let _ = editor.update(cx, |editor, cx| {
          editor.pending_layout_prep_task = None;
          editor.install_layout_prep_batch(width, result, cx);
          if let Some(next_request) = editor.pending_layout_prep_request.take() {
            editor.start_layout_prep_task(next_request, cx);
          }
        });
      }),
    );
  }

  fn install_layout_prep_batch(&mut self, width: Pixels, result: ParagraphPrepBatchResult, cx: &mut Context<Self>) {
    self.resize_layout_aux_caches();
    self.layout_prep_metrics.batches = self.layout_prep_metrics.batches.saturating_add(1);
    self.layout_prep_metrics.requested = self.layout_prep_metrics.requested.saturating_add(result.requested);
    self.layout_prep_metrics.completed = self.layout_prep_metrics.completed.saturating_add(result.completed);
    self.layout_prep_metrics.text_bytes = self.layout_prep_metrics.text_bytes.saturating_add(result.text_bytes);

    if result.edit_generation == self.edit_generation && result.invisibility_mode == self.invisibility_mode {
      let deferred = result
        .deferred_paragraphs
        .iter()
        .copied()
        .filter(|paragraph_ix| self.paragraph_needs_layout_prep(*paragraph_ix))
        .collect::<Vec<_>>();
      if !deferred.is_empty() {
        self.merge_pending_layout_prep_request(LayoutPrepRequest {
          width,
          edit_generation: result.edit_generation,
          invisibility_mode: result.invisibility_mode,
          paragraphs: deferred,
        });
      }
    }

    let mut installed = 0usize;
    for prep in result.preps {
      let paragraph_ix = prep.paragraph_ix;
      let valid = result.edit_generation == self.edit_generation
        && result.invisibility_mode == self.invisibility_mode
        && prep.key.edit_generation == self.edit_generation
        && prep.key.invisibility_mode == self.invisibility_mode
        && self
          .document
          .paragraphs
          .get(paragraph_ix)
          .is_some_and(|paragraph| paragraph_cache_key(&self.document, paragraph) == prep.key.paragraph_key);
      if !valid {
        self.layout_prep_metrics.stale = self.layout_prep_metrics.stale.saturating_add(1);
        continue;
      }
      if let Some(slot) = self.paragraph_prep_cache.get_mut(paragraph_ix) {
        slot.set(Arc::new(prep));
        installed += 1;
      }
    }
    if installed == 0 {
      return;
    }
    self.layout_prep_metrics.installed = self.layout_prep_metrics.installed.saturating_add(installed);
    if self.current_layout_width() == width {
      self.resume_chunk_prefetch_after_typing = true;
    }
    cx.notify();
  }

  fn clear_all_layout_prep(&mut self) {
    for slot in &mut self.paragraph_prep_cache {
      slot.clear();
    }
    self.pending_layout_prep_task = None;
    self.pending_layout_prep_request = None;
  }

  fn clear_layout_prep_range(&mut self, range: Range<usize>) {
    self.resize_layout_aux_caches();
    for paragraph_ix in range {
      if let Some(slot) = self.paragraph_prep_cache.get_mut(paragraph_ix) {
        slot.clear();
      }
    }
    self.pending_layout_prep_request = None;
  }

  fn clear_layout_work_caches(&mut self) {
    self.layout_generation = self.layout_generation.wrapping_add(1);
    self.paragraph_shaping_cache.clear();
    self.paragraph_shaping_cache.resize_with(self.document.paragraphs.len(), || None);
    self.pending_chunk_prefetch = false;
    self.chunk_prefetch_queue.clear();
  }

  fn clear_layout_work_cache_range(&mut self, range: Range<usize>) {
    self.resize_layout_aux_caches();
    for paragraph_ix in range {
      if let Some(cache) = self.paragraph_shaping_cache.get_mut(paragraph_ix) {
        *cache = None;
      }
    }
    self.pending_chunk_prefetch = false;
    self.chunk_prefetch_queue.clear();
  }

  fn paragraph_work_key(&self, prep: &ParagraphPrep, width: Pixels) -> ParagraphLayoutWorkKey {
    ParagraphLayoutWorkKey {
      prep_key: prep.key,
      width,
      layout_generation: self.layout_generation,
    }
  }

  fn take_paragraph_shape_cache(&mut self, paragraph_ix: usize, key: ParagraphLayoutWorkKey) -> FragmentShapeCache {
    self.resize_layout_aux_caches();
    match self.paragraph_shaping_cache.get_mut(paragraph_ix).and_then(Option::take) {
      Some(entry) if entry.key == key => entry.fragment_shapes,
      _ => FragmentShapeCache::default(),
    }
  }

  fn store_paragraph_shape_cache(&mut self, paragraph_ix: usize, key: ParagraphLayoutWorkKey, fragment_shapes: FragmentShapeCache) {
    self.resize_layout_aux_caches();
    if let Some(slot) = self.paragraph_shaping_cache.get_mut(paragraph_ix) {
      *slot = Some(ParagraphShapingCacheEntry { key, fragment_shapes });
    }
  }
}
