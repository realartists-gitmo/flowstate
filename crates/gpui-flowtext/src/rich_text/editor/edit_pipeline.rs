#[hotpath::measure_all]
impl RichTextEditor {
  #[hotpath::measure]
  fn insert_single_grapheme_fast_path(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
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
    let Some(paragraph_id) = paragraph_id_at(&self.document, caret.paragraph) else {
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
    let text = text.to_owned();
    let canonical_text = text.clone();

    if !insert_text_at(&mut self.document, caret.paragraph, caret.byte, &text, styles) {
      return false;
    }
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
        text,
        styles,
      }],
      canonical_operations: vec![CanonicalOperation::InsertText {
        paragraph: paragraph_id,
        byte: caret.byte,
        text: canonical_text,
        styles,
      }],
    });
    self.redo_stack.clear();
    self.after_text_mutation(cx);
    self.mark_document_changed_with_reconcile(after_generation, false, cx);
    true
  }

  fn apply_document_edit_with_capture_range(
    &mut self,
    cx: &mut Context<Self>,
    capture_range: Option<Range<usize>>,
    edit: impl FnOnce(&mut Self, &mut Context<Self>),
  ) {
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
    log_timing_lazy("edit command", timing, || format!("paragraphs={}", self.document.paragraphs.len()));
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
    let canonical_operations = self.canonical_operations_for_span_edit(&before_span, &after_span, before_block_count);
    self.identity_map.reconcile(&self.document);
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

  fn canonical_operations_for_span_edit(
    &self,
    before_span: &DocumentSpan,
    after_span: &DocumentSpan,
    before_block_count: usize,
  ) -> Vec<CanonicalOperation> {
    let fallback = || {
      vec![CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph: self.identity_map.paragraph_id(before_span.start_paragraph),
        before: before_span.clone(),
        after: after_span.clone(),
      }]
    };
    if before_block_count != self.document.blocks.len() {
      return fallback();
    }
    if before_span.paragraphs.len() != after_span.paragraphs.len() || before_span.text != after_span.text {
      return self
        .canonical_operations_for_content_replacement(before_span, after_span)
        .unwrap_or_else(fallback);
    }

    let mut operations = Vec::new();
    for (relative_ix, (before, after)) in before_span
      .paragraphs
      .iter()
      .zip(&after_span.paragraphs)
      .enumerate()
    {
      let Some(paragraph_id) = self
        .identity_map
        .paragraph_id(before_span.start_paragraph + relative_ix)
      else {
        return fallback();
      };
      if before.style != after.style {
        operations.push(CanonicalOperation::SetParagraphStyle {
          paragraph: paragraph_id,
          style: after.style,
        });
      }
      Self::append_run_style_diff_operations(&mut operations, paragraph_id, &before.runs, &after.runs);
    }
    if operations.is_empty() { fallback() } else { operations }
  }

  fn canonical_operations_for_content_replacement(
    &self,
    before_span: &DocumentSpan,
    after_span: &DocumentSpan,
  ) -> Option<Vec<CanonicalOperation>> {
    let start = before_span.start_paragraph;
    let before_len = before_span.paragraphs.len();
    let after_len = after_span.paragraphs.len();
    if before_len == 0 || after_len == 0 {
      return None;
    }
    let after_ids = self.document.ids.paragraph_ids.get(start..start + after_len)?;

    let before_ids: Vec<ParagraphId> = (0..before_len)
      .filter_map(|ix| self.identity_map.paragraph_id(start + ix))
      .collect();

    // Capture ranges intentionally include neighbouring paragraphs. When the
    // paragraph shape and stable IDs are unchanged, diff each paragraph in
    // place instead of replacing the whole captured span. Whole-span replay
    // caused unchanged paragraph text to be inserted again after destructive
    // edits, producing the collaboration "ghost paragraph" duplication.
    if before_len == after_len && before_ids.as_slice() == after_ids {
      let mut operations = Vec::new();
      for relative_ix in 0..before_len {
        let paragraph_id = before_ids[relative_ix];
        let before_text = paragraph_text_from_span(before_span, relative_ix)?;
        let after_text = paragraph_text_from_span(after_span, relative_ix)?;
        let before_paragraph = before_span.paragraphs.get(relative_ix)?;
        let after_paragraph = after_span.paragraphs.get(relative_ix)?;
        Self::append_minimal_text_replacement(
          &mut operations,
          paragraph_id,
          &before_text,
          &after_text,
          &after_paragraph.runs,
        );
        if before_paragraph.style != after_paragraph.style {
          operations.push(CanonicalOperation::SetParagraphStyle {
            paragraph: paragraph_id,
            style: after_paragraph.style,
          });
        }
        // Text insertions carry their own style and Loro marks contract around
        // deletions automatically. Replaying every run after every keystroke
        // multiplied one edit into many mark/unmark mutations and dominated
        // collaboration throughput. Full run replay is only needed for a
        // style-only edit.
        if before_text == after_text && before_paragraph.runs != after_paragraph.runs {
          Self::append_exact_run_style_operations(&mut operations, paragraph_id, &after_paragraph.runs);
        }
      }
      return Some(operations);
    }

    // Structural edits still reconcile by stable paragraph ID. Never encode a
    // captured multi-paragraph span as one cross-paragraph DeleteRange followed
    // by reinsertion: the granular adapter only trims the two endpoints, so
    // surviving middle paragraphs retain their old text and then receive a
    // second copy.
    let common_before = before_ids
      .iter()
      .copied()
      .filter(|id| after_ids.contains(id))
      .collect::<Vec<_>>();
    let common_after = after_ids
      .iter()
      .copied()
      .filter(|id| before_ids.contains(id))
      .collect::<Vec<_>>();
    if common_before != common_after || common_after.is_empty() {
      return None;
    }

    let mut operations = Vec::with_capacity(before_ids.len() + after_len * 4);

    // Remove vanished containers first. `JoinParagraphs` maps to an explicit
    // RemoveParagraph mutation; text transferred into a survivor is represented
    // by that survivor's minimal text delta below.
    let survivor = common_after[0];
    for removed_id in before_ids.iter().copied().filter(|id| !after_ids.contains(id)) {
      operations.push(CanonicalOperation::JoinParagraphs {
        first: survivor,
        second: removed_id,
      });
    }

    for (after_ix, (&paragraph_id, after_paragraph)) in after_ids.iter().zip(&after_span.paragraphs).enumerate() {
      let after_text = paragraph_text_from_span(after_span, after_ix)?;
      if let Some(before_ix) = before_ids.iter().position(|id| *id == paragraph_id) {
        let before_text = paragraph_text_from_span(before_span, before_ix)?;
        let before_paragraph = before_span.paragraphs.get(before_ix)?;
        Self::append_minimal_text_replacement(
          &mut operations,
          paragraph_id,
          &before_text,
          &after_text,
          &after_paragraph.runs,
        );
        if before_paragraph.style != after_paragraph.style {
          operations.push(CanonicalOperation::SetParagraphStyle {
            paragraph: paragraph_id,
            style: after_paragraph.style,
          });
        }
        if before_text == after_text && before_paragraph.runs != after_paragraph.runs {
          Self::append_exact_run_style_operations(&mut operations, paragraph_id, &after_paragraph.runs);
        }
      } else {
        let previous_id = *after_ids.get(after_ix.checked_sub(1)?)?;
        operations.push(CanonicalOperation::SplitParagraph {
          paragraph: previous_id,
          byte: paragraph_text_len(after_span.paragraphs.get(after_ix - 1)?),
          new_paragraph: paragraph_id,
        });
        if !after_text.is_empty() {
          operations.push(CanonicalOperation::InsertText {
            paragraph: paragraph_id,
            byte: 0,
            styles: run_style_at_byte(&after_paragraph.runs, 0),
            text: after_text,
          });
        }
        operations.push(CanonicalOperation::SetParagraphStyle {
          paragraph: paragraph_id,
          style: after_paragraph.style,
        });
        Self::append_exact_run_style_operations(&mut operations, paragraph_id, &after_paragraph.runs);
      }
    }
    Some(operations)
  }

  fn append_minimal_text_replacement(
    operations: &mut Vec<CanonicalOperation>,
    paragraph: ParagraphId,
    before_text: &str,
    after_text: &str,
    after_runs: &[TextRun],
  ) {
    if before_text == after_text {
      return;
    }

    let mut prefix = 0usize;
    for (before_char, after_char) in before_text.chars().zip(after_text.chars()) {
      if before_char != after_char {
        break;
      }
      prefix += before_char.len_utf8();
    }

    let mut suffix = 0usize;
    for (before_char, after_char) in before_text[prefix..].chars().rev().zip(after_text[prefix..].chars().rev()) {
      if before_char != after_char {
        break;
      }
      suffix += before_char.len_utf8();
    }

    let before_middle_end = before_text.len().saturating_sub(suffix);
    let after_middle_end = after_text.len().saturating_sub(suffix);
    if std::env::var_os("FLOWSTATE_COLLAB_CANARY").is_some() {
      eprintln!(
        "[FLOWSTATE_COLLAB_CANARY editor::minimal_text_diff] paragraph={} before_bytes={} after_bytes={} prefix={} suffix={} delete_start={} delete_end={} insert_start={} insert_end={} before_window={:?} after_window={:?}",
        paragraph.0,
        before_text.len(),
        after_text.len(),
        prefix,
        suffix,
        prefix,
        before_middle_end,
        prefix,
        after_middle_end,
        collaboration_text_window(before_text, prefix, before_middle_end),
        collaboration_text_window(after_text, prefix, after_middle_end),
      );
    }
    if prefix < before_middle_end {
      operations.push(CanonicalOperation::DeleteRange {
        start_paragraph: paragraph,
        start_byte: prefix,
        end_paragraph: paragraph,
        end_byte: before_middle_end,
      });
    }
    if prefix < after_middle_end {
      operations.push(CanonicalOperation::InsertText {
        paragraph,
        byte: prefix,
        text: after_text[prefix..after_middle_end].to_string(),
        styles: run_style_at_byte(after_runs, prefix),
      });
    }
  }

  fn append_exact_run_style_operations(operations: &mut Vec<CanonicalOperation>, paragraph: ParagraphId, runs: &[TextRun]) {
    let mut offset = 0;
    for run in runs {
      let end = offset + run.len;
      if end > offset {
        operations.push(CanonicalOperation::SetRunStyles {
          paragraph,
          range: offset..end,
          styles: run.styles,
        });
      }
      offset = end;
    }
  }

  fn append_run_style_diff_operations(
    operations: &mut Vec<CanonicalOperation>,
    paragraph: ParagraphId,
    before_runs: &[TextRun],
    after_runs: &[TextRun],
  ) {
    let before_len = before_runs.iter().map(|run| run.len).sum::<usize>();
    let after_len = after_runs.iter().map(|run| run.len).sum::<usize>();
    if before_len != after_len || before_len == 0 {
      return;
    }
    let mut boundaries = Vec::with_capacity(before_runs.len() + after_runs.len() + 2);
    boundaries.push(0);
    boundaries.push(before_len);
    append_run_boundaries(&mut boundaries, before_runs);
    append_run_boundaries(&mut boundaries, after_runs);
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut pending: Option<(Range<usize>, RunStyles)> = None;
    for window in boundaries.windows(2) {
      let start = window[0];
      let end = window[1];
      if start == end {
        continue;
      }
      let before_style = run_style_at_byte(before_runs, start);
      let after_style = run_style_at_byte(after_runs, start);
      if before_style == after_style {
        continue;
      }
      match &mut pending {
        Some((range, style)) if *style == after_style && range.end == start => range.end = end,
        Some((range, style)) => {
          operations.push(CanonicalOperation::SetRunStyles {
            paragraph,
            range: range.clone(),
            styles: *style,
          });
          pending = Some((start..end, after_style));
        },
        None => pending = Some((start..end, after_style)),
      }
    }
    if let Some((range, styles)) = pending {
      operations.push(CanonicalOperation::SetRunStyles { paragraph, range, styles });
    }
  }

  fn insert_paragraph_break_at_caret(&mut self, caret: DocumentOffset, _block_ix: usize, cx: &mut Context<Self>) {
    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let before_span = capture_document_span(&self.document, caret.paragraph..caret.paragraph + 1);
    let before_paragraph_id = paragraph_id_at(&self.document, caret.paragraph);
    self.layout_invalidation_hint = Some(caret.paragraph..caret.paragraph + 1);
    self.suppress_mutation_notify += 1;
    self.insert_paragraph_break(cx);
    self.suppress_mutation_notify = self.suppress_mutation_notify.saturating_sub(1);
    self.layout_invalidation_hint = None;
    self.identity_map.reconcile(&self.document);
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
      canonical_operations: match (before_paragraph_id, paragraph_id_at(&self.document, caret.paragraph + 1)) {
        (Some(paragraph), Some(new_paragraph)) => vec![CanonicalOperation::SplitParagraph {
          paragraph,
          byte: caret.byte,
          new_paragraph,
        }],
        _ => vec![CanonicalOperation::ReplaceParagraphSpan {
          start_paragraph: paragraph_id_at(&self.document, caret.paragraph),
          before: before_span,
          after: after_span,
        }],
      },
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
    self.last_collaboration_edit = self
      .undo_stack
      .last()
      .map(|record| CollaborationEdit::from_operations(record.canonical_operations.clone()));
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
    self.identity_map.reconcile(&self.document);
    self.invalidate_document_layout_caches();
    self.refresh_save_status();
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    self.schedule_recovery_write(cx);
    cx.notify();
  }

  fn redo_collaboration_edit_for_history(record: &EditRecord) -> CollaborationEdit {
    Self::collaboration_edit_from_history_operations(record.canonical_operations.clone())
  }

  fn undo_collaboration_edit_for_history(&self, record: &EditRecord) -> CollaborationEdit {
    if let Some(operations) = self.collaboration_undo_operations_from_edit_record(record) {
      return Self::collaboration_edit_from_history_operations(operations);
    }
    let operations = Self::invert_history_canonical_operations(&record.canonical_operations)
      .unwrap_or_else(|| vec![CanonicalOperation::ReplaceDocument]);
    Self::collaboration_edit_from_history_operations(operations)
  }

  fn collaboration_undo_operations_from_edit_record(&self, record: &EditRecord) -> Option<Vec<CanonicalOperation>> {
    let mut operations = Vec::new();
    for operation in record.operations.iter().rev() {
      match operation {
        EditOperation::ReplaceParagraphSpan { before, after } => {
          // History restore has already captured both exact paragraph states.
          // For non-structural edits, derive the inverse CRDT transaction from
          // the current (after) span back to the restored (before) span. The
          // previous implementation only restored styles when text was equal,
          // so undoing deletions/replacements fell through to ReplaceDocument,
          // which intentionally produces no granular collaboration mutations.
          if before.paragraphs.len() != after.paragraphs.len() {
            return None;
          }
          let mut inverse = self.canonical_operations_for_content_replacement(after, before)?;
          operations.append(&mut inverse);
        },
        _ => return None,
      }
    }
    (!operations.is_empty()).then_some(operations)
  }

  fn append_style_restore_operations(
    &self,
    operations: &mut Vec<CanonicalOperation>,
    restore: &DocumentSpan,
    current: &DocumentSpan,
  ) -> Option<()> {
    if restore.paragraphs.len() != current.paragraphs.len() || restore.text != current.text {
      return None;
    }
    for (relative_ix, (restore_paragraph, current_paragraph)) in restore
      .paragraphs
      .iter()
      .zip(&current.paragraphs)
      .enumerate()
    {
      let paragraph = self.identity_map.paragraph_id(restore.start_paragraph + relative_ix)?;
      if restore_paragraph.style != current_paragraph.style {
        operations.push(CanonicalOperation::SetParagraphStyle {
          paragraph,
          style: restore_paragraph.style,
        });
      }
      Self::append_run_style_diff_operations(operations, paragraph, &current_paragraph.runs, &restore_paragraph.runs);
    }
    Some(())
  }

  fn collaboration_edit_from_history_operations(operations: Vec<CanonicalOperation>) -> CollaborationEdit {
    if operations.is_empty() {
      CollaborationEdit::from_operations(vec![CanonicalOperation::ReplaceDocument])
    } else {
      CollaborationEdit::from_operations(operations)
    }
  }

  fn invert_history_canonical_operations(operations: &[CanonicalOperation]) -> Option<Vec<CanonicalOperation>> {
    let mut inverted = Vec::with_capacity(operations.len());
    for operation in operations.iter().rev() {
      inverted.push(Self::invert_history_canonical_operation(operation)?);
    }
    Some(inverted)
  }

  fn invert_history_canonical_operation(operation: &CanonicalOperation) -> Option<CanonicalOperation> {
    match operation {
      CanonicalOperation::InsertText { paragraph, byte, text, .. } => Some(CanonicalOperation::DeleteRange {
        start_paragraph: *paragraph,
        start_byte: *byte,
        end_paragraph: *paragraph,
        end_byte: byte.saturating_add(text.len()),
      }),
      CanonicalOperation::SplitParagraph {
        paragraph, new_paragraph, ..
      } => Some(CanonicalOperation::JoinParagraphs {
        first: *paragraph,
        second: *new_paragraph,
      }),
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => Some(CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph: *start_paragraph,
        before: after.clone(),
        after: before.clone(),
      }),
      CanonicalOperation::DeleteRange { .. }
      | CanonicalOperation::JoinParagraphs { .. }
      | CanonicalOperation::SetParagraphStyle { .. }
      | CanonicalOperation::SetRunStyles { .. }
      | CanonicalOperation::InsertBlock { .. }
      | CanonicalOperation::DeleteBlock { .. }
      | CanonicalOperation::MoveBlock { .. }
      | CanonicalOperation::ReplaceBlock { .. }
      | CanonicalOperation::ReplaceDocument => None,
    }
  }
}

