// Projection snapshots carry semantic style slots; their appearance catalog is editor-local.
fn projection_with_local_theme(mut document: DocumentProjection, theme: &DocumentTheme) -> DocumentProjection {
  document.theme = theme.clone();
  document
}

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn clear_document_equation_caches(&self) {
    let keys = self.document.blocks.iter().filter_map(|block| match block {
      Block::Equation(equation) => Some((equation.source.clone(), matches!(equation.display, EquationDisplay::Display))),
      _ => None,
    });
    EquationRenderer::clear_entries(keys);
  }

  pub fn new_with_path(mut document: DocumentProjection, document_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
    rebuild_document_sections(&mut document);
    let paragraph_count = document.paragraphs.len();
    let saved_generation = if document_path.is_some() { 0 } else { u64::MAX };
    let identity_map = DocumentIdentityMap::new(&document);
    Self {
      focus_handle: cx.focus_handle(),
      focus_subscriptions: Vec::new(),
      scroll_handle: VirtualListScrollHandle::new(),
      disposed: false,
      document_display_name: document_path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| SharedString::from(name.to_string_lossy().to_string())),
      recovery_path: document_path.as_deref().map(recovery_path_for_document),
      document_path,
      committed_document: document.clone(),
      document,
      selection: EditorSelection::caret(),
      selection_movement_epoch: 0,
      runtime_edit_selection_epoch: None,
      next_local_transaction_id: 1,
      runtime_transaction_in_flight: None,
      config: RichTextEditorConfig::default(),
      edit_generation: 0,
      saved_generation,
      next_edit_generation: 1,
      last_send_document_generation: None,
      last_format_export_generation: None,
      zoom_percent: 100.0,
      save_status: SaveStatus::Saved,
      undo_stack: Vec::new(),
      redo_stack: Vec::new(),
      identity_map,
      pending_semantic_edits: Vec::new(),
      reconciliation_recoveries: 0,
      command_capture_route: CommandCaptureRoute::Disabled,
      native_save_hook: None,
      native_export_hook: None,
      native_undo_hook: None,
      native_recovery_hook: None,
      suppress_command_capture: 0,
      session_undo_redirect: None,
      collaboration_role: None,
      own_collaboration_caret_color_rgb: None,
      recovery_write_in_progress: false,
      recovery_write_pending: false,
      last_recovery_generation: 0,
      paste_cache: None,
      pending_styles: None,
      armed_inline_tool: None,
      current_highlight_style: HighlightStyle::Custom(1),
      current_highlight_choice: Some(HighlightStyle::Custom(1)),
      selecting: false,
      drag_granularity: SelectionGranularity::Character,
      drag_anchor: None,
      smart_selection_left_anchor_word: false,
      smart_selection_exact_override: false,
      last_drag_position: None,
      pending_text_drag: None,
      active_text_drag: None,
      drop_preview: None,
      image_resize_drag: None,
      table_column_resize_drag: None,
      selected_block: None,
      table_cell_block_ix: 0,
      table_cell_anchor: 0,
      table_cell_caret: 0,
      equation_source_anchor: 0,
      equation_source_caret: 0,
      autoscroll_active: false,
      caret_visible: true,
      caret_blink_active: false,
      external_carets: Vec::new(),
      search_highlights: Vec::new(),
      active_search_highlight: None,
      last_text_input_at: None,
      ime_marked_range: None,
      pending_typing_prefetch_resume: false,
      resume_chunk_prefetch_after_typing: false,
      paragraph_chunk_layout_cache: vec![None; paragraph_count],
      paragraph_prep_cache: vec![ParagraphPrepSlot::default(); paragraph_count],
      paragraph_shaping_cache: (0..paragraph_count).map(|_| None).collect(),
      paragraph_estimate_height_cache: vec![None; paragraph_count],
      pending_layout_prep_task: None,
      pending_layout_prep_request: None,
      layout_generation: 0,
      layout_prep_metrics: LayoutPrepMetrics::default(),
      layout_runtime_metrics: LayoutRuntimeMetrics::default(),
      pending_chunk_prefetch: false,
      chunk_prefetch_queue: VecDeque::new(),
      paragraph_height_cache: vec![None; paragraph_count],
      paragraph_height_cache_revision: 0,
      scroll_materialize_signature: None,
      scroll_materialize_stall_frames: 0,
      item_sizes_cache: None,
      pending_item_sizes_patch_range: None,
      layout_invalidation_hint: None,
      suppress_mutation_notify: 0,
      last_scroll_anchor: None,
      scroll_anchor_lock: None,
      height_prefix_index: HeightPrefixIndex::default(),
      measured_item_width: None,
      pending_viewport_size_refresh: false,
      initial_layout_hidden: true,
      pending_snap_to_paragraph: None,
      pending_scroll_head_after_layout: false,
      visible_layout_generation: 0,
      visible_layout_range: 0..0,
      visible_chunk_anchors: Vec::new(),
      layout_cache_retain_ranges: ParagraphCacheRetainRanges::default(),
      prep_cache_retain_ranges: ParagraphCacheRetainRanges::default(),
      invisibility_mode: false,
      collapsed_section_ids: FxHashSet::default(),
      hovered_collapse_paragraph: None,
      goal_x: None,
    }
  }

  pub fn document(&self) -> &DocumentProjection {
    &self.document
  }

  fn emit_selection_changed(&self, cx: &mut Context<Self>) {
    cx.emit(EditorEvent::SelectionChanged {
      selection: self.selection.clone(),
    });
  }

  pub fn dispose_for_close(&mut self) {
    if self.disposed {
      return;
    }

    self.clear_document_equation_caches();
    self.disposed = true;
    self.focus_subscriptions = Vec::new();
    self.release_transient_memory();

    self.document_path = None;
    self.recovery_path = None;
    self.document = blank_document();
    self.committed_document = self.document.clone();
    self.identity_map = DocumentIdentityMap::new(&self.document);
    let fid_sel_before = self.fidelity_caret_before();
    self.selection = EditorSelection::caret();
    self.fidelity_caret_set("dispose_for_close", &fid_sel_before);
    self.selection_movement_epoch = 0;
    self.runtime_edit_selection_epoch = None;
    self.next_local_transaction_id = 1;
    self.runtime_transaction_in_flight = None;
    self.edit_generation = 0;
    self.saved_generation = 0;
    self.next_edit_generation = 1;
    self.last_send_document_generation = None;
    self.last_format_export_generation = None;
    self.zoom_percent = 100.0;
    self.collapsed_section_ids.clear();
    self.hovered_collapse_paragraph = None;
    self.document.theme.zoom_factor = 1.0;
    self.save_status = SaveStatus::Saved;
    self.last_recovery_generation = 0;
  }

  fn release_transient_memory(&mut self) {
    self.undo_stack = Vec::new();
    self.redo_stack = Vec::new();
    self.pending_semantic_edits.clear();
    self.runtime_edit_selection_epoch = None;
    self.runtime_transaction_in_flight = None;
    self.command_capture_route = CommandCaptureRoute::Disabled;
    self.native_save_hook = None;
    self.native_export_hook = None;
    self.native_undo_hook = None;
    self.native_recovery_hook = None;
    self.suppress_command_capture = 0;
    self.session_undo_redirect = None;
    self.collaboration_role = None;
    self.own_collaboration_caret_color_rgb = None;
    self.recovery_write_in_progress = false;
    self.recovery_write_pending = false;
    self.paste_cache = None;
    self.search_highlights.clear();
    self.active_search_highlight = None;
    self.pending_styles = None;
    self.armed_inline_tool = None;
    self.selecting = false;
    self.drag_granularity = SelectionGranularity::Character;
    self.drag_anchor = None;
    self.smart_selection_left_anchor_word = false;
    self.smart_selection_exact_override = false;
    self.last_drag_position = None;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.drop_preview = None;
    self.image_resize_drag = None;
    self.table_column_resize_drag = None;
    self.selected_block = None;
    self.table_cell_block_ix = 0;
    self.table_cell_anchor = 0;
    self.table_cell_caret = 0;
    self.equation_source_anchor = 0;
    self.equation_source_caret = 0;
    self.autoscroll_active = false;
    self.caret_visible = false;
    self.caret_blink_active = false;
    self.external_carets.clear();
    self.last_text_input_at = None;
    self.ime_marked_range = None;
    self.pending_typing_prefetch_resume = false;
    self.resume_chunk_prefetch_after_typing = false;
    self.paragraph_chunk_layout_cache = Vec::new();
    self.paragraph_prep_cache = Vec::new();
    self.paragraph_shaping_cache = Vec::new();
    self.paragraph_estimate_height_cache = Vec::new();
    self.pending_layout_prep_task = None;
    self.pending_layout_prep_request = None;
    self.layout_generation = self.layout_generation.wrapping_add(1);
    self.layout_prep_metrics = LayoutPrepMetrics::default();
    self.layout_runtime_metrics = LayoutRuntimeMetrics::default();
    self.pending_chunk_prefetch = false;
    self.chunk_prefetch_queue = VecDeque::new();
    self.paragraph_height_cache = Vec::new();
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.pending_item_sizes_patch_range = None;
    self.layout_invalidation_hint = None;
    self.suppress_mutation_notify = 0;
    self.last_scroll_anchor = None;
    self.scroll_anchor_lock = None;
    self.height_prefix_index = HeightPrefixIndex::default();
    self.measured_item_width = None;
    self.pending_viewport_size_refresh = false;
    self.initial_layout_hidden = true;
    self.pending_snap_to_paragraph = None;
    self.pending_scroll_head_after_layout = false;
    self.visible_layout_generation = self.visible_layout_generation.wrapping_add(1);
    self.visible_layout_range = 0..0;
    self.visible_chunk_anchors = Vec::new();
    self.layout_cache_retain_ranges = ParagraphCacheRetainRanges::default();
    self.prep_cache_retain_ranges = ParagraphCacheRetainRanges::default();
    self.goal_x = None;
  }

  pub fn take_pending_session_edits(&mut self) -> Vec<SemanticCommandBatch> {
    if self.command_capture_route.accepts_collaboration() {
      std::mem::take(&mut self.pending_semantic_edits)
    } else {
      Vec::new()
    }
  }

  pub fn take_pending_runtime_edits(&mut self) -> Vec<SemanticCommandBatch> {
    if self.command_capture_route.accepts_runtime() {
      std::mem::take(&mut self.pending_semantic_edits)
    } else {
      Vec::new()
    }
  }

  pub fn take_pending_semantic_edits(&mut self) -> Vec<SemanticCommandBatch> {
    std::mem::take(&mut self.pending_semantic_edits)
  }

  pub fn prepend_pending_semantic_edits(&mut self, mut edits: Vec<SemanticCommandBatch>) {
    if edits.is_empty() {
      return;
    }
    edits.extend(std::mem::take(&mut self.pending_semantic_edits));
    self.pending_semantic_edits = edits;
  }

  pub(super) fn rebuild_visible_from_committed(
    &mut self,
    retry_edits: Vec<SemanticCommandBatch>,
    selection: Option<EditorSelection>,
    cx: &mut Context<Self>,
  ) {
    // §hang-watchdog: this O(document) reconcile runs on every applied collab
    // update; on a 6000-paragraph doc that streams in over many updates it is a
    // prime suspect for the multi-second freezes (it does not log while running).
    let rebuild_started = std::time::Instant::now();
    // The local caret is authoritative live state. Capture it against the
    // pre-rebuild document so it can be re-anchored onto the rebuilt one; we
    // never re-derive the caret from a replayed batch's recorded position, which
    // is what let typed text outrun the cursor on empty-paragraph boundaries.
    let stable_live_selection = StableEditorSelection::capture(&self.document, &self.selection);
    let live_selection = self.selection.clone();
    // Fidelity: snapshot the pre-rebuild caret, document size, and identity map so
    // the reconciliation can be checked for a backward caret jump and dropped ids.
    let fid_sel_before = self.fidelity_caret_before();
    let fid_size_before = flowstate_fidelity::enabled().then(|| self.fidelity_document_size());
    let fid_ids_before = self.fidelity_ids_before();
    // Fingerprint what the user is currently looking at. A rebuild that
    // reproduces it exactly (same paragraph count and total text length) is a
    // local acknowledgement — the text is unchanged and at most some paragraph
    // ids were canonically reassigned by a repair — so the raw live caret is
    // exactly correct and must be kept verbatim. Paragraph count + total byte
    // length is an O(1) fingerprint (vs. iterating per-paragraph lengths) that
    // still captures every local-ack and repair-id-reassignment case.
    let live_fingerprint = (self.document.paragraphs.len(), self.document.text.byte_len());
    let frontier = self.committed_document.frontier.clone();
    let mut edits = retry_edits;
    edits.extend(std::mem::take(&mut self.pending_semantic_edits));
    let mut document = self.committed_document.clone();
    let mut replayed = Vec::with_capacity(edits.len());
    let mut rejected = 0usize;

    for mut edit in edits {
      if edit.semantic_commands.is_empty() {
        continue;
      }
      let mut candidate = document.clone();
      if edit
        .semantic_commands
        .iter()
        .all(|command| replay_semantic_command_on_projection(&mut candidate, command))
      {
        edit.base_frontier.clone_from(&frontier);
        document = candidate;
        replayed.push(edit);
      } else {
        rejected += 1;
      }
    }

    if rejected > 0 {
      self.note_reconciliation_recovery(
        rejected,
        "optimistic editor batches could not be replayed onto the canonical projection",
        cx,
      );
    }

    document.frontier.clone_from(&frontier);
    let theme = self.document.theme.clone();
    self.document = projection_with_local_theme(document, &theme);
    self.identity_map.reconcile(&self.document);
    self.pending_semantic_edits = replayed;

    // When the rebuild is structurally identical to what the user is looking at
    // (every local acknowledgement, even one whose canonical projection
    // reassigned paragraph ids during a repair), the raw live caret is exactly
    // correct, so keep it verbatim — this is what makes "typed text outruns the
    // cursor" structurally impossible on any document. Only a genuine external
    // delta (a remote op or a structural canonical change) re-anchors the caret
    // along its stable paragraph anchor.
    let structurally_identical = (self.document.paragraphs.len(), self.document.text.byte_len()) == live_fingerprint;
    self.selection = if structurally_identical {
      live_selection
    } else {
      stable_live_selection
        .map(|selection| selection.resolve(&self.document))
        .or(selection)
        .unwrap_or(live_selection)
    };
    clamp_selection_to_document(&self.document, &mut self.selection);
    let rebuild_ms = rebuild_started.elapsed().as_millis();
    if rebuild_ms > 150 {
      tracing::warn!("slow reconcile rebuild (hang watchdog): {rebuild_ms}ms, paragraphs={}", self.document.paragraphs.len());
      flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Reconcile, "slow-rebuild", || {
        format!("rebuild_visible_from_committed took {rebuild_ms}ms (paragraphs={})", self.document.paragraphs.len())
      });
    }
    if flowstate_fidelity::enabled() {
      let (pre_paras, pre_len) = fid_size_before.unwrap_or((0, 0));
      let (post_paras, post_len) = self.fidelity_document_size();
      flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Reconcile, "rebuild", || {
        format!(
          "branch={} pre_paras={pre_paras} post_paras={post_paras} pre_len={pre_len} post_len={post_len} pending={} rejected={rejected}",
          if structurally_identical { "identical" } else { "transform" },
          self.pending_semantic_edits.len(),
        )
      });
      self.fidelity_note_dropped_ids("rebuild_visible_from_committed", &fid_ids_before);
      self.fidelity_check_visible_matches_committed("rebuild_visible_from_committed");
      let shrank = post_paras < pre_paras || post_len < pre_len;
      self.fidelity_check_caret_not_regressed("rebuild_visible_from_committed", &fid_sel_before, shrank);
      self.fidelity_caret_set("rebuild_visible_from_committed", &fid_sel_before);
    }
    self.emit_selection_changed(cx);
    self.after_text_mutation(cx);
  }

  /// Open the exactly-one-outstanding runtime transaction gate. Hosts must
  /// check `runtime_transaction_in_flight()` before flushing a new batch.
  pub fn begin_runtime_transaction(&mut self, transaction_id: u128) {
    debug_assert!(
      self.runtime_transaction_in_flight.is_none(),
      "a runtime transaction is already in flight; the flush protocol allows exactly one",
    );
    self.runtime_edit_selection_epoch = Some(self.selection_movement_epoch);
    self.runtime_transaction_in_flight = Some(transaction_id);
  }

  #[must_use]
  pub fn runtime_transaction_in_flight(&self) -> bool {
    self.runtime_transaction_in_flight.is_some()
  }

  /// Complete a local runtime transaction only after its authoritative projection
  /// has already been materialized into `committed_document`. This is the only
  /// success path for closing the transaction gate.
  pub fn complete_runtime_transaction(
    &mut self,
    transaction_id: u128,
    expected_frontier: Vec<u8>,
    selection: Option<StableEditorSelection>,
    cx: &mut Context<Self>,
  ) -> Result<(), ProjectionApplyError> {
    if self.runtime_transaction_in_flight != Some(transaction_id) {
      return Err(ProjectionApplyError::UnexpectedTransaction {
        expected: self.runtime_transaction_in_flight,
        actual: transaction_id,
      });
    }
    if self.committed_document.frontier != expected_frontier {
      return Err(ProjectionApplyError::StaleFrontier {
        expected: expected_frontier,
        actual: self.committed_document.frontier.clone(),
      });
    }
    self.finish_runtime_transaction(selection, cx);
    Ok(())
  }

  /// Close the transaction gate after a failed or repaired commit. The caller
  /// must already have restored a canonical projection (via
  /// `replace_document_projection_replaying_pending`) or be detaching.
  pub fn abort_runtime_transaction(&mut self, cx: &mut Context<Self>) {
    self.runtime_edit_selection_epoch = None;
    self.runtime_transaction_in_flight = None;
    // Newer optimistic edits may be queued behind the failed transaction; wake
    // the host so it can schedule the next serialized runtime flush.
    cx.notify();
  }

  fn finish_runtime_transaction(&mut self, acknowledged_selection: Option<StableEditorSelection>, cx: &mut Context<Self>) {
    let fid_sel_before = self.fidelity_caret_before();
    if self.pending_semantic_edits.is_empty() {
      // The projection rebuild that preceded completion already reconciled the
      // authoritative live caret; we never override it here (overriding it with
      // the flushed batch's position is what let typed text outrun the cursor).
      // We only recover to the acknowledged position if the live caret no longer
      // resolves into the document — a defensive net that should never fire on
      // the normal path.
      if !self.selection_within_document()
        && let Some(acknowledged_selection) = acknowledged_selection
      {
        self.selection = acknowledged_selection.resolve(&self.document);
        clamp_selection_to_document(&self.document, &mut self.selection);
        self.fidelity_caret_set("finish_runtime_transaction/acknowledged", &fid_sel_before);
        self.emit_selection_changed(cx);
      }
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
    }
    // The document is unchanged here, so any backward caret move is a regression
    // (not a deletion): pass `shrank = false`.
    self.fidelity_check_caret_not_regressed("finish_runtime_transaction", &fid_sel_before, false);
    self.runtime_edit_selection_epoch = None;
    self.runtime_transaction_in_flight = None;
    // A completion with newer optimistic edits still queued must wake the host
    // so it can schedule the next serialized runtime flush.
    cx.notify();
  }

  fn selection_within_document(&self) -> bool {
    [self.selection.anchor, self.selection.head].iter().all(|offset| {
      self
        .document
        .paragraphs
        .get(offset.paragraph)
        .is_some_and(|paragraph| offset.byte <= paragraph_text_len(paragraph))
    })
  }

  /// Count of reconciliation recoveries since this editor was created. A
  /// nonzero value means optimistic state diverged from the canonical
  /// projection and had to be repaired — always a bug worth investigating.
  pub fn reconciliation_recoveries(&self) -> u64 {
    self.reconciliation_recoveries
  }

  pub(super) fn note_reconciliation_recovery(&mut self, dropped_batches: usize, reason: &'static str, cx: &mut Context<Self>) {
    self.reconciliation_recoveries = self.reconciliation_recoveries.wrapping_add(1);
    cx.emit(EditorEvent::ReconciliationRecovery {
      dropped_batches,
      reason: reason.into(),
      total_recoveries: self.reconciliation_recoveries,
    });
  }

  pub fn replace_document_projection_replaying_pending(
    &mut self,
    document: DocumentProjection,
    retry_edits: Vec<SemanticCommandBatch>,
    selection: Option<EditorSelection>,
    cx: &mut Context<Self>,
  ) {
    let theme = self.document.theme.clone();
    self.committed_document = projection_with_local_theme(document, &theme);
    self.rebuild_visible_from_committed(retry_edits, selection, cx);
  }

  pub fn restore_runtime_selection(&mut self, selection: EditorSelection, cx: &mut Context<Self>) {
    let fid_sel_before = self.fidelity_caret_before();
    self.selection = selection;
    clamp_selection_to_document(&self.document, &mut self.selection);
    self.fidelity_caret_set("restore_runtime_selection", &fid_sel_before);
    self.emit_selection_changed(cx);
    self.scroll_head_into_view();
    cx.notify();
  }

  pub fn mark_as_unsaved_branch(&mut self, cx: &mut Context<Self>) {
    self.edit_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.refresh_save_status();
    cx.notify();
  }

  pub fn set_session_capture(&mut self, on: bool) {
    self.set_command_capture_route(if on {
      CommandCaptureRoute::Collaboration
    } else if self.command_capture_route.accepts_collaboration() {
      CommandCaptureRoute::Disabled
    } else {
      self.command_capture_route
    });
  }

  pub fn set_runtime_capture(&mut self, on: bool) {
    self.set_command_capture_route(if on {
      CommandCaptureRoute::Runtime
    } else if self.command_capture_route.accepts_runtime() {
      CommandCaptureRoute::Disabled
    } else {
      self.command_capture_route
    });
    if !on {
      self.runtime_edit_selection_epoch = None;
      self.runtime_transaction_in_flight = None;
    }
  }

  fn set_command_capture_route(&mut self, route: CommandCaptureRoute) {
    self.command_capture_route = route;
  }

  pub(super) fn capture_semantic_edit(&mut self, mut edit: SemanticCommandBatch) {
    if self.command_capture_route.is_enabled() && self.suppress_command_capture == 0 {
      if edit.transaction_id == 0 {
        edit.transaction_id = self.next_local_transaction_id;
        self.next_local_transaction_id = self.next_local_transaction_id.wrapping_add(1).max(1);
      }
      edit.selection_movement_epoch = self.selection_movement_epoch;
      edit.stable_selection_after = edit
        .selection_after
        .as_ref()
        .and_then(|selection| StableEditorSelection::capture(&self.document, selection));
      self.pending_semantic_edits.push(edit);
    }
  }

  pub(super) fn note_explicit_selection_movement(&mut self) {
    self.selection_movement_epoch = self.selection_movement_epoch.wrapping_add(1);
  }

  pub(super) fn command_capture_enabled(&self) -> bool {
    self.command_capture_route.is_enabled()
  }

  pub(super) fn local_history_enabled(&self) -> bool {
    !self.command_capture_enabled() && self.native_undo_hook.is_none() && self.session_undo_redirect.is_none()
  }

  pub(super) fn record_local_history(&mut self, record: EditRecord) {
    if self.local_history_enabled() {
      self.undo_stack.push(record);
      self.redo_stack.clear();
    }
  }

  pub fn set_native_save_hook(&mut self, hook: Option<NativeSaveHook>) {
    self.native_save_hook = hook;
  }

  pub fn set_native_export_hook(&mut self, hook: Option<NativeExportHook>) {
    self.native_export_hook = hook;
  }

  pub fn set_native_undo_hook(&mut self, hook: Option<NativeUndoHook>) {
    self.native_undo_hook = hook;
  }

  pub fn set_native_recovery_hook(&mut self, hook: Option<NativeRecoveryHook>) {
    self.native_recovery_hook = hook;
  }

  #[cfg(test)]
  pub(crate) fn pending_scroll_head_after_layout_for_test(&self) -> bool {
    self.pending_scroll_head_after_layout
  }

  pub fn set_session_undo_redirect(&mut self, hook: Option<Rc<dyn Fn(UndoRedirect)>>) {
    self.session_undo_redirect = hook;
  }

  pub fn clear_undo_redo_stacks(&mut self) {
    self.undo_stack.clear();
    self.redo_stack.clear();
  }

  pub fn set_recovery_path(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
    self.recovery_path = path;
    self.schedule_recovery_write(cx);
  }

  pub fn collaboration_role(&self) -> Option<CollaborationRole> {
    self.collaboration_role
  }

  pub fn set_collaboration_role(&mut self, role: Option<CollaborationRole>, cx: &mut Context<Self>) {
    if self.collaboration_role == role {
      return;
    }
    self.collaboration_role = role;
    cx.notify();
  }

  pub fn can_write_collaboration(&self) -> bool {
    self
      .collaboration_role
      .is_none_or(CollaborationRole::can_write)
  }

  pub fn paragraph_id(&self, paragraph_ix: usize) -> Option<ParagraphId> {
    self.identity_map.paragraph_id(paragraph_ix)
  }

  pub fn block_id(&self, block_ix: usize) -> Option<BlockId> {
    self.identity_map.block_id(block_ix)
  }

  fn semantic_block_id(&self, block_ix: usize) -> Option<BlockId> {
    self
      .identity_map
      .block_id(block_ix)
      .or_else(|| self.document.ids.block_ids.get(block_ix).copied())
  }

  pub fn replace_document_projection(&mut self, document: DocumentProjection, cx: &mut Context<Self>) {
    self.committed_document = projection_with_local_theme(document, &self.document.theme);
    self.document = self.committed_document.clone();
    self.identity_map.reconcile(&self.document);
    self.after_text_mutation(cx);
  }

  pub fn document_path(&self) -> Option<&PathBuf> {
    self.document_path.as_ref()
  }

  pub fn set_document_path_for_runtime(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.document_path = Some(path.clone());
    self.recovery_path = Some(recovery_path_for_document(&path));
    cx.notify();
  }

  pub fn set_document_display_name(&mut self, name: SharedString, cx: &mut Context<Self>) {
    self.document_display_name = Some(name);
    cx.notify();
  }

  pub fn config(&self) -> &RichTextEditorConfig {
    &self.config
  }

  pub fn update_config(&mut self, update: impl FnOnce(&mut RichTextEditorConfig), cx: &mut Context<Self>) {
    update(&mut self.config);
    cx.notify();
  }

  pub fn set_smart_word_selection(&mut self, enabled: bool, cx: &mut Context<Self>) {
    if self.config.smart_word_selection != enabled {
      self.config.smart_word_selection = enabled;
      cx.notify();
    }
  }

  pub fn toggle_smart_word_selection(&mut self, cx: &mut Context<Self>) {
    self.config.smart_word_selection = !self.config.smart_word_selection;
    cx.notify();
  }

  pub fn set_show_own_collaboration_caret_color(&mut self, enabled: bool, cx: &mut Context<Self>) {
    if self.config.show_own_collaboration_caret_color != enabled {
      self.config.show_own_collaboration_caret_color = enabled;
      cx.notify();
    }
  }

  pub fn set_own_collaboration_caret_color(&mut self, color_rgb: Option<u32>, cx: &mut Context<Self>) {
    if self.own_collaboration_caret_color_rgb != color_rgb {
      self.own_collaboration_caret_color_rgb = color_rgb;
      cx.notify();
    }
  }

  pub(super) fn local_caret_color_rgb(&self) -> Option<u32> {
    self
      .config
      .show_own_collaboration_caret_color
      .then_some(self.own_collaboration_caret_color_rgb)
      .flatten()
  }

  pub fn save_status(&self) -> &SaveStatus {
    &self.save_status
  }

  pub fn selection(&self) -> &EditorSelection {
    &self.selection
  }

  pub fn set_external_carets(&mut self, external_carets: Vec<ExternalCaret>, cx: &mut Context<Self>) {
    if self.external_carets != external_carets {
      self.external_carets = external_carets;
      cx.notify();
    }
  }

  pub fn external_carets_for_paragraph(&self, paragraph_ix: usize) -> Vec<ExternalCaret> {
    self
      .external_carets
      .iter()
      .filter(|caret| caret.offset.paragraph == paragraph_ix)
      .cloned()
      .collect()
  }
}

