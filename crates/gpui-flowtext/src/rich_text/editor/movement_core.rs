#[hotpath::measure_all]
impl RichTextEditor {
  fn move_to_offset(&mut self, new_head: DocumentOffset, head_affinity: SelectionAffinity, head_gravity: VisualGravity, extend: bool, cx: &mut Context<Self>) {
    let selection = self.selection.moved(new_head, head_affinity, head_gravity, extend);
    if self.selection.same_positions(&selection) {
      self.goal_x = None;
      return;
    }
    self.note_explicit_selection_movement();
    let fid_before = self.fidelity_caret_before();
    self.selection = selection;
    self.fidelity_caret_set("move_to_offset", &fid_before);
    self.emit_selection_changed(cx);
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn word_left(&self, offset: DocumentOffset) -> DocumentOffset {
    previous_debate_word_boundary_in_document(&self.document, offset)
  }

  fn word_right(&self, offset: DocumentOffset) -> DocumentOffset {
    next_debate_word_boundary_in_document(&self.document, offset)
  }

  fn page_move(&mut self, dir: VDir, extend: bool, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let Some(layout) = self.layout_for_offset(head) else {
      return;
    };
    let Some(bounds) = layout.bounds else {
      return;
    };
    let delta = (bounds.size.height - px(40.0)).max(px(40.0));
    let signed_delta = match dir {
      VDir::Up => delta,
      VDir::Down => -delta,
    };
    let old_offset = self.scroll_handle.offset();
    let new_offset = clamp_scroll_offset(&self.scroll_handle, point(old_offset.x, old_offset.y + signed_delta));
    self.scroll_handle.set_offset(new_offset);

    let Some(caret) = caret_bounds(&layout, head, self.selection.head_gravity, bounds.origin) else {
      cx.notify();
      return;
    };
    let target_y = match dir {
      VDir::Up => (caret.origin.y - delta).max(bounds.top()),
      VDir::Down => (caret.origin.y + delta).min(bounds.bottom()),
    };
    let target = self
      .hit_test_cached_position(point(caret.origin.x, target_y))
      .unwrap_or_else(|| layout.hit_test(point(caret.origin.x, target_y)));
    self.move_to_offset(target, SelectionAffinity::Neutral, VisualGravity::Neutral, extend, cx);
  }

  // §act-ten A10.12: neither hook invalidates layout caches anymore. The
  // caches are CONTENT-keyed (§act-nine A9.3 — style+version keys at all four
  // sites), so a mutation's own version bumps invalidate exactly the touched
  // paragraphs; the unconditional whole-cache nuke these hooks used to run
  // (via the dead `layout_invalidation_hint` fallback — its setters were
  // deleted in the loro-first cutover) cost delete-word, toolbar formatting
  // and collab asset arrival a full O(doc) re-prep each. The one caller that
  // genuinely replaces the whole document (`replace_document_projection`)
  // performs its own explicit invalidation.
  pub(super) fn after_text_mutation(&mut self, cx: &mut Context<Self>) {
    self.mark_text_input_interaction();
    self.pending_styles = None;
    self.goal_x = None;
    self.pending_scroll_head_after_layout = true;
    self.reset_caret_blink(cx);
    self.notify_after_mutation(cx);
  }

  pub(super) fn after_formatting_mutation(&mut self, cx: &mut Context<Self>) {
    self.pending_styles = None;
    self.goal_x = None;
    self.reset_caret_blink(cx);
    self.notify_after_mutation(cx);
  }

}
