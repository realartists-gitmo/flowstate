// Projection snapshots carry semantic style slots; their appearance catalog is editor-local.
fn projection_with_local_theme(mut document: DocumentProjection, theme: &DocumentTheme) -> DocumentProjection {
  document.theme = theme.clone();
  document
}

// §act-nine A9.3 version discipline (prerequisite for content-keyed layout
// caches): a canonical install replaces THE projection with materializer
// output whose paragraph versions all restart at 0. The layout caches key on
// (style, version), so a surviving paragraph id whose content changed would
// collide with its cached entries and serve stale layout. Carry every
// surviving id's version FORWARD — always advanced by one, so a
// (style, version) pair is never reused for possibly-different content.
// Conservative by design: surviving-but-unchanged paragraphs re-prep once per
// canonical install (a rare whole-document event). Genuinely new ids keep 0.
fn carry_forward_paragraph_versions(previous: &DocumentProjection, incoming: &mut DocumentProjection) {
  if previous.paragraphs.is_empty() || incoming.paragraphs.is_empty() {
    return;
  }
  let mut previous_versions: FxHashMap<ParagraphId, u64> = FxHashMap::default();
  for (ix, id) in previous.ids.paragraph_ids.iter().enumerate() {
    if let Some(paragraph) = previous.paragraphs.get(ix) {
      previous_versions.insert(*id, paragraph.version);
    }
  }
  let ids = incoming.ids.paragraph_ids.clone();
  for (ix, id) in ids.iter().enumerate() {
    let Some(previous_version) = previous_versions.get(id) else {
      continue;
    };
    if let Some(paragraph) = paragraphs_mut(incoming).get_mut(ix) {
      paragraph.version = previous_version.wrapping_add(1);
    }
    // Mirror into the block copy — the projection invariant every other
    // paragraph mutation site maintains.
    if let Some(row) = incoming.blocks.block_row_for_paragraph_ix(ix) {
      update_paragraph_block_at(incoming, ix, row);
    }
  }
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
      write_authority: None,
      document,
      selection: EditorSelection::caret(),
      caret_anchor: None,
      selection_movement_epoch: 0,
      config: RichTextEditorConfig::default(),
      edit_generation: 0,
      saved_generation,
      next_edit_generation: 1,
      last_send_document_generation: None,
      last_format_export_generation: None,
      zoom_percent: 100.0,
      zoom_anchor: None,
      zoom_anchor_apply_pending: false,
      save_status: SaveStatus::Saved,
      identity_map,
      reconciliation_recoveries: 0,
      native_save_hook: None,
      context_menu_hook: None,
      native_export_hook: None,
      native_recovery_hook: None,
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
      external_selections: Vec::new(),
      annotation_selections: Vec::new(),
      hovered_annotation: None,
      jump_flash: None,
      jump_flash_generation: 0,
      search_highlights: Vec::new(),
      active_search_highlight: None,
      last_text_input_at: None,
      ime_marked_range: None,
      pending_typing_prefetch_resume: false,
      resume_chunk_prefetch_after_typing: false,
      paragraph_chunk_layout_cache: vec![None; paragraph_count],
      paragraph_prep_cache: FxHashMap::default(),
      paragraph_shaping_cache: FxHashMap::default(),
      paragraph_estimate_height_cache: FxHashMap::default(),
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

  fn emit_selection_changed(&mut self, cx: &mut Context<Self>) {
    // Re-arm the caret's CRDT anchor for the NEW selection while we are in sync
    // with the core (the fast O(log n) path for repositioning the caret across a
    // remote edit). Without this the anchor only refreshed after a write/sync,
    // so the first edit — or an incoming remote edit — after any caret move fell
    // to the O(doc) `fork_at` rebase (~350ms on a large doc). Fires once per
    // selection change; `encode_selection_anchor` is a no-op when out of sync or
    // the caret is non-body.
    self.capture_caret_anchor();
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
    let fid_sel_before = self.fidelity_caret_before();
    self.selection = EditorSelection::caret();
    self.fidelity_caret_set("dispose_for_close", &fid_sel_before);
    self.selection_movement_epoch = 0;
    self.edit_generation = 0;
    self.saved_generation = 0;
    self.next_edit_generation = 1;
    self.last_send_document_generation = None;
    self.last_format_export_generation = None;
    self.zoom_percent = 100.0;
    self.zoom_anchor = None;
    self.zoom_anchor_apply_pending = false;
    self.collapsed_section_ids.clear();
    self.hovered_collapse_paragraph = None;
    self.document.theme.zoom_factor = 1.0;
    self.save_status = SaveStatus::Saved;
    self.last_recovery_generation = 0;
  }

  fn release_transient_memory(&mut self) {
    self.native_save_hook = None;
    self.context_menu_hook = None;
    self.native_export_hook = None;
    self.native_recovery_hook = None;
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
    self.external_selections.clear();
    self.annotation_selections.clear();
    self.hovered_annotation = None;
    self.jump_flash = None;
    self.last_text_input_at = None;
    self.ime_marked_range = None;
    self.pending_typing_prefetch_resume = false;
    self.resume_chunk_prefetch_after_typing = false;
    self.paragraph_chunk_layout_cache = Vec::new();
    self.paragraph_prep_cache = FxHashMap::default();
    self.paragraph_shaping_cache = FxHashMap::default();
    self.paragraph_estimate_height_cache = FxHashMap::default();
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

  /// Count of reconciliation recoveries since this editor was created. A
  /// nonzero value means optimistic state diverged from the canonical
  /// projection and had to be repaired — always a bug worth investigating.
  pub fn reconciliation_recoveries(&self) -> u64 {
    self.reconciliation_recoveries
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

  pub(super) fn note_explicit_selection_movement(&mut self) {
    self.selection_movement_epoch = self.selection_movement_epoch.wrapping_add(1);
  }

  pub fn set_context_menu_hook(&mut self, hook: Option<ContextMenuHook>) {
    self.context_menu_hook = hook;
  }

  pub fn set_native_save_hook(&mut self, hook: Option<NativeSaveHook>) {
    self.native_save_hook = hook;
  }

  pub fn set_native_export_hook(&mut self, hook: Option<NativeExportHook>) {
    self.native_export_hook = hook;
  }

  pub fn set_native_recovery_hook(&mut self, hook: Option<NativeRecoveryHook>) {
    self.native_recovery_hook = hook;
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
    self.install_canonical_projection(document, cx);
    // Whole-document replacement: the stale-scan (content-key revalidation of
    // every slot) is warranted HERE — the shared mutation hook no longer
    // invalidates anything (§act-ten A10.12).
    self.invalidate_stale_paragraph_layout_caches();
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

  /// §data-loss fix (2026-07-08): reconcile the editor's WRITE path to the
  /// authority's canonical open path when the runtime attaches — including
  /// resetting it to `None`. Imported formats (docx/pdf) load with `None`: the
  /// source file is NOT a save target, so save/autosave must stay disabled
  /// until the user picks a `.db8` via Save As. The phase-V pending panel seeds
  /// the editor with the *source* path for display/recents; without this reset
  /// that source path becomes the autosave target and gets overwritten with a
  /// `.db8` journal (the docx-clobber bug). Mirrors the one-shot open path,
  /// which builds the editor with `loaded.path` directly.
  pub fn set_runtime_document_path(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
    self.recovery_path = path.as_ref().map(|path| recovery_path_for_document(path));
    self.document_path = path;
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

  /// Install a selection (host/test surface). Clamped to the document; emits
  /// the selection-changed event.
  pub fn set_selection(&mut self, selection: EditorSelection, cx: &mut Context<Self>) {
    let anchor = self.clamp_offset_to_document(selection.anchor);
    let head = self.clamp_offset_to_document(selection.head);
    self.selection = EditorSelection { anchor, head, ..selection };
    self.emit_selection_changed(cx);
    cx.notify();
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

  pub fn set_external_selections(&mut self, external_selections: Vec<ExternalSelection>, cx: &mut Context<Self>) {
    if self.external_selections != external_selections {
      self.external_selections = external_selections;
      cx.notify();
    }
  }

  /// Install non-presence annotation ranges, such as unresolved comment
  /// anchors. These are an overlay only and never mutate document run styles.
  pub fn set_annotation_selections(&mut self, selections: Vec<ExternalSelection>, cx: &mut Context<Self>) {
    if self.annotation_selections != selections {
      self.annotation_selections = selections;
      // The set moved under the pointer; a stale index would glow the wrong
      // span until the next mouse move.
      self.hovered_annotation = None;
      cx.notify();
    }
  }

  /// The installed annotation overlay, read-only (hosts and headless tests
  /// assert on what would paint).
  pub fn annotation_selections(&self) -> &[ExternalSelection] {
    &self.annotation_selections
  }

  /// Track which annotation span the pointer is inside so its underline can
  /// paint the hover emphasis. Called from the mouse-move path with an
  /// already-computed hit-test offset (no extra hit test).
  pub(super) fn update_annotation_hover(&mut self, offset: DocumentOffset, cx: &mut Context<Self>) {
    let next = self
      .annotation_selections
      .iter()
      .position(|annotation| !annotation.selection.is_caret() && offset_in_range(offset, annotation.selection.normalized()));
    if self.hovered_annotation != next {
      self.hovered_annotation = next;
      cx.notify();
    }
  }

  /// Peer selection spans that intersect `paragraph_ix`. A multi-paragraph
  /// selection is returned for every paragraph it covers; the shared paint path
  /// slices the correct byte span per paragraph, mirroring the local selection.
  pub(super) fn external_selections_for_paragraph(&self, paragraph_ix: usize) -> Vec<ExternalSelection> {
    self
      .external_selections
      .iter()
      .filter(|external| {
        let range = external.selection.normalized();
        range.start.paragraph <= paragraph_ix && range.end.paragraph >= paragraph_ix
      })
      .cloned()
      .collect()
  }

  /// Annotation spans intersecting `paragraph_ix`, each tagged with whether it
  /// is the hovered span (which paints the stronger underline).
  pub(super) fn annotation_selections_for_paragraph(&self, paragraph_ix: usize) -> Vec<(ExternalSelection, bool)> {
    self
      .annotation_selections
      .iter()
      .enumerate()
      .filter(|(_, annotation)| {
        let range = annotation.selection.normalized();
        range.start.paragraph <= paragraph_ix && range.end.paragraph >= paragraph_ix
      })
      .map(|(ix, annotation)| (annotation.clone(), self.hovered_annotation == Some(ix)))
      .collect()
  }

  pub(super) fn jump_flash_for_paragraph(&self, paragraph_ix: usize) -> Option<ExternalSelection> {
    let flash = self.jump_flash.as_ref()?;
    let range = flash.selection.normalized();
    (range.start.paragraph <= paragraph_ix && range.end.paragraph >= paragraph_ix).then(|| ExternalSelection {
      selection: flash.selection.clone(),
      color_rgb: flash.color_rgb,
    })
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
  #[allow(
    clippy::ref_option,
    reason = "gated diagnostic helper borrows the caller's Option-typed caret snapshot; Option<&T> would push .as_ref() onto every call site"
  )]
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
        "site={site} old={old:?} new={:?} frontier_len={} gen={}",
        self.selection,
        self.document.frontier.len(),
        self.edit_generation,
      )
    });
  }
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

fn table_column_width_from_input_width(width: &InputTableColumnWidth) -> TableColumnWidth {
  match *width {
    InputTableColumnWidth::Auto => TableColumnWidth::Auto,
    InputTableColumnWidth::FixedPx(px) => TableColumnWidth::FixedPx(px),
    InputTableColumnWidth::Fraction(fraction) => TableColumnWidth::Fraction(fraction),
  }
}

#[cfg(test)]
mod projection_theme_tests {
  use super::*;

  #[test]
  fn replacement_projection_preserves_local_theme_catalog() {
    let mut current = blank_document();
    current.theme.body_font_size = px(18.0);
    current.theme.custom_highlight_styles.insert(
      77,
      CustomHighlightStyle {
        color: rgb(0x0012_3456).into(),
      },
    );

    let replacement = projection_with_local_theme(blank_document(), &current.theme);

    assert_eq!(replacement.theme.body_font_size, px(18.0));
    assert!(replacement.theme.custom_highlight_styles.contains_key(&77));
  }
}