// -- Fidelity instrumentation helpers -------------------------------------
//
// Strictly additive diagnostics for caret/reconciliation/text fidelity. Kept in
// a separate impl block WITHOUT `#[hotpath::measure_all]` so the caret hot path
// is not wrapped in profiling timers, and so every entry point stays gated by a
// single relaxed atomic load (`flowstate_fidelity::enabled`) when tracing is
// off. None of these read/write anything that alters editor behavior.
impl RichTextEditor {
  /// Snapshot the live selection immediately before a `self.selection =` write,
  /// but only when fidelity tracing is on. Returns `None` (and clones nothing)
  /// when off, so the common caret hot path pays one atomic load.
  #[inline]
  pub(super) fn fidelity_caret_before(&self) -> Option<EditorSelection> {
    flowstate_fidelity::enabled().then(|| self.selection.clone())
  }

  /// Emit the `Caret`/`set` firehose event for a completed selection write whose
  /// pre-value was captured via [`Self::fidelity_caret_before`]. No-op when the
  /// snapshot is `None` (tracing off).
  #[inline]
  #[allow(clippy::ref_option, reason = "gated diagnostic helper borrows the caller's Option-typed caret snapshot; Option<&T> would push .as_ref() onto every call site")]
  pub(super) fn fidelity_caret_set(&self, site: &'static str, before: &Option<EditorSelection>) {
    if let Some(old) = before {
      self.fidelity_caret_set_from(site, old);
    }
  }