fn append_run_boundaries(boundaries: &mut Vec<usize>, runs: &[TextRun]) {
  let mut offset = 0;
  for run in runs {
    offset += run.len;
    boundaries.push(offset);
  }
}

fn run_style_at_byte(runs: &[TextRun], byte: usize) -> RunStyles {
  let mut offset = 0;
  for run in runs {
    let next = offset + run.len;
    if byte < next {
      return run.styles;
    }
    offset = next;
  }
  RunStyles::default()
}

fn paragraph_text_from_span(span: &DocumentSpan, paragraph_ix: usize) -> Option<String> {
  let mut byte = 0;
  for (ix, paragraph) in span.paragraphs.iter().enumerate() {
    let len = paragraph_text_len(paragraph);
    if ix == paragraph_ix {
      return span.text.get(byte..byte + len).map(str::to_string);
    }
    byte = byte.checked_add(len)?;
    // DocumentSpan::text preserves the document's newline separator between
    // captured paragraphs. Skip that separator before slicing the next
    // paragraph. Omitting it shifted every non-first paragraph one byte left,
    // causing collaboration deletions to target the character to the right.
    byte = byte.checked_add(1)?;
  }
  None
}

fn collaboration_text_window(text: &str, start: usize, end: usize) -> String {
  let mut window_start = start.saturating_sub(32);
  while window_start > 0 && !text.is_char_boundary(window_start) {
    window_start -= 1;
  }
  let mut window_end = end.saturating_add(32).min(text.len());
  while window_end < text.len() && !text.is_char_boundary(window_end) {
    window_end += 1;
  }
  text[window_start..window_end].to_string()
}

