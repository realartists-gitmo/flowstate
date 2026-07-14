const LAYOUT_PREP_MAX_PARAGRAPHS_PER_BATCH: usize = 96;
const LAYOUT_PREP_MAX_TEXT_BYTES_PER_BATCH: usize = 512 * 1024;

#[hotpath::measure_all]
impl RichTextEditor {
  /// Resolve a paragraph index to its stable id, the key for the id-keyed
  /// prep/shaping caches (§perf-heaven T8.12).
  pub(super) fn paragraph_id_at(&self, paragraph_ix: usize) -> Option<ParagraphId> {
    self.document.ids.paragraph_ids.get(paragraph_ix).copied()
  }

  fn resize_layout_aux_caches(&mut self) {
    let paragraph_count = self.document.paragraphs.len();
    // §perf-heaven T8.12/T7.14: the prep, shaping, and estimate caches are all
    // id-keyed maps — no positional resize. Deleted paragraphs leave stale id
    // entries; bound each map so a long editing session cannot leak them without
    // limit. §act-ten A10.12: when the bound trips, RETAIN the live ids instead
    // of clearing wholesale — after a mass delete every SURVIVING paragraph
    // used to re-prep/re-shape/re-estimate for no reason.
    if self.paragraph_prep_cache.len() > paragraph_count * 2 + 64
      || self.paragraph_shaping_cache.len() > paragraph_count * 2 + 64
      || self.paragraph_estimate_height_cache.len() > paragraph_count * 2 + 64
    {
      let live: std::collections::HashSet<ParagraphId> = self.document.ids.paragraph_ids.iter().copied().collect();
      self.paragraph_prep_cache.retain(|id, _| live.contains(id));
      self.paragraph_shaping_cache.retain(|id, _| live.contains(id));
      self.paragraph_estimate_height_cache.retain(|id, _| live.contains(id));
    }
  }

  // §act-nine A9.3: validity is CONTENT-keyed — the (style, version) cache key
  // plus the invisibility mode plus the stable paragraph id — with NO global
  // `edit_generation`, so edits to OTHER paragraphs keep this prep. Soundness
  // rests on version discipline (every content change bumps the version;
  // structural rebuilds and canonical installs carry versions forward instead
  // of resetting to 0). §act-eleven A11.7: NO positional check either — a
  // structural shift (Enter/delete-row above) moves a paragraph without
  // changing its content, and (id, style, version) already pins exactly that
  // content; the prep's stored `paragraph_ix` is build-time context only
  // (position-dependent consumers take the CURRENT index as a parameter).
  pub(super) fn valid_paragraph_prep(&self, paragraph_ix: usize) -> Option<Arc<ParagraphPrep>> {
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let paragraph_id = self.paragraph_id_at(paragraph_ix)?;
    let expected_key = ParagraphPrepKey {
      paragraph_key: paragraph_cache_key(&self.document, paragraph),
      invisibility_mode: self.invisibility_mode,
    };
    self
      .paragraph_prep_cache
      .get(&paragraph_id)
      .and_then(|slot| slot.get(self.invisibility_mode))
      .filter(|prep| prep.paragraph_id == paragraph_id && prep.key == expected_key)
      .cloned()
  }

  fn paragraph_needs_layout_prep(&self, paragraph_ix: usize) -> bool {
    if self.invisibility_mode && !self.paragraph_materialized_in_current_mode(paragraph_ix) {
      return false;
    }
    self.valid_paragraph_prep(paragraph_ix).is_none()
  }