  /// Emit the `Caret`/`set` firehose event for a completed selection write whose
  /// pre-value is already held in an existing local (no extra clone). The detail
  /// closure is lazy, so this costs one atomic load when tracing is off.
  #[inline]
  pub(super) fn fidelity_caret_set_from(&self, site: &'static str, old: &EditorSelection) {
    flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Caret, "set", || {
      format!(
        "site={site} old={old:?} new={:?} frontier_len={} gen={} pending={}",
        self.selection,
        self.committed_document.frontier.len(),
        self.edit_generation,
        self.pending_semantic_edits.len(),
      )
    });
  }

  /// Cheap document size fingerprint (paragraph count, total text length) used to
  /// decide whether a reconciliation shrank the document — a legitimate reason
  /// for the caret to move backward (a remote/committed deletion).
  #[inline]
  pub(super) fn fidelity_document_size(&self) -> (usize, usize) {
    (
      self.document.paragraphs.len(),
      self.document.paragraphs.iter().map(paragraph_text_len).sum::<usize>(),
    )
  }

  /// Assert a reconciliation write did not move the caret head BACKWARD for a
  /// non-user, non-deletion reason. `shrank` suppresses the check when the
  /// document lost paragraphs or text (a deletion legitimately pulls the caret
  /// back). No-op when `before` is `None` (tracing off).
  #[inline]
  #[allow(clippy::ref_option, reason = "gated diagnostic helper borrows the caller's Option-typed caret snapshot; Option<&T> would push .as_ref() onto every call site")]
  pub(super) fn fidelity_check_caret_not_regressed(&self, site: &'static str, before: &Option<EditorSelection>, shrank: bool) {
    let Some(before) = before else { return };
    let before = before.head;
    let after = self.selection.head;
    let regressed = after.paragraph < before.paragraph || (after.paragraph == before.paragraph && after.byte < before.byte);
    flowstate_fidelity::check(!regressed || shrank, flowstate_fidelity::FidelityClass::Caret, "reconcile-regressed", || {
      format!("site={site} before={before:?} after={after:?} shrank={shrank}")
    });
  }

  /// Snapshot the paragraph/block id vectors before a reconcile, gated on tracing.
  #[inline]
  pub(super) fn fidelity_ids_before(&self) -> Option<(Vec<ParagraphId>, Vec<BlockId>)> {
    flowstate_fidelity::enabled().then(|| (self.document.ids.paragraph_ids.clone(), self.document.ids.block_ids.clone()))
  }

  /// Emit an `Identity` firehose event listing paragraph/block ids that were
  /// present before a reconcile but are absent afterward (a prime suspect for
  /// caret jumps and lossy stable-selection fallbacks). No-op when the snapshot
  /// is `None` (tracing off).
  #[allow(clippy::ref_option, reason = "gated diagnostic helper borrows the caller's Option-typed id snapshot; Option<&T> would push .as_ref() onto every call site")]
  pub(super) fn fidelity_note_dropped_ids(&self, site: &'static str, before: &Option<(Vec<ParagraphId>, Vec<BlockId>)>) {
    let Some((pre_paras, pre_blocks)) = before else { return };
    let dropped_paras: Vec<ParagraphId> = pre_paras
      .iter()
      .copied()
      .filter(|id| !self.document.ids.paragraph_ids.contains(id))
      .collect();
    let dropped_blocks: Vec<BlockId> = pre_blocks
      .iter()
      .copied()
      .filter(|id| !self.document.ids.block_ids.contains(id))
      .collect();
    if !dropped_paras.is_empty() || !dropped_blocks.is_empty() {
      flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Identity, "id-dropped", || {
        format!("site={site} dropped_paragraphs={dropped_paras:?} dropped_blocks={dropped_blocks:?}")
      });
    }
  }

  /// Independently verify the visible document equals `committed_document` with
  /// the stored pending edits replayed onto it (paragraph texts compared). All
  /// work is gated behind `enabled()` so it is free when tracing is off.
  pub(super) fn fidelity_check_visible_matches_committed(&self, site: &'static str) {
    if !flowstate_fidelity::enabled() {
      return;
    }
    let mut expected = self.committed_document.clone();
    let mut replay_ok = true;
    for edit in &self.pending_semantic_edits {
      for command in &edit.semantic_commands {
        replay_ok &= replay_semantic_command_on_projection(&mut expected, command);
      }
    }
    let matches = replay_ok
      && expected.paragraphs.len() == self.document.paragraphs.len()
      && (0..expected.paragraphs.len()).all(|ix| paragraph_text(&expected, ix) == paragraph_text(&self.document, ix));
    flowstate_fidelity::check(matches, flowstate_fidelity::FidelityClass::Reconcile, "visible-vs-committed-mismatch", || {
      format!(
        "site={site} replay_ok={replay_ok} expected_paras={} visible_paras={} pending={}",
        expected.paragraphs.len(),
        self.document.paragraphs.len(),
        self.pending_semantic_edits.len(),
      )
    });
  }
}

