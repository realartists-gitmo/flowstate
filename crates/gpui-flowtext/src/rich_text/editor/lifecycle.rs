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
      pending_collab_edits: Vec::new(),
      pending_runtime_edits: Vec::new(),
      runtime_edits_in_flight: 0,
      pending_command_selection: None,
      pending_projection_rollback: None,
      collab_capture: false,
      runtime_capture: false,
      native_save_hook: None,
      native_export_hook: None,
      native_undo_hook: None,
      native_recovery_hook: None,
      suppress_collab_capture: 0,
      collab_undo_redirect: None,
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
    self.pending_collab_edits.clear();
    self.pending_runtime_edits.clear();
    self.runtime_edits_in_flight = 0;
    self.pending_command_selection = None;
    self.pending_projection_rollback = None;
    self.collab_capture = false;
    self.runtime_capture = false;
    self.native_save_hook = None;
    self.native_export_hook = None;
    self.native_undo_hook = None;
    self.native_recovery_hook = None;
    self.suppress_collab_capture = 0;
    self.collab_undo_redirect = None;
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

  pub fn take_pending_collab_edits(&mut self) -> Vec<CollaborationEdit> {
    std::mem::take(&mut self.pending_collab_edits)
  }

  pub fn take_pending_runtime_edits(&mut self) -> Vec<CollaborationEdit> {
    std::mem::take(&mut self.pending_runtime_edits)
  }

  pub fn complete_runtime_edit(&mut self, selection: Option<EditorSelection>, cx: &mut Context<Self>) {
    self.runtime_edits_in_flight = self.runtime_edits_in_flight.saturating_sub(1);
    if self.runtime_edits_in_flight == 0 && self.pending_runtime_edits.is_empty() && self.pending_collab_edits.is_empty() {
      if let Some(selection) = selection {
        self.selection = selection;
        clamp_selection_to_document(&self.document, &mut self.selection);
        self.emit_selection_changed(cx);
      }
      self.pending_command_selection = None;
      self.pending_projection_rollback = None;
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    }
  }

  pub fn begin_runtime_edit(&mut self) {
    self.runtime_edits_in_flight = self.runtime_edits_in_flight.saturating_add(1);
  }

  pub fn restore_runtime_selection(&mut self, selection: EditorSelection, cx: &mut Context<Self>) {
    self.selection = selection;
    clamp_selection_to_document(&self.document, &mut self.selection);
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

  pub fn set_collab_capture(&mut self, on: bool) {
    self.collab_capture = on;
    if !on {
      self.pending_collab_edits.clear();
    }
  }

  pub fn set_runtime_capture(&mut self, on: bool) {
    self.runtime_capture = on;
    if !on {
      self.pending_runtime_edits.clear();
      self.runtime_edits_in_flight = 0;
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

  pub fn set_collab_undo_redirect(&mut self, hook: Option<Rc<dyn Fn(UndoRedirect)>>) {
    self.collab_undo_redirect = hook;
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

  pub fn table_cell_id(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> Option<TableCellId> {
    self.identity_map.table_cell_id(block_ix, row_ix, cell_ix)
  }

  pub fn replace_document_from_collaboration(&mut self, document: DocumentProjection, cx: &mut Context<Self>) {
    self.document = document;
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