  pub(super) fn ensure_paragraph_prep_sync(&mut self, paragraph_ix: usize) -> Option<Arc<ParagraphPrep>> {
    if let Some(prep) = self.valid_paragraph_prep(paragraph_ix) {
      return Some(prep);
    }
    let prep = Arc::new(build_paragraph_prep(&self.document, paragraph_ix, self.invisibility_mode)?);
    self.resize_layout_aux_caches();
    self.paragraph_prep_cache.entry(prep.paragraph_id).or_default().set(prep.clone());
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
    if pending.invisibility_mode != request.invisibility_mode {
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
        invisibility_mode: request.invisibility_mode,
        paragraphs: overflow,
      });
    }
    let width = request.width;
    let batch = paragraph_prep_batch_request(
      &self.document,
      request.invisibility_mode,
      request.paragraphs,
      LAYOUT_PREP_MAX_PARAGRAPHS_PER_BATCH,
      LAYOUT_PREP_MAX_TEXT_BYTES_PER_BATCH,
    );
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

  pub(super) fn install_layout_prep_batch(&mut self, width: Pixels, result: ParagraphPrepBatchResult, cx: &mut Context<Self>) {
    self.resize_layout_aux_caches();
    self.layout_prep_metrics.batches = self.layout_prep_metrics.batches.saturating_add(1);
    self.layout_prep_metrics.requested = self.layout_prep_metrics.requested.saturating_add(result.requested);
    self.layout_prep_metrics.completed = self.layout_prep_metrics.completed.saturating_add(result.completed);
    self.layout_prep_metrics.text_bytes = self.layout_prep_metrics.text_bytes.saturating_add(result.text_bytes);

    if result.invisibility_mode == self.invisibility_mode {
      let deferred = result
        .deferred_paragraphs
        .iter()
        .copied()
        .filter(|paragraph_ix| self.paragraph_needs_layout_prep(*paragraph_ix))
        .collect::<Vec<_>>();
      if !deferred.is_empty() {
        self.merge_pending_layout_prep_request(LayoutPrepRequest {
          width,
          invisibility_mode: result.invisibility_mode,
          paragraphs: deferred,
        });
      }
    }

    let mut installed = 0usize;
    // §act-eleven A11.7: resolve each prep's CURRENT index by id (one map for
    // the whole batch) — the prep's build-time position may have shifted
    // structurally during the background build, and a shifted-but-unchanged
    // paragraph must still install (the id-keyed cache doesn't care where the
    // paragraph lives).
    let index_by_id: FxHashMap<ParagraphId, usize> = self
      .document
      .ids
      .paragraph_ids
      .iter()
      .enumerate()
      .map(|(current_ix, id)| (*id, current_ix))
      .collect();
    for prep in result.preps {
      // §act-nine A9.3 install gate: a completed background prep installs iff
      // its content is STILL CURRENT — same paragraph id (wherever it now
      // lives) with the same (style, version) content key and invisibility
      // mode. No `edit_generation` conjunct: an unrelated edit landing during
      // the batch must not discard prep for untouched paragraphs (the
      // background-prep thrash during typing bursts).
      let valid = prep.key.invisibility_mode == self.invisibility_mode
        && index_by_id.get(&prep.paragraph_id).is_some_and(|&current_ix| {
          self
            .document
            .paragraphs
            .get(current_ix)
            .is_some_and(|paragraph| paragraph_cache_key(&self.document, paragraph) == prep.key.paragraph_key)
        });
      if !valid {
        self.layout_prep_metrics.stale = self.layout_prep_metrics.stale.saturating_add(1);
        continue;
      }
      self.paragraph_prep_cache.entry(prep.paragraph_id).or_default().set(Arc::new(prep));
      installed += 1;
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
    self.paragraph_prep_cache.clear();
    self.pending_layout_prep_task = None;
    self.pending_layout_prep_request = None;
  }
  fn clear_layout_work_caches(&mut self) {
    self.layout_generation = self.layout_generation.wrapping_add(1);
    self.paragraph_shaping_cache.clear();
    self.layout_cache_retain_ranges = ParagraphCacheRetainRanges::default();
    self.prep_cache_retain_ranges = ParagraphCacheRetainRanges::default();
    self.pending_chunk_prefetch = false;
    self.chunk_prefetch_queue.clear();
  }
  pub(super) fn paragraph_work_key(&self, prep: &ParagraphPrep, width: Pixels) -> ParagraphLayoutWorkKey {
    ParagraphLayoutWorkKey {
      prep_key: prep.key,
      width,
      layout_generation: self.layout_generation,
    }
  }

  pub(super) fn take_paragraph_shape_cache(&mut self, paragraph_ix: usize, key: ParagraphLayoutWorkKey) -> FragmentShapeCache {
    self.resize_layout_aux_caches();
    match self.paragraph_id_at(paragraph_ix).and_then(|id| self.paragraph_shaping_cache.remove(&id)) {
      Some(entry) if entry.key == key => entry.fragment_shapes,
      _ => FragmentShapeCache::default(),
    }
  }

  pub(super) fn store_paragraph_shape_cache(&mut self, paragraph_ix: usize, key: ParagraphLayoutWorkKey, fragment_shapes: FragmentShapeCache) {
    self.resize_layout_aux_caches();
    if let Some(paragraph_id) = self.paragraph_id_at(paragraph_ix) {
      self.paragraph_shaping_cache.insert(paragraph_id, ParagraphShapingCacheEntry { key, fragment_shapes });
    }
  }
}