pub fn replay_semantic_command_on_projection(document: &mut DocumentProjection, command: &SemanticEditCommand) -> bool {
  match command {
    SemanticEditCommand::InsertText { at, text, styles } => {
      if !valid_document_offset(document, *at) {
        return false;
      }
      insert_text_at(document, at.paragraph, at.byte, text, *styles);
      true
    },
    SemanticEditCommand::DeleteRange { range } => {
      if range.start > range.end || !valid_document_offset(document, range.start) || !valid_document_offset(document, range.end) {
        return false;
      }
      if range.start.paragraph == range.end.paragraph {
        delete_range_in_paragraph(document, range.start.paragraph, range.start.byte..range.end.byte);
      } else {
        delete_cross_paragraph_range(document, range.clone());
      }
      true
    },
    SemanticEditCommand::SplitParagraph {
      at,
      source_paragraph,
      source_block,
      new_paragraph,
      new_block,
      inherited_style,
    } => {
      if !valid_document_offset(document, *at) {
        return false;
      }
      if document.ids.paragraph_ids.get(at.paragraph).copied() != Some(*source_paragraph) {
        return false;
      }
      let Some(source_block_ix) = block_ix_for_paragraph(document, at.paragraph) else {
        return false;
      };
      if document.ids.block_ids.get(source_block_ix).copied() != Some(*source_block)
        || document.ids.paragraph_ids.contains(new_paragraph)
        || document.ids.block_ids.contains(new_block)
      {
        return false;
      }
      split_paragraph_at(document, at.paragraph, at.byte);
      document.ids.paragraph_ids[at.paragraph + 1] = *new_paragraph;
      if let Some(new_block_ix) = block_ix_for_paragraph(document, at.paragraph + 1) {
        document.ids.block_ids[new_block_ix] = *new_block;
      }
      let mut updated_style = false;
      if let Some(paragraph) = paragraphs_mut(document).get_mut(at.paragraph + 1)
        && paragraph.style != *inherited_style
      {
        paragraph.style = *inherited_style;
        bump_paragraph_version(paragraph);
        updated_style = true;
      }
      if updated_style {
        update_paragraph_block(document, at.paragraph + 1);
        rebuild_document_sections(document);
      }
      true
    },
    SemanticEditCommand::JoinParagraphs { first, second } => {
      let Some(first_ix) = paragraph_index_for_id(document, *first) else {
        return false;
      };
      let Some(second_ix) = paragraph_index_for_id(document, *second) else {
        return false;
      };
      if first_ix + 1 != second_ix {
        return false;
      }
      let byte = paragraph_text_len(&document.paragraphs[first_ix]);
      delete_cross_paragraph_range(
        document,
        DocumentOffset { paragraph: first_ix, byte }..DocumentOffset {
          paragraph: second_ix,
          byte: 0,
        },
      );
      true
    },
    SemanticEditCommand::SetParagraphStyle { paragraph, style } => {
      let Some(paragraph_ix) = paragraph_index_for_id(document, *paragraph) else {
        return false;
      };
      let mut updated_style = false;
      if let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix)
        && paragraph.style != *style
      {
        paragraph.style = *style;
        bump_paragraph_version(paragraph);
        updated_style = true;
      }
      if updated_style {
        update_paragraph_block(document, paragraph_ix);
        rebuild_document_sections(document);
      }
      true
    },
    SemanticEditCommand::SetRunStyles { paragraph, range, styles } => {
      let Some(paragraph_ix) = paragraph_index_for_id(document, *paragraph) else {
        return false;
      };
      let Some(paragraph) = document.paragraphs.get(paragraph_ix) else {
        return false;
      };
      if range.start > range.end || range.end > paragraph_text_len(paragraph) {
        return false;
      }
      mutate_runs_in_range(
        document,
        DocumentOffset {
          paragraph: paragraph_ix,
          byte: range.start,
        }..DocumentOffset {
          paragraph: paragraph_ix,
          byte: range.end,
        },
        |run_styles| *run_styles = *styles,
      );
      true
    },
    SemanticEditCommand::ReplaceParagraphSpan { start, before, after } => {
      let start_paragraph = start
        .map(|offset| offset.paragraph)
        .unwrap_or(before.start_paragraph)
        .min(document.paragraphs.len());
      let old_count = before
        .paragraphs
        .len()
        .min(document.paragraphs.len().saturating_sub(start_paragraph));
      let current = capture_document_span(document, start_paragraph..start_paragraph + old_count);
      let mut replacement = after.clone();
      replacement.start_paragraph = start_paragraph;
      apply_document_span_replacement(document, &current, &replacement);
      true
    },
    SemanticEditCommand::InsertBlock { block, block_ix, after } => {
      let mut blocks = projection_structural_blocks_from_document(document);
      let row = (*block_ix).min(blocks.len());
      blocks.insert(row, structural_block_for_input(*block, None, after.clone()));
      rebuild_document_from_projection_structural_blocks(document, blocks);
      true
    },
    SemanticEditCommand::DeleteBlock { block } => {
      let Some(block_ix) = document.ids.block_ids.iter().position(|id| id == block) else {
        return false;
      };
      let mut blocks = projection_structural_blocks_from_document(document);
      if block_ix >= blocks.len() {
        return false;
      }
      blocks.remove(block_ix);
      rebuild_document_from_projection_structural_blocks(document, blocks);
      true
    },
    SemanticEditCommand::MoveBlock { block, new_block_ix } => {
      let Some(block_ix) = document.ids.block_ids.iter().position(|id| id == block) else {
        return false;
      };
      let mut blocks = projection_structural_blocks_from_document(document);
      if block_ix >= blocks.len() {
        return false;
      }
      let block = blocks.remove(block_ix);
      blocks.insert((*new_block_ix).min(blocks.len()), block);
      rebuild_document_from_projection_structural_blocks(document, blocks);
      true
    },
    SemanticEditCommand::ReplaceBlock { block, block_ix, after } => {
      let row = block
        .and_then(|block| document.ids.block_ids.iter().position(|id| *id == block))
        .unwrap_or(*block_ix);
      let mut blocks = projection_structural_blocks_from_document(document);
      if row >= blocks.len() {
        return false;
      }
      let block_id = block.unwrap_or(blocks[row].block_id);
      blocks[row] = structural_block_for_input(block_id, blocks[row].paragraph_id, after.clone());
      rebuild_document_from_projection_structural_blocks(document, blocks);
      true
    },
    SemanticEditCommand::InsertTableRow {
      table,
      new_row_id,
      after_row,
      row,
    } => replay_table_edit(document, *table, |table| {
      if table.rows.iter().any(|existing| existing.id == *new_row_id) {
        return false;
      }
      let pos = table_row_insert_pos(table, *after_row).min(table.rows.len());
      table.rows.insert(pos, table_row_from_input_row(row));
      true
    }),
    SemanticEditCommand::DeleteTableRow { table, row_id } => replay_table_edit(document, *table, |table| {
      let Some(ix) = table_row_index(table, *row_id) else {
        return false;
      };
      table.rows.remove(ix);
      true
    }),
    SemanticEditCommand::MoveTableRow { table, row_id, after_row } => replay_table_edit(document, *table, |table| {
      let Some(from) = table_row_index(table, *row_id) else {
        return false;
      };
      let row = table.rows.remove(from);
      let pos = table_row_insert_pos(table, *after_row).min(table.rows.len());
      table.rows.insert(pos, row);
      true
    }),
    SemanticEditCommand::InsertTableColumn {
      table,
      new_column_id,
      after_column,
      width,
      cells,
    } => replay_table_edit(document, *table, |table| {
      if table.columns.iter().any(|column| column.id == *new_column_id) {
        return false;
      }
      let pos = table_column_insert_pos(table, *after_column).min(table.columns.len());
      table.columns.insert(
        pos,
        TableColumn {
          id: *new_column_id,
          width: table_column_width_from_input_width(width),
        },
      );
      for row in &mut table.rows {
        let cell = cells
          .iter()
          .find(|cell| cell.row_id == row.id)
          .map(table_cell_from_input_cell)
          .unwrap_or_else(|| default_table_cell(row.id, *new_column_id));
        let cell_pos = pos.min(row.cells.len());
        row.cells.insert(cell_pos, cell);
      }
      true
    }),
    SemanticEditCommand::DeleteTableColumn { table, column_id } => replay_table_edit(document, *table, |table| {
      let Some(ix) = table_column_index(table, *column_id) else {
        return false;
      };
      table.columns.remove(ix);
      for row in &mut table.rows {
        if let Some(cell_ix) = row.cells.iter().position(|cell| cell.column_id == *column_id) {
          row.cells.remove(cell_ix);
        }
      }
      true
    }),
    SemanticEditCommand::MoveTableColumn {
      table,
      column_id,
      after_column,
    } => replay_table_edit(document, *table, |table| {
      let Some(from) = table_column_index(table, *column_id) else {
        return false;
      };
      let column = table.columns.remove(from);
      let pos = table_column_insert_pos(table, *after_column).min(table.columns.len());
      table.columns.insert(pos, column);
      for row in &mut table.rows {
        if let Some(cell_from) = row.cells.iter().position(|cell| cell.column_id == *column_id) {
          let cell = row.cells.remove(cell_from);
          let cell_pos = pos.min(row.cells.len());
          row.cells.insert(cell_pos, cell);
        }
      }
      true
    }),
    SemanticEditCommand::ReplaceTableCell {
      table,
      row_id,
      column_id,
      cell,
    } => replay_table_edit(document, *table, |table| {
      let Some(row) = table.rows.iter_mut().find(|row| row.id == *row_id) else {
        return false;
      };
      let Some(target) = row.cells.iter_mut().find(|target| target.column_id == *column_id) else {
        return false;
      };
      *target = table_cell_from_input_cell(cell);
      true
    }),
    SemanticEditCommand::SetTableCellSpan {
      table,
      row_id,
      column_id,
      row_span,
      column_span,
    } => replay_table_edit(document, *table, |table| {
      let Some(row) = table.rows.iter_mut().find(|row| row.id == *row_id) else {
        return false;
      };
      let Some(cell) = row.cells.iter_mut().find(|cell| cell.column_id == *column_id) else {
        return false;
      };
      cell.row_span = (*row_span).max(1);
      cell.col_span = (*column_span).max(1);
      true
    }),
    SemanticEditCommand::SetTableColumnWidth { table, column_ix, width } => replay_table_edit(document, *table, |table| {
      let Some(column) = table.columns.get_mut(*column_ix) else {
        return false;
      };
      column.width = table_column_width_from_input_width(width);
      true
    }),
    SemanticEditCommand::ReplaceEquationSourceRange { equation, range, text } => {
      let Some(block_ix) = document.ids.block_ids.iter().position(|id| id == equation) else {
        return false;
      };
      let Some(Block::Equation(equation)) = Arc::make_mut(&mut document.blocks).get_mut(block_ix) else {
        return false;
      };
      let mut source = equation.source.to_string();
      if range.start > range.end || range.end > source.len() || !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return false;
      }
      source.replace_range(range.clone(), text);
      equation.source = source.into();
      equation.version = equation.version.wrapping_add(1);
      true
    },
    SemanticEditCommand::ReplaceImageAltText { image, text } => replay_image_edit(document, *image, |image| {
      image.alt_text = text.clone().into();
      true
    }),
    SemanticEditCommand::ReplaceImageCaption { image, caption } => replay_image_edit(document, *image, |image| {
      image.caption = caption.as_ref().map(paragraph_from_input_paragraph);
      true
    }),
    SemanticEditCommand::SetImageLayout { image, sizing, alignment } => replay_image_edit(document, *image, |image| {
      image.sizing = image_sizing_from_input_sizing(sizing);
      image.alignment = alignment_from_input_alignment(*alignment);
      true
    }),
  }
}

