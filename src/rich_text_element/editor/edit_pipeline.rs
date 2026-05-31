#[hotpath::measure_all]
impl RichTextEditor {
  fn block_local_mutation(&self, cx: &mut Context<Self>) -> bool {
    if self.can_edit_locally() {
      false
    } else {
      cx.notify();
      true
    }
  }

  fn insert_single_grapheme_fast_path(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    if self.block_local_mutation(cx) {
      return true;
    }
    if !is_single_grapheme_text_insert(text) || !self.selection.is_caret() || self.selected_block.is_some() {
      return false;
    }
    let caret = self.selection.head;
    let Some(paragraph) = self.document.paragraphs.get(caret.paragraph) else {
      return false;
    };
    if self.invisibility_mode && matches!(paragraph.style, ParagraphStyle::Normal) {
      return false;
    }
    if caret.byte > paragraph_text_len(paragraph) {
      return false;
    }
    let Some(paragraph_id) = self.identity_map.paragraph_id(caret.paragraph) else {
      return false;
    };

    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let styles = if let Some(styles) = self.pending_styles {
      styles
    } else {
      let (run_ix, _) = run_containing(paragraph, caret.byte);
      paragraph
        .runs
        .get(run_ix)
        .map(|run| run.styles)
        .unwrap_or_default()
    };

    insert_text_at(&mut self.document, caret.paragraph, caret.byte, text, styles);
    let after = DocumentOffset {
      paragraph: caret.paragraph,
      byte: caret.byte + text.len(),
    };
    self.selection = EditorSelection { anchor: after, head: after };

    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::InsertText {
        paragraph: caret.paragraph,
        byte: caret.byte,
        text: text.to_string(),
        styles,
      }],
      canonical_operations: vec![CanonicalOperation::InsertText {
        paragraph: paragraph_id,
        byte: caret.byte,
        text: text.to_string(),
        styles,
      }],
    });
    self.redo_stack.clear();
    self.layout_invalidation_hint = Some(caret.paragraph..caret.paragraph + 1);
    self.suppress_mutation_notify += 1;
    self.after_text_mutation(cx);
    self.suppress_mutation_notify = self.suppress_mutation_notify.saturating_sub(1);
    self.mark_document_changed_with_reconcile(after_generation, false, cx);
    true
  }

  fn apply_document_edit_with_capture_range(
    &mut self,
    cx: &mut Context<Self>,
    capture_range: Option<Range<usize>>,
    edit: impl FnOnce(&mut Self, &mut Context<Self>),
  ) {
    if self.block_local_mutation(cx) {
      return;
    }
    let timing = Instant::now();
    let before_selection = self.selection.clone();
    let before_paragraph_count = self.document.paragraphs.len();
    let before_block_count = self.document.blocks.len();
    let before_range = capture_range.unwrap_or_else(|| self.edit_capture_range());
    let before_span = capture_document_span(&self.document, before_range);
    self.layout_invalidation_hint = Some(before_span.start_paragraph..before_span.start_paragraph + before_span.paragraphs.len());
    self.suppress_mutation_notify += 1;
    edit(self, cx);
    self.suppress_mutation_notify = self.suppress_mutation_notify.saturating_sub(1);
    self.layout_invalidation_hint = None;
    let paragraph_delta = self.document.paragraphs.len() as isize - before_paragraph_count as isize;
    let after_count = before_span
      .paragraphs
      .len()
      .saturating_add_signed(paragraph_delta)
      .min(
        self
          .document
          .paragraphs
          .len()
          .saturating_sub(before_span.start_paragraph),
      );
    let after_span = capture_document_span(&self.document, before_span.start_paragraph..before_span.start_paragraph + after_count);
    self.finish_document_edit(before_span, before_selection, before_block_count, after_span, cx);
    log_timing_lazy("edit command", timing, || {
      format!("paragraphs={}", self.document.paragraphs.len())
    });
  }

  fn edit_capture_range(&self) -> Range<usize> {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return 0..0;
    }
    let range = self.selection.normalized();
    let start = range.start.paragraph.saturating_sub(1);
    let end = (range.end.paragraph + 2)
      .min(paragraph_count)
      .max(start + 1);
    start..end
  }

  fn finish_document_edit(
    &mut self,
    before_span: DocumentSpan,
    before_selection: EditorSelection,
    before_block_count: usize,
    after_span: DocumentSpan,
    cx: &mut Context<Self>,
  ) {
    if before_span == after_span && before_selection == self.selection {
      return;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let canonical_operations = vec![CanonicalOperation::ReplaceParagraphSpan {
      start_paragraph: self.identity_map.paragraph_id(before_span.start_paragraph),
      before: before_span.clone(),
      after: after_span.clone(),
    }];
    let identity_shape_changed = before_span.paragraphs.len() != after_span.paragraphs.len() || before_block_count != self.document.blocks.len();
    let record = EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceParagraphSpan {
        before: before_span,
        after: after_span,
      }],
      canonical_operations,
    };
    self.undo_stack.push(record);
    self.redo_stack.clear();
    self.mark_document_changed_with_reconcile(after_generation, identity_shape_changed, cx);
  }

  fn insert_paragraph_break_at_caret(&mut self, caret: DocumentOffset, block_ix: usize, cx: &mut Context<Self>) {
    if self.block_local_mutation(cx) {
      return;
    }
    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let before_span = capture_document_span(&self.document, caret.paragraph..caret.paragraph + 1);
    self.layout_invalidation_hint = Some(caret.paragraph..caret.paragraph + 1);
    self.suppress_mutation_notify += 1;
    self.insert_paragraph_break(cx);
    self.suppress_mutation_notify = self.suppress_mutation_notify.saturating_sub(1);
    self.layout_invalidation_hint = None;
    self
      .identity_map
      .insert_split_paragraph(caret.paragraph, block_ix);
    let after_span = capture_document_span(&self.document, caret.paragraph..caret.paragraph + 2);

    if before_span == after_span && before_selection == self.selection {
      return;
    }

    let record = EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceParagraphSpan {
        before: before_span.clone(),
        after: after_span.clone(),
      }],
      canonical_operations: vec![CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph: self.identity_map.paragraph_id(caret.paragraph),
        before: before_span,
        after: after_span,
      }],
    };
    self.undo_stack.push(record);
    self.redo_stack.clear();
    self.mark_document_changed_with_reconcile(after_generation, false, cx);
  }

  fn mark_document_changed(&mut self, generation: u64, cx: &mut Context<Self>) {
    self.mark_document_changed_with_reconcile(generation, true, cx);
  }

  fn mark_document_changed_with_reconcile(&mut self, generation: u64, reconcile_identity: bool, cx: &mut Context<Self>) {
    self.edit_generation = generation;
    if reconcile_identity {
      self.identity_map.reconcile(&self.document);
    }
    self.last_collaboration_edit = self.undo_stack.last().map(|record| CollaborationEdit {
      operations: record.canonical_operations.clone(),
    });
    self.refresh_save_status();
    self.schedule_recovery_write(cx);
    cx.notify();
  }

  fn notify_after_mutation(&self, cx: &mut Context<Self>) {
    if self.suppress_mutation_notify == 0 {
      cx.notify();
    }
  }

  fn after_history_restore(&mut self, cx: &mut Context<Self>) {
    self.goal_x = None;
    self.invalidate_document_layout_caches();
    self.refresh_save_status();
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    self.schedule_recovery_write(cx);
    cx.notify();
  }

}
