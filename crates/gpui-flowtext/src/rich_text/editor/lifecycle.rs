#[hotpath::measure_all]
impl RichTextEditor {
  pub fn clear_document_equation_caches(&self) {
    let keys = self.document.blocks.iter().filter_map(|block| match block {
      Block::Equation(equation) => Some((equation.source.clone(), matches!(equation.display, EquationDisplay::Display))),
      _ => None,
    });
    EquationRenderer::clear_entries(keys);
  }

  pub fn new_with_path(document: Document, document_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
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
      document,
      selection: EditorSelection::caret(),
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
      last_collaboration_edit: None,
      collaboration_role: None,
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
      local_caret_color_rgb: None,
      external_carets: Vec::new(),
      external_selections: Vec::new(),
      search_highlights: Vec::new(),
      active_search_highlight: None,
      last_text_input_at: None,
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
      item_sizes_cache: None,
      pending_item_sizes_patch_range: None,
      layout_invalidation_hint: None,
      suppress_mutation_notify: 0,
      remote_projection_depth: 0,
      remote_projection_dirty: false,
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

  pub fn document(&self) -> &Document {
    &self.document
  }

  pub fn document_path(&self) -> Option<&PathBuf> {
    self.document_path.as_ref()
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
    self.identity_map = DocumentIdentityMap::new(&self.document);
    self.selection = EditorSelection::caret();
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
    self.last_collaboration_edit = None;
    self.collaboration_role = None;
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
    self.local_caret_color_rgb = None;
    self.external_carets.clear();
    self.external_selections.clear();
    self.last_text_input_at = None;
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
    self.remote_projection_depth = 0;
    self.remote_projection_dirty = false;
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

  pub fn last_collaboration_edit(&self) -> Option<&CollaborationEdit> {
    self.last_collaboration_edit.as_ref()
  }

  pub fn last_collaboration_operations(&self) -> Option<&[CanonicalOperation]> {
    self
      .last_collaboration_edit
      .as_ref()
      .map(|edit| edit.operations.as_slice())
  }

  pub fn last_collaboration_source_mutations(&self) -> Option<&[Db8CollabSourceMutation]> {
    self
      .last_collaboration_edit
      .as_ref()
      .map(|edit| edit.source_mutations.as_slice())
  }

  pub fn last_collaboration_operation_bytes(&self) -> Option<Vec<u8>> {
    self
      .last_collaboration_edit
      .as_ref()
      .and_then(|edit| crate::encode_canonical_operations(&edit.operations))
  }

  pub fn clear_collaboration_edit(&mut self) {
    self.last_collaboration_edit = None;
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

  pub fn set_local_caret_color_rgb(&mut self, color_rgb: Option<u32>, cx: &mut Context<Self>) {
    if self.local_caret_color_rgb == color_rgb {
      return;
    }
    self.local_caret_color_rgb = color_rgb;
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

  pub fn table_cell_id(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> Option<TableCellId> {
    self.identity_map.table_cell_id(block_ix, row_ix, cell_ix)
  }

  #[hotpath::measure]
  pub fn apply_remote_operations(&mut self, operations: &[CanonicalOperation], cx: &mut Context<Self>) -> bool {
    if Self::collab_canary_enabled() {
      eprintln!("[FLOWSTATE_COLLAB_CANARY editor::apply_remote_operations] ops={}", operations.len());
    }
    let mut applied_any = false;
    let mut outcome = RemoteOperationOutcome::Applied;
    let mut selection = self.selection.clone();
    for operation in operations {
      let before_document = remote_selection_transform_needs_pre_edit_document(operation).then(|| self.document.clone());
      let applied = self.apply_canonical_operation(operation);
      if matches!(applied, RemoteOperationOutcome::Applied) {
        let transform_document = before_document.as_ref().unwrap_or(&self.document);
        transform_selection_for_remote_operation(&mut selection, operation, &self.identity_map, transform_document);
        self.identity_map.reconcile(&self.document);
      }
      applied_any |= matches!(applied, RemoteOperationOutcome::Applied);
      outcome = outcome.max(applied);
    }
    if Self::collab_canary_enabled() {
      eprintln!("[FLOWSTATE_COLLAB_CANARY editor::apply_remote_operations_result] applied_any={applied_any} outcome={outcome:?}");
    }
    self.identity_map.reconcile(&self.document);
    self.selection = selection;
    self.last_collaboration_edit = None;
    if applied_any {
      self.mark_remote_document_changed(cx);
      self.after_remote_text_mutation(cx);
    } else if !matches!(outcome, RemoteOperationOutcome::Applied) {
      cx.notify();
    }
    matches!(outcome, RemoteOperationOutcome::Applied)
  }

  #[hotpath::measure]
  pub fn replace_document_from_collaboration(&mut self, document: Document, cx: &mut Context<Self>) {
    if Self::collab_canary_enabled() {
      eprintln!(
        "[FLOWSTATE_COLLAB_CANARY editor::replace_document_from_collaboration] paragraphs={}",
        document.paragraphs.len()
      );
    }
    self.document = document;
    self.identity_map.reconcile(&self.document);
    self.clamp_selection_to_document();
    self.last_collaboration_edit = None;
    self.mark_remote_document_changed(cx);
    self.after_remote_text_mutation(cx);
  }

  pub fn apply_remote_projection_batch<F>(&mut self, cx: &mut Context<Self>, apply: F) -> bool
  where
    F: FnOnce(&mut Self, &mut Context<Self>) -> bool,
  {
    let backup_document = self.document.clone();
    let backup_selection = self.selection.clone();
    self.remote_projection_depth = self.remote_projection_depth.saturating_add(1);
    let success = apply(self, cx);
    self.remote_projection_depth = self.remote_projection_depth.saturating_sub(1);

    if !success {
      self.document = backup_document;
      self.selection = backup_selection;
      self.identity_map.reconcile(&self.document);
      self.remote_projection_dirty = false;
      self.after_remote_text_mutation(cx);
      cx.notify();
      return false;
    }

    if self.remote_projection_depth == 0 && self.remote_projection_dirty {
      self.remote_projection_dirty = false;
      rebuild_document_sections(&mut self.document);
      self.identity_map.reconcile(&self.document);
      self.clamp_selection_to_document();
      self.mark_remote_document_changed(cx);
      self.after_remote_text_mutation(cx);
    }
    true
  }

  fn finish_remote_projection_change(&mut self, structural: bool, cx: &mut Context<Self>) {
    if structural {
      self.identity_map.reconcile(&self.document);
    }
    if self.remote_projection_depth > 0 {
      self.remote_projection_dirty = true;
      return;
    }
    rebuild_document_sections(&mut self.document);
    self.identity_map.reconcile(&self.document);
    self.clamp_selection_to_document();
    self.mark_remote_document_changed(cx);
    self.after_remote_text_mutation(cx);
  }

  /// Apply a remote text-content change to a single paragraph, bypassing
  /// `CanonicalOperation`.  This is the single-authority incremental path:
  /// the paragraph's text is replaced according to the CRDT diff, and runs
  /// are preserved for unchanged portions.
  pub fn apply_remote_text_change(&mut self, paragraph: ParagraphId, new_text: &str, cx: &mut Context<Self>) -> bool {
    let Some(paragraph_ix) = self.identity_map.paragraph_index(paragraph) else {
      return false;
    };
    let old_text = crate::paragraph_text(&self.document, paragraph_ix);
    if old_text == new_text {
      return true;
    }

    let old_bytes = old_text.as_bytes();
    let new_bytes = new_text.as_bytes();

    let prefix = old_bytes
      .iter()
      .zip(new_bytes.iter())
      .take_while(|(a, b)| a == b)
      .count();

    let suffix = old_bytes[prefix..]
      .iter()
      .rev()
      .zip(new_bytes[prefix..].iter().rev())
      .take_while(|(a, b)| a == b)
      .count();

    let old_middle_end = old_bytes.len() - suffix;

    if prefix < old_middle_end
      && !delete_range_in_paragraph(&mut self.document, paragraph_ix, prefix..old_middle_end)
    {
      return false;
    }

    let new_middle = &new_bytes[prefix..(new_bytes.len() - suffix)];
    if !new_middle.is_empty() {
      let text = String::from_utf8_lossy(new_middle).to_string();
      if !insert_text_at(&mut self.document, paragraph_ix, prefix, &text, RunStyles::default()) {
        return false;
      }
    }

    self.finish_remote_projection_change(false, cx);
    true
  }

  /// Apply authoritative remote text and inline runs atomically.
  pub fn apply_remote_paragraph_state(
    &mut self,
    paragraph: ParagraphId,
    new_text: &str,
    runs: Vec<TextRun>,
    cx: &mut Context<Self>,
  ) -> bool {
    let run_total: usize = runs.iter().map(|run| run.len).sum();
    if run_total != new_text.len() || runs.iter().any(|run| run.len == 0) {
      return false;
    }
    let Some(paragraph_ix) = self.identity_map.paragraph_index(paragraph) else {
      return false;
    };
    let old_len = crate::paragraph_text_len(&self.document.paragraphs[paragraph_ix]);
    if old_len > 0 && !delete_range_in_paragraph(&mut self.document, paragraph_ix, 0..old_len) {
      return false;
    }
    if !new_text.is_empty() && !insert_text_at(&mut self.document, paragraph_ix, 0, new_text, RunStyles::default()) {
      return false;
    }
    let Some(p) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix) else {
      return false;
    };
    p.runs = runs;
    bump_paragraph_version(p);
    update_paragraph_block(&mut self.document, paragraph_ix);
    if crate::paragraph_text(&self.document, paragraph_ix) != new_text {
      return false;
    }
    self.finish_remote_projection_change(false, cx);
    true
  }

  /// Apply paragraph-level style while preserving authoritative inline runs.
  pub fn apply_remote_paragraph_style(
    &mut self,
    paragraph: ParagraphId,
    style: ParagraphStyle,
    cx: &mut Context<Self>,
  ) -> bool {
    let Some(paragraph_ix) = self.identity_map.paragraph_index(paragraph) else {
      return false;
    };
    let Some(p) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix) else {
      return false;
    };
    p.style = style;
    bump_paragraph_version(p);
    update_paragraph_block(&mut self.document, paragraph_ix);
    self.finish_remote_projection_change(false, cx);
    true
  }

  /// Apply a remote style/metadata change to a single paragraph, bypassing
  /// `CanonicalOperation`.  Sets both the paragraph-level style and the
  /// full run list (from the CRDT metadata).
  pub fn apply_remote_style_change(
    &mut self,
    paragraph: ParagraphId,
    style: ParagraphStyle,
    runs: Vec<TextRun>,
    cx: &mut Context<Self>,
  ) -> bool {
    let Some(paragraph_ix) = self.identity_map.paragraph_index(paragraph) else {
      return false;
    };
    let Some(p) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix) else {
      return false;
    };
    p.style = style;
    p.runs = runs;
    bump_paragraph_version(p);
    update_paragraph_block(&mut self.document, paragraph_ix);
    self.finish_remote_projection_change(false, cx);
    true
  }

  /// Insert a paragraph and apply its full authoritative state as one receiver
  /// projection. This is used for remote splits and avoids exposing intermediate
  /// split/replace/style states to layout and identity reconciliation.
  pub fn apply_remote_insert_paragraph_authoritative(
    &mut self,
    new_paragraph: ParagraphId,
    position: usize,
    text: &str,
    runs: Vec<TextRun>,
    style: ParagraphStyle,
    cx: &mut Context<Self>,
  ) -> bool {
    let run_total: usize = runs.iter().map(|run| run.len).sum();
    if run_total != text.len() || runs.iter().any(|run| run.len == 0) {
      return false;
    }
    if self.identity_map.paragraph_index(new_paragraph).is_some() || self.document.paragraphs.is_empty() || position == 0 {
      return false;
    }

    let split_ix = position
      .saturating_sub(1)
      .min(self.document.paragraphs.len().saturating_sub(1));
    let byte = crate::paragraph_text_len(&self.document.paragraphs[split_ix]);
    if !split_paragraph_at_with_id(&mut self.document, split_ix, byte, new_paragraph) {
      return false;
    }

    let new_ix = split_ix + 1;
    let old_len = crate::paragraph_text_len(&self.document.paragraphs[new_ix]);
    if old_len > 0 && !delete_range_in_paragraph(&mut self.document, new_ix, 0..old_len) {
      return false;
    }
    if !text.is_empty() && !insert_text_at(&mut self.document, new_ix, 0, text, RunStyles::default()) {
      return false;
    }
    let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(new_ix) else {
      return false;
    };
    paragraph.style = style;
    paragraph.runs = runs;
    bump_paragraph_version(paragraph);
    update_paragraph_block(&mut self.document, new_ix);
    self.finish_remote_projection_change(true, cx);
    true
  }

  /// Insert a new paragraph at the given positional index, bypassing
  /// `CanonicalOperation`.  `new_paragraph` is the stable ID the new
  /// paragraph should carry (from the CRDT).
  pub fn apply_remote_insert_paragraph(&mut self, new_paragraph: ParagraphId, position: usize, cx: &mut Context<Self>) -> bool {
    self.apply_remote_insert_paragraph_authoritative(
      new_paragraph,
      position,
      "",
      Vec::new(),
      ParagraphStyle::Normal,
      cx,
    )
  }

  #[must_use]
  pub fn previous_paragraph_id_for_remote_removal(&self, paragraph: ParagraphId) -> Option<ParagraphId> {
    let paragraph_ix = self.identity_map.paragraph_index(paragraph)?;
    if paragraph_ix == 0 {
      return None;
    }
    self.identity_map.paragraph_id(paragraph_ix - 1)
  }

  /// Apply a remote paragraph join as one receiver projection. The first
  /// paragraph is replaced with the authoritative merged text/runs, then the
  /// removed paragraph plus its local text is deleted in the same operation.
  pub fn apply_remote_join_paragraphs_authoritative(
    &mut self,
    first: ParagraphId,
    second: ParagraphId,
    merged_text: &str,
    runs: Vec<TextRun>,
    cx: &mut Context<Self>,
  ) -> bool {
    let run_total: usize = runs.iter().map(|run| run.len).sum();
    if run_total != merged_text.len() || runs.iter().any(|run| run.len == 0) {
      return false;
    }
    let Some(first_ix) = self.identity_map.paragraph_index(first) else {
      return false;
    };
    let Some(second_ix) = self.identity_map.paragraph_index(second) else {
      return false;
    };
    if second_ix != first_ix + 1 {
      return false;
    }

    let old_first_len = crate::paragraph_text_len(&self.document.paragraphs[first_ix]);
    if old_first_len > 0 && !delete_range_in_paragraph(&mut self.document, first_ix, 0..old_first_len) {
      return false;
    }
    if !merged_text.is_empty() && !insert_text_at(&mut self.document, first_ix, 0, merged_text, RunStyles::default()) {
      return false;
    }
    let Some(first_paragraph) = paragraphs_mut(&mut self.document).get_mut(first_ix) else {
      return false;
    };
    first_paragraph.runs = runs;
    bump_paragraph_version(first_paragraph);
    update_paragraph_block(&mut self.document, first_ix);

    let merged_len = crate::paragraph_text_len(&self.document.paragraphs[first_ix]);
    let second_len = crate::paragraph_text_len(&self.document.paragraphs[second_ix]);
    let removed = delete_cross_paragraph_range(
      &mut self.document,
      crate::DocumentOffset {
        paragraph: first_ix,
        byte: merged_len,
      }..crate::DocumentOffset {
        paragraph: second_ix,
        byte: second_len,
      },
    );
    if !removed {
      return false;
    }
    self.finish_remote_projection_change(true, cx);
    true
  }

  /// Remove a paragraph after its predecessor has already been replaced with
  /// authoritative merged CRDT text. This deletes the paragraph break and the
  /// removed paragraph's local text instead of appending that local text again.
  pub fn apply_remote_remove_paragraph_after_authoritative_join(
    &mut self,
    paragraph: ParagraphId,
    cx: &mut Context<Self>,
  ) -> bool {
    let Some(first) = self.previous_paragraph_id_for_remote_removal(paragraph) else {
      return false;
    };
    let Some(first_ix) = self.identity_map.paragraph_index(first) else {
      return false;
    };
    let merged_text = crate::paragraph_text(&self.document, first_ix);
    let runs = self.document.paragraphs[first_ix].runs.clone();
    self.apply_remote_join_paragraphs_authoritative(first, paragraph, &merged_text, runs, cx)
  }

  /// Remove a paragraph by joining it with its neighbour, bypassing
  /// `CanonicalOperation`.  Joins with the previous paragraph if possible,
  /// otherwise the next.
  pub fn apply_remote_remove_paragraph(&mut self, paragraph: ParagraphId, cx: &mut Context<Self>) -> bool {
    let Some(first) = self.previous_paragraph_id_for_remote_removal(paragraph) else {
      return false;
    };
    let Some(first_ix) = self.identity_map.paragraph_index(first) else {
      return false;
    };
    let Some(second_ix) = self.identity_map.paragraph_index(paragraph) else {
      return false;
    };
    if second_ix != first_ix + 1 {
      return false;
    }
    let first_text = crate::paragraph_text(&self.document, first_ix);
    let second_text = crate::paragraph_text(&self.document, second_ix);
    let mut merged_text = String::with_capacity(first_text.len() + second_text.len());
    merged_text.push_str(&first_text);
    merged_text.push_str(&second_text);
    let runs = self.document.paragraphs[first_ix].runs.clone();
    self.apply_remote_join_paragraphs_authoritative(first, paragraph, &merged_text, runs, cx)
  }

  fn mark_remote_document_changed(&mut self, cx: &mut Context<Self>) {
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.edit_generation = generation;
    self.refresh_save_status();
    self.schedule_recovery_write(cx);
  }

  fn collab_canary_enabled() -> bool {
    std::env::var_os("FLOWSTATE_COLLAB_CANARY").is_some()
  }

  fn apply_canonical_operation(&mut self, operation: &CanonicalOperation) -> RemoteOperationOutcome {
    match operation {
      CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => {
        let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        if !paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: paragraph_ix,
            byte: *byte,
          },
        ) {
          return RemoteOperationOutcome::Conflict;
        }
        insert_text_at(&mut self.document, paragraph_ix, *byte, text, *styles);
        RemoteOperationOutcome::Applied
      },
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => {
        let Some(start_paragraph) = self.identity_map.paragraph_index(*start_paragraph) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        let Some(end_paragraph) = self.identity_map.paragraph_index(*end_paragraph) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        if !paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: start_paragraph,
            byte: *start_byte,
          },
        ) || !paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: end_paragraph,
            byte: *end_byte,
          },
        ) {
          return RemoteOperationOutcome::Conflict;
        }
        delete_cross_paragraph_range(
          &mut self.document,
          DocumentOffset {
            paragraph: start_paragraph,
            byte: *start_byte,
          }..DocumentOffset {
            paragraph: end_paragraph,
            byte: *end_byte,
          },
        );
        RemoteOperationOutcome::Applied
      },
      CanonicalOperation::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      } => {
        let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        if self.identity_map.paragraph_index(*new_paragraph).is_some() {
          return RemoteOperationOutcome::Conflict;
        }
        if !paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: paragraph_ix,
            byte: *byte,
          },
        ) {
          return RemoteOperationOutcome::Conflict;
        }
        if split_paragraph_at_with_id(&mut self.document, paragraph_ix, *byte, *new_paragraph) {
          RemoteOperationOutcome::Applied
        } else {
          RemoteOperationOutcome::Conflict
        }
      },
      CanonicalOperation::JoinParagraphs { first, second } => {
        let Some(first_ix) = self.identity_map.paragraph_index(*first) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        let Some(second_ix) = self.identity_map.paragraph_index(*second) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        if second_ix != first_ix + 1 {
          return RemoteOperationOutcome::Conflict;
        }
        let byte = paragraph_text_len(&self.document.paragraphs[first_ix]);
        delete_cross_paragraph_range(
          &mut self.document,
          DocumentOffset { paragraph: first_ix, byte }..DocumentOffset {
            paragraph: second_ix,
            byte: 0,
          },
        );
        RemoteOperationOutcome::Applied
      },
      CanonicalOperation::SetParagraphStyle { paragraph, style } => {
        let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix) else {
          return RemoteOperationOutcome::Conflict;
        };
        paragraph.style = *style;
        bump_paragraph_version(paragraph);
        update_paragraph_block(&mut self.document, paragraph_ix);
        rebuild_document_sections(&mut self.document);
        RemoteOperationOutcome::Applied
      },
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => {
        let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph) else {
          return RemoteOperationOutcome::RepairNeeded;
        };
        if !paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.start,
          },
        ) || !paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.end,
          },
        ) {
          return RemoteOperationOutcome::Conflict;
        }
        mutate_runs_in_range(
          &mut self.document,
          DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.start,
          }..DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.end,
          },
          |run_styles| *run_styles = *styles,
        );
        RemoteOperationOutcome::Applied
      },
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => {
        let start = start_paragraph
          .and_then(|id| self.identity_map.paragraph_index(id))
          .unwrap_or(before.start_paragraph);
        let Some(end) = start.checked_add(before.paragraphs.len()) else {
          return RemoteOperationOutcome::Conflict;
        };
        if end > self.document.paragraphs.len() {
          return RemoteOperationOutcome::Conflict;
        }
        let current = capture_document_span(&self.document, start..end);
        if current != *before {
          return RemoteOperationOutcome::Conflict;
        }
        let replacement = DocumentSpan {
          start_paragraph: start,
          paragraphs: after.paragraphs.clone(),
          text: after.text.clone(),
        };
        let _ = apply_document_span_replacement(&mut self.document, &current, &replacement);
        RemoteOperationOutcome::Applied
      },
      CanonicalOperation::InsertBlock { .. }
      | CanonicalOperation::DeleteBlock { .. }
      | CanonicalOperation::MoveBlock { .. }
      | CanonicalOperation::ReplaceBlock { .. }
      | CanonicalOperation::ReplaceDocument => RemoteOperationOutcome::RepairNeeded,
    }
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

  pub fn save_status(&self) -> &SaveStatus {
    &self.save_status
  }

  pub fn selection(&self) -> &EditorSelection {
    &self.selection
  }
  pub fn set_stable_external_carets(&mut self, external_carets: Vec<super::StableExternalCaret>, cx: &mut Context<Self>) {
    let mut remapped = Vec::with_capacity(external_carets.len());
    for caret in external_carets {
      if let Some(caret) = self
        .identity_map
        .remap_stable_external_caret(caret, &self.document)
      {
        remapped.push(caret);
      }
    }
    self.set_external_carets(remapped, cx);
  }

  pub fn remap_stable_selection(&self, selection: super::StableEditorSelection) -> Option<EditorSelection> {
    self
      .identity_map
      .remap_stable_selection(selection, &self.document)
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
  pub fn set_external_selections(&mut self, external_selections: Vec<ExternalSelection>, cx: &mut Context<Self>) {
    if self.external_selections != external_selections {
      self.external_selections = external_selections;
      cx.notify();
    }
  }

  pub(super) fn external_selections_for_paragraph(&self, paragraph_ix: usize) -> Vec<ExternalSelection> {
    self
      .external_selections
      .iter()
      .filter(|selection| {
        let range = selection.selection.normalized();
        range.start.paragraph <= paragraph_ix && range.end.paragraph >= paragraph_ix
      })
      .cloned()
      .collect()
  }

  fn clamp_selection_to_document(&mut self) {
    self.selection = EditorSelection {
      anchor: clamp_document_offset(&self.document, self.selection.anchor),
      head: clamp_document_offset(&self.document, self.selection.head),
    };
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block
      && !table_cell_selection_exists(&self.document, block_ix, row_ix, cell_ix)
    {
      self.selected_block = None;
    }
  }
}