fn valid_document_offset(document: &DocumentProjection, offset: DocumentOffset) -> bool {
  let Some(paragraph) = document.paragraphs.get(offset.paragraph) else {
    return false;
  };
  if offset.byte > paragraph_text_len(paragraph) {
    return false;
  }
  document
    .text
    .is_char_boundary(paragraph_byte_range(document, offset.paragraph).start + offset.byte)
}

fn structural_block_for_input(block_id: BlockId, paragraph_id: Option<ParagraphId>, block: InputBlock) -> ProjectionStructuralBlock {
  ProjectionStructuralBlock {
    block_id,
    paragraph_id: match &block {
      InputBlock::Paragraph(_) => Some(paragraph_id.unwrap_or_else(new_paragraph_id)),
      InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
    },
    block,
  }
}

fn replay_table_edit(document: &mut DocumentProjection, table_id: BlockId, edit: impl FnOnce(&mut TableBlock) -> bool) -> bool {
  let Some(block_ix) = document.ids.block_ids.iter().position(|id| *id == table_id) else {
    return false;
  };
  let Some(Block::Table(table)) = Arc::make_mut(&mut document.blocks).get_mut(block_ix) else {
    return false;
  };
  if !edit(table) {
    return false;
  }
  table.version = table.version.wrapping_add(1);
  true
}