#[cfg(test)]
mod edit_pipeline_tests {
  use super::*;

  #[test]
  fn paragraph_text_from_span_skips_inter_paragraph_separators() {
    let document = document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "before".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "Africa War".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "after".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
    );
    let span = capture_document_span(&document, 0..3);

    assert_eq!(paragraph_text_from_span(&span, 0).as_deref(), Some("before"));
    assert_eq!(paragraph_text_from_span(&span, 1).as_deref(), Some("Africa War"));
    assert_eq!(paragraph_text_from_span(&span, 2).as_deref(), Some("after"));
  }

  #[test]
  fn undo_collaboration_edit_inverts_representable_history_operations() {
    let record = EditRecord {
      before_selection: EditorSelection::caret(),
      before_generation: 1,
      after_selection: EditorSelection::caret(),
      after_generation: 2,
      operations: Vec::new(),
      canonical_operations: vec![CanonicalOperation::InsertText {
        paragraph: ParagraphId(7),
        byte: 2,
        text: "abc".to_string(),
        styles: RunStyles::default(),
      }],
    };

    let edit = RichTextEditor::collaboration_edit_from_history_operations(
      RichTextEditor::invert_history_canonical_operations(&record.canonical_operations)
        .unwrap_or_else(|| vec![CanonicalOperation::ReplaceDocument]),
    );

    assert!(matches!(
      edit.operations.as_slice(),
      [CanonicalOperation::DeleteRange {
        start_paragraph: ParagraphId(7),
        start_byte: 2,
        end_paragraph: ParagraphId(7),
        end_byte: 5,
      }]
    ));
  }

  #[test]
  fn redo_collaboration_edit_reuses_record_canonical_operations() {
    let record = EditRecord {
      before_selection: EditorSelection::caret(),
      before_generation: 1,
      after_selection: EditorSelection::caret(),
      after_generation: 2,
      operations: Vec::new(),
      canonical_operations: vec![CanonicalOperation::InsertText {
        paragraph: ParagraphId(7),
        byte: 2,
        text: "abc".to_string(),
        styles: RunStyles::default(),
      }],
    };

    let edit = RichTextEditor::redo_collaboration_edit_for_history(&record);

    assert!(matches!(
      edit.operations.as_slice(),
      [CanonicalOperation::InsertText {
        paragraph: ParagraphId(7),
        byte: 2,
        text,
        styles,
      }] if text == "abc" && *styles == RunStyles::default()
    ));
  }

  #[test]
  fn undo_collaboration_edit_all_operations_are_representable() {
    let record = EditRecord {
      before_selection: EditorSelection::caret(),
      before_generation: 1,
      after_selection: EditorSelection::caret(),
      after_generation: 2,
      operations: Vec::new(),
      canonical_operations: vec![CanonicalOperation::ReplaceDocument],
    };

    let edit = RichTextEditor::collaboration_edit_from_history_operations(
      RichTextEditor::invert_history_canonical_operations(&record.canonical_operations)
        .unwrap_or_else(|| vec![CanonicalOperation::ReplaceDocument]),
    );

    assert!(matches!(edit.operations.as_slice(), [CanonicalOperation::ReplaceDocument]));
  }
}