fn clamp_document_offset(document: &Document, offset: DocumentOffset) -> DocumentOffset {
  let Some(paragraph) = document.paragraphs.get(offset.paragraph) else {
    return document_end(document);
  };
  DocumentOffset {
    paragraph: offset.paragraph,
    byte: offset.byte.min(paragraph_text_len(paragraph)),
  }
}

fn table_cell_selection_exists(document: &Document, block_ix: usize, row_ix: usize, cell_ix: usize) -> bool {
  matches!(
    document.blocks.get(block_ix),
    Some(Block::Table(table)) if table.rows.get(row_ix).and_then(|row| row.cells.get(cell_ix)).is_some()
  )
}

fn paragraph_offset_in_bounds(document: &Document, offset: DocumentOffset) -> bool {
  paragraph_offset_is_char_boundary(document, offset.paragraph, offset.byte)
}

fn remote_selection_transform_needs_pre_edit_document(operation: &CanonicalOperation) -> bool {
  matches!(operation, CanonicalOperation::JoinParagraphs { .. })
}

fn transform_selection_for_remote_operation(
  selection: &mut EditorSelection,
  operation: &CanonicalOperation,
  identity_map: &DocumentIdentityMap,
  before_document: &Document,
) {
  selection.anchor = transform_offset_for_remote_operation(selection.anchor, operation, identity_map, before_document);
  selection.head = transform_offset_for_remote_operation(selection.head, operation, identity_map, before_document);
}