fn replay_image_edit(document: &mut DocumentProjection, image_id: BlockId, edit: impl FnOnce(&mut ImageBlock) -> bool) -> bool {
  let Some(block_ix) = document.ids.block_ids.iter().position(|id| *id == image_id) else {
    return false;
  };
  let Some(Block::Image(image)) = Arc::make_mut(&mut document.blocks).get_mut(block_ix) else {
    return false;
  };
  if !edit(image) {
    return false;
  }
  image.version = image.version.wrapping_add(1);
  true
}

fn table_row_from_input_row(row: &InputTableRow) -> TableRow {
  TableRow {
    id: row.id,
    cells: row.cells.iter().map(table_cell_from_input_cell).collect(),
  }
}

fn table_cell_from_input_cell(cell: &InputTableCell) -> TableCell {
  TableCell {
    id: cell.id,
    row_id: cell.row_id,
    column_id: cell.column_id,
    blocks: cell
      .blocks
      .iter()
      .map(|block| match block {
        InputTableCellBlock::Paragraph(paragraph) => TableCellBlock::Paragraph(table_cell_paragraph_from_input_paragraph(paragraph)),
        InputTableCellBlock::Table(table) => TableCellBlock::Table(table_from_input_table(table)),
      })
      .collect(),
    row_span: cell.row_span,
    col_span: cell.col_span,
  }
}

