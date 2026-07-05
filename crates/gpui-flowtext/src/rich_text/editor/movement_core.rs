#[hotpath::measure_all]
impl RichTextEditor {
  fn move_to_offset(&mut self, new_head: DocumentOffset, head_affinity: SelectionAffinity, head_gravity: VisualGravity, extend: bool, cx: &mut Context<Self>) {
    let selection = self.selection.moved(new_head, head_affinity, head_gravity, extend);
    if self.selection.same_positions(&selection) {
      self.goal_x = None;
      return;
    }
    self.note_explicit_selection_movement();
    self.selection = selection;
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

  pub(super) fn after_text_mutation(&mut self, cx: &mut Context<Self>) {
    self.mark_text_input_interaction();
    self.pending_styles = None;
    self.goal_x = None;
    if let Some(range) = self.layout_invalidation_hint.take() {
      let end = range
        .end
        .max(self.selection.head.paragraph.saturating_add(2))
        .min(self.document.paragraphs.len());
      self.invalidate_paragraph_layout_cache_range(range.start..end.max(range.start));
    } else {
      self.invalidate_stale_paragraph_layout_caches();
    }
    self.pending_scroll_head_after_layout = true;
    self.reset_caret_blink(cx);
    self.notify_after_mutation(cx);
  }

  pub(super) fn after_formatting_mutation(&mut self, cx: &mut Context<Self>) {
    self.pending_styles = None;
    self.goal_x = None;
    if let Some(range) = self.layout_invalidation_hint.take() {
      self.invalidate_paragraph_layout_cache_range(range);
    } else {
      self.invalidate_stale_paragraph_layout_caches();
    }
    self.reset_caret_blink(cx);
    self.notify_after_mutation(cx);
  }

}