fn transform_offset_for_remote_operation(
  offset: DocumentOffset,
  operation: &CanonicalOperation,
  identity_map: &DocumentIdentityMap,
  before_document: &Document,
) -> DocumentOffset {
  match operation {
    CanonicalOperation::InsertText { paragraph, byte, text, .. } => {
      let Some(paragraph_ix) = identity_map.paragraph_index(*paragraph) else {
        return offset;
      };
      if offset.paragraph == paragraph_ix && offset.byte >= *byte {
        return DocumentOffset {
          paragraph: offset.paragraph,
          byte: offset.byte.saturating_add(text.len()),
        };
      }
      offset
    },
    CanonicalOperation::DeleteRange {
      start_paragraph,
      start_byte,
      end_paragraph,
      end_byte,
    } => {
      let Some(start_ix) = identity_map.paragraph_index(*start_paragraph) else {
        return offset;
      };
      let Some(end_ix) = identity_map.paragraph_index(*end_paragraph) else {
        return offset;
      };
      if start_ix == end_ix {
        if offset.paragraph != start_ix || offset.byte <= *start_byte {
          return offset;
        }
        if offset.byte <= *end_byte {
          return DocumentOffset {
            paragraph: start_ix,
            byte: *start_byte,
          };
        }
        return DocumentOffset {
          paragraph: start_ix,
          byte: offset
            .byte
            .saturating_sub(end_byte.saturating_sub(*start_byte)),
        };
      }
      if offset.paragraph < start_ix {
        return offset;
      }
      if offset.paragraph == start_ix {
        if offset.byte <= *start_byte {
          return offset;
        }
        return DocumentOffset {
          paragraph: start_ix,
          byte: *start_byte,
        };
      }
      if offset.paragraph < end_ix {
        return DocumentOffset {
          paragraph: start_ix,
          byte: *start_byte,
        };
      }
      if offset.paragraph == end_ix {
        if offset.byte <= *end_byte {
          return DocumentOffset {
            paragraph: start_ix,
            byte: *start_byte,
          };
        }
        return DocumentOffset {
          paragraph: start_ix,
          byte: start_byte.saturating_add(offset.byte.saturating_sub(*end_byte)),
        };
      }
      DocumentOffset {
        paragraph: offset
          .paragraph
          .saturating_sub(end_ix.saturating_sub(start_ix)),
        byte: offset.byte,
      }
    },
    CanonicalOperation::SplitParagraph { paragraph, byte, .. } => {
      let Some(paragraph_ix) = identity_map.paragraph_index(*paragraph) else {
        return offset;
      };
      if offset.paragraph == paragraph_ix && offset.byte > *byte {
        return DocumentOffset {
          paragraph: paragraph_ix + 1,
          byte: offset.byte.saturating_sub(*byte),
        };
      }
      if offset.paragraph > paragraph_ix {
        return DocumentOffset {
          paragraph: offset.paragraph + 1,
          byte: offset.byte,
        };
      }
      offset
    },
    CanonicalOperation::JoinParagraphs { first, second } => {
      let Some(first_ix) = identity_map.paragraph_index(*first) else {
        return offset;
      };
      let Some(second_ix) = identity_map.paragraph_index(*second) else {
        return offset;
      };
      let first_len = before_document
        .paragraphs
        .get(first_ix)
        .map(paragraph_text_len)
        .unwrap_or(0);
      if offset.paragraph == second_ix {
        return DocumentOffset {
          paragraph: first_ix,
          byte: first_len.saturating_add(offset.byte),
        };
      }
      if offset.paragraph > second_ix {
        return DocumentOffset {
          paragraph: offset.paragraph - 1,
          byte: offset.byte,
        };
      }
      offset
    },
    CanonicalOperation::SetParagraphStyle { .. }
    | CanonicalOperation::SetRunStyles { .. }
    | CanonicalOperation::ReplaceParagraphSpan { .. }
    | CanonicalOperation::InsertBlock { .. }
    | CanonicalOperation::DeleteBlock { .. }
    | CanonicalOperation::MoveBlock { .. }
    | CanonicalOperation::ReplaceBlock { .. }
    | CanonicalOperation::ReplaceDocument => offset,
  }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum RemoteOperationOutcome {
  Applied,
  RepairNeeded,
  Conflict,
}

#[cfg(test)]
mod lifecycle_tests {
  use super::*;

  #[test]
  fn clamp_document_offset_returns_document_end_for_stale_paragraph() {
    let document = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "abc".to_string(),
          styles: RunStyles::default(),
        }],
      }],
    );

    assert_eq!(
      clamp_document_offset(&document, DocumentOffset { paragraph: 9, byte: 100 },),
      DocumentOffset { paragraph: 0, byte: 3 }
    );
  }

  fn three_paragraph_document() -> Document {
    document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "abc".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "def".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "ghi".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
    )
  }

  fn transformed_offset(document: &Document, offset: DocumentOffset, operation: &CanonicalOperation) -> DocumentOffset {
    let identity_map = DocumentIdentityMap::new(document);
    transform_offset_for_remote_operation(offset, operation, &identity_map, document)
  }

  #[test]
  fn remote_insert_shifts_offsets_at_and_after_insert_byte_only() {
    let document = three_paragraph_document();
    let paragraph = paragraph_id_at(&document, 1).unwrap();
    let operation = CanonicalOperation::InsertText {
      paragraph,
      byte: 1,
      text: "XYZ".to_string(),
      styles: RunStyles::default(),
    };

    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 0 }, &operation),
      DocumentOffset { paragraph: 1, byte: 0 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 1 }, &operation),
      DocumentOffset { paragraph: 1, byte: 4 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 2, byte: 1 }, &operation),
      DocumentOffset { paragraph: 2, byte: 1 }
    );
  }

  #[test]
  fn remote_same_paragraph_delete_clamps_inside_and_shifts_after_range() {
    let document = three_paragraph_document();
    let paragraph = paragraph_id_at(&document, 1).unwrap();
    let operation = CanonicalOperation::DeleteRange {
      start_paragraph: paragraph,
      start_byte: 1,
      end_paragraph: paragraph,
      end_byte: 3,
    };

    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 0 }, &operation),
      DocumentOffset { paragraph: 1, byte: 0 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 2 }, &operation),
      DocumentOffset { paragraph: 1, byte: 1 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 5 }, &operation),
      DocumentOffset { paragraph: 1, byte: 3 }
    );
  }

  #[test]
  fn remote_cross_paragraph_delete_maps_offsets_to_merged_document() {
    let document = three_paragraph_document();
    let first = paragraph_id_at(&document, 0).unwrap();
    let third = paragraph_id_at(&document, 2).unwrap();
    let operation = CanonicalOperation::DeleteRange {
      start_paragraph: first,
      start_byte: 2,
      end_paragraph: third,
      end_byte: 1,
    };

    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 0, byte: 1 }, &operation),
      DocumentOffset { paragraph: 0, byte: 1 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 2 }, &operation),
      DocumentOffset { paragraph: 0, byte: 2 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 2, byte: 3 }, &operation),
      DocumentOffset { paragraph: 0, byte: 4 }
    );
  }

  #[test]
  fn remote_split_and_join_transform_paragraph_coordinates_with_byte_seams() {
    let document = three_paragraph_document();
    let first = paragraph_id_at(&document, 0).unwrap();
    let second = paragraph_id_at(&document, 1).unwrap();
    let split = CanonicalOperation::SplitParagraph {
      paragraph: first,
      byte: 2,
      new_paragraph: ParagraphId(999),
    };
    let join = CanonicalOperation::JoinParagraphs { first, second };

    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 0, byte: 3 }, &split),
      DocumentOffset { paragraph: 1, byte: 1 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 2, byte: 1 }, &split),
      DocumentOffset { paragraph: 3, byte: 1 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 1, byte: 2 }, &join),
      DocumentOffset { paragraph: 0, byte: 5 }
    );
    assert_eq!(
      transformed_offset(&document, DocumentOffset { paragraph: 2, byte: 1 }, &join),
      DocumentOffset { paragraph: 1, byte: 1 }
    );
  }

  #[test]
  #[ignore = "target state: remote structural operations must return repair-needed or conflict instead of silently no-oping"]
  fn remote_structural_operations_must_be_repairable_or_conflicting() {}
}