/// §P2b table position resolution shared by every id-addressed replay case.
/// Mirrors the canonical apply in flowstate-collab so the optimistic projection
/// and the merged Loro state stay byte-identical.
fn table_row_index(table: &TableBlock, row_id: RowId) -> Option<usize> {
  table.rows.iter().position(|row| row.id == row_id)
}

fn table_row_insert_pos(table: &TableBlock, after_row: Option<RowId>) -> usize {
  match after_row {
    None => 0,
    Some(anchor) => table
      .rows
      .iter()
      .position(|row| row.id == anchor)
      .map(|ix| ix + 1)
      .unwrap_or(table.rows.len()),
  }
}

fn table_column_index(table: &TableBlock, column_id: ColumnId) -> Option<usize> {
  table.columns.iter().position(|column| column.id == column_id)
}

fn table_column_insert_pos(table: &TableBlock, after_column: Option<ColumnId>) -> usize {
  match after_column {
    None => 0,
    Some(anchor) => table
      .columns
      .iter()
      .position(|column| column.id == anchor)
      .map(|ix| ix + 1)
      .unwrap_or(table.columns.len()),
  }
}

fn table_column_width_from_input_width(width: &InputTableColumnWidth) -> TableColumnWidth {
  match *width {
    InputTableColumnWidth::Auto => TableColumnWidth::Auto,
    InputTableColumnWidth::FixedPx(px) => TableColumnWidth::FixedPx(px),
    InputTableColumnWidth::Fraction(fraction) => TableColumnWidth::Fraction(fraction),
  }
}

fn image_sizing_from_input_sizing(sizing: &InputImageSizing) -> ImageSizing {
  match *sizing {
    InputImageSizing::Intrinsic => ImageSizing::Intrinsic,
    InputImageSizing::FitWidth => ImageSizing::FitWidth,
    InputImageSizing::Fixed { width_px, height_px } => ImageSizing::Fixed { width_px, height_px },
  }
}

#[cfg(test)]
mod projection_theme_tests {
  use super::*;

  #[test]
  fn replacement_projection_preserves_local_theme_catalog() {
    let mut current = blank_document();
    current.theme.body_font_size = px(18.0);
    current.theme.custom_highlight_styles.insert(77, CustomHighlightStyle {
      color: rgb(0x0012_3456).into(),
    });

    let replacement = projection_with_local_theme(blank_document(), &current.theme);

    assert_eq!(replacement.theme.body_font_size, px(18.0));
    assert!(replacement.theme.custom_highlight_styles.contains_key(&77));
  }
}
