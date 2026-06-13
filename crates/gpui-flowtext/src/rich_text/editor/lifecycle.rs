#[hotpath::measure_all]
impl RichTextEditor {
  pub fn clear_document_equation_caches(&self) {
    let keys = self.document.blocks.iter().filter_map(|block| match block {
      Block::Equation(equation) => Some((equation.source.clone(), matches!(equation.display, EquationDisplay::Display))),
      _ => None,
    });
    EquationRenderer::clear_entries(keys);
  }

  pub fn new_with_path(mut document: Document, document_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
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
      last_collaboration_edit: None,
      pending_collab_edits: Vec::new(),
      collab_capture: false,
      suppress_collab_capture: 0,
      collab_undo_redirect: None,
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
      external_carets: Vec::new(),
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
    self.last_collaboration_edit = None;
    self.pending_collab_edits.clear();
    self.collab_capture = false;
    self.suppress_collab_capture = 0;
    self.collab_undo_redirect = None;
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
    self.external_carets.clear();
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

  pub fn last_collaboration_operation_bytes(&self) -> Option<Vec<u8>> {
    self
      .last_collaboration_edit
      .as_ref()
      .and_then(|edit| crate::encode_canonical_operations(&edit.operations))
  }

  pub fn clear_collaboration_edit(&mut self) {
    self.last_collaboration_edit = None;
  }

  pub fn take_pending_collab_edits(&mut self) -> Vec<CollaborationEdit> {
    std::mem::take(&mut self.pending_collab_edits)
  }

  pub fn set_collab_capture(&mut self, on: bool) {
    self.collab_capture = on;
    if !on {
      self.pending_collab_edits.clear();
    }
  }

  pub fn set_collab_undo_redirect(&mut self, hook: Option<Rc<dyn Fn(UndoRedirect)>>) {
    self.collab_undo_redirect = hook;
  }

  pub fn clear_collab_history(&mut self) {
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

  pub fn table_cell_id(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> Option<TableCellId> {
    self.identity_map.table_cell_id(block_ix, row_ix, cell_ix)
  }

  pub fn apply_remote_operations(&mut self, operations: &[CanonicalOperation], cx: &mut Context<Self>) {
    for operation in operations {
      self.apply_canonical_operation(operation);
    }
    self.identity_map.reconcile(&self.document);
    self.last_collaboration_edit = None;
    self.after_text_mutation(cx);
  }

  pub fn replace_document_from_collaboration(&mut self, document: Document, cx: &mut Context<Self>) {
    self.document = document;
    self.identity_map.reconcile(&self.document);
    self.last_collaboration_edit = None;
    self.after_text_mutation(cx);
  }

  fn apply_canonical_operation(&mut self, operation: &CanonicalOperation) {
    match operation {
      CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => {
        if let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph)
          && paragraph_offset_in_bounds(
            &self.document,
            DocumentOffset {
              paragraph: paragraph_ix,
              byte: *byte,
            },
          )
        {
          insert_text_at(&mut self.document, paragraph_ix, *byte, text, *styles);
        }
      },
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => {
        let Some(start_paragraph) = self.identity_map.paragraph_index(*start_paragraph) else {
          return;
        };
        let Some(end_paragraph) = self.identity_map.paragraph_index(*end_paragraph) else {
          return;
        };
        if paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: start_paragraph,
            byte: *start_byte,
          },
        ) && paragraph_offset_in_bounds(
          &self.document,
          DocumentOffset {
            paragraph: end_paragraph,
            byte: *end_byte,
          },
        ) {
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
        }
      },
      CanonicalOperation::SplitParagraph { paragraph, byte, .. } => {
        if let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph)
          && paragraph_offset_in_bounds(
            &self.document,
            DocumentOffset {
              paragraph: paragraph_ix,
              byte: *byte,
            },
          )
        {
          split_paragraph_at(&mut self.document, paragraph_ix, *byte);
        }
      },
      CanonicalOperation::JoinParagraphs { first, second } => {
        let Some(first_ix) = self.identity_map.paragraph_index(*first) else {
          return;
        };
        let Some(second_ix) = self.identity_map.paragraph_index(*second) else {
          return;
        };
        if second_ix == first_ix + 1 {
          let byte = paragraph_text_len(&self.document.paragraphs[first_ix]);
          delete_cross_paragraph_range(
            &mut self.document,
            DocumentOffset { paragraph: first_ix, byte }..DocumentOffset {
              paragraph: second_ix,
              byte: 0,
            },
          );
        }
      },
      CanonicalOperation::SetParagraphStyle { paragraph, style } => {
        if let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph)
          && let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix)
        {
          paragraph.style = *style;
          bump_paragraph_version(paragraph);
          update_paragraph_block(&mut self.document, paragraph_ix);
          rebuild_document_sections(&mut self.document);
        }
      },
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => {
        if let Some(paragraph_ix) = self.identity_map.paragraph_index(*paragraph)
          && paragraph_offset_in_bounds(
            &self.document,
            DocumentOffset {
              paragraph: paragraph_ix,
              byte: range.start,
            },
          )
          && paragraph_offset_in_bounds(
            &self.document,
            DocumentOffset {
              paragraph: paragraph_ix,
              byte: range.end,
            },
          )
        {
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
        }
      },
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => {
        let start = start_paragraph
          .and_then(|id| self.identity_map.paragraph_index(id))
          .unwrap_or(before.start_paragraph);
        let current = capture_document_span(&self.document, start..start + before.paragraphs.len());
        let replacement = DocumentSpan {
          start_paragraph: start,
          paragraphs: after.paragraphs.clone(),
          text: after.text.clone(),
        };
        apply_document_span_replacement(&mut self.document, &current, &replacement);
      },
      CanonicalOperation::InsertBlock { .. }
      | CanonicalOperation::DeleteBlock { .. }
      | CanonicalOperation::MoveBlock { .. }
      | CanonicalOperation::ReplaceBlock { .. }
      | CanonicalOperation::ReplaceDocument => {},
    }
  }

  pub fn document_path(&self) -> Option<&PathBuf> {
    self.document_path.as_ref()
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

fn paragraph_offset_in_bounds(document: &Document, offset: DocumentOffset) -> bool {
  document
    .paragraphs
    .get(offset.paragraph)
    .is_some_and(|paragraph| offset.byte <= paragraph_text_len(paragraph))
}
