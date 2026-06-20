#[hotpath::measure_all]
impl RichTextEditor {
  fn backspace_pending_runtime_command(&mut self, cx: &mut Context<Self>) -> bool {
    if self.suppress_collab_capture != 0 || !(self.collab_capture || self.runtime_capture) || !self.selection.is_caret() {
      return false;
    }
    let Some(pending_selection) = self.pending_command_selection.clone() else {
      return false;
    };
    if pending_selection == self.selection || !pending_selection.is_caret() {
      return false;
    }
    let caret = pending_selection.head;
    let Some((insert_at, inserted)) = self.undo_stack.iter().rev().find_map(|record| {
      record.semantic_commands.iter().rev().find_map(|command| match command {
        SemanticEditCommand::InsertText { at, text, .. } if at.paragraph == caret.paragraph => {
          let relative = caret.byte.checked_sub(at.byte)?;
          (relative > 0 && relative <= text.len() && text.is_char_boundary(relative)).then(|| (*at, &text[..relative]))
        },
        _ => None,
      })
    }) else {
      return false;
    };
    let Some((last_grapheme_byte, _)) = inserted.grapheme_indices(true).next_back() else {
      return false;
    };
    let delete_start = DocumentOffset {
      paragraph: caret.paragraph,
      byte: insert_at.byte.saturating_add(last_grapheme_byte),
    };
    let after_selection = EditorSelection {
      anchor: delete_start,
      head: delete_start,
    };
    let command = SemanticEditCommand::DeleteRange {
      range: delete_start..caret,
    };
    let edit = CollaborationEdit {
      semantic_commands: vec![command.clone()],
      selection_after: Some(after_selection.clone()),
    };
    if self.collab_capture {
      self.pending_collab_edits.push(edit.clone());
    }
    if self.runtime_capture {
      self.pending_runtime_edits.push(edit);
    }
    let before_generation = self.next_edit_generation.saturating_sub(1);
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: pending_selection,
      before_generation,
      after_selection: after_selection.clone(),
      after_generation,
      operations: Vec::new(),
      semantic_commands: vec![command],
    });
    self.redo_stack.clear();
    self.pending_command_selection = Some(after_selection);
    cx.notify();
    true
  }

  pub(super) fn insert_single_grapheme_fast_path(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    if !is_single_grapheme_text_insert(text) || !self.selection.is_caret() || self.selected_block.is_some() {
      return false;
    }
    let runtime_owned = self.suppress_collab_capture == 0 && (self.collab_capture || self.runtime_capture);
    let caret = self
      .pending_command_selection
      .as_ref()
      .map_or(self.selection.head, |selection| selection.head);
    let Some(paragraph) = self.document.paragraphs.get(caret.paragraph) else {
      return false;
    };
    if self.invisibility_mode && matches!(paragraph.style, ParagraphStyle::Normal) {
      return false;
    }
    if !runtime_owned && caret.byte > paragraph_text_len(paragraph) {
      return false;
    }
    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let styles = if let Some(styles) = self.pending_styles {
      styles
    } else {
      let (run_ix, _) = run_containing(paragraph, caret.byte.min(paragraph_text_len(paragraph)));
      paragraph
        .runs
        .get(run_ix)
        .map(|run| run.styles)
        .unwrap_or_default()
    };

    let after = DocumentOffset {
      paragraph: caret.paragraph,
      byte: caret.byte + text.len(),
    };
    let after_selection = EditorSelection { anchor: after, head: after };
    if runtime_owned {
      let merge_with_pending = (self.collab_capture && !self.pending_collab_edits.is_empty())
        || (self.runtime_capture && !self.pending_runtime_edits.is_empty());
      let command = SemanticEditCommand::InsertText {
        at: caret,
        text: text.to_string(),
        styles,
      };
      let edit = CollaborationEdit {
        semantic_commands: vec![command.clone()],
        selection_after: Some(after_selection.clone()),
      };
      if self.collab_capture {
        self.pending_collab_edits.push(edit.clone());
      }
      if self.runtime_capture {
        self.pending_runtime_edits.push(edit);
      }
      self.pending_command_selection = Some(after_selection.clone());
      if merge_with_pending
        && let Some(record) = self.undo_stack.last_mut()
        && let Some(SemanticEditCommand::InsertText {
          at,
          text: previous_text,
          styles: previous_styles,
        }) = record.semantic_commands.last_mut()
        && at.paragraph == caret.paragraph
        && *previous_styles == styles
        && at.byte + previous_text.len() == caret.byte
      {
        previous_text.push_str(text);
        record.after_selection = after_selection;
        record.after_generation = after_generation;
        self.redo_stack.clear();
        cx.notify();
        return true;
      }
      self.undo_stack.push(EditRecord {
        before_selection,
        before_generation,
        after_selection,
        after_generation,
        operations: Vec::new(),
        semantic_commands: vec![command],
      });
      self.redo_stack.clear();
      cx.notify();
      return true;
    }

    insert_text_at(&mut self.document, caret.paragraph, caret.byte, text, styles);
    self.selection = EditorSelection { anchor: after, head: after };
    self.emit_selection_changed(cx);

    let mut merged_into_previous = false;
    if let Some(record) = self.undo_stack.last_mut()
      && before_selection.anchor == before_selection.head
      && record.after_selection == before_selection
      && record.operations.len() == 1
      && record.semantic_commands.len() == 1
      && let EditOperation::InsertText {
        paragraph,
        byte,
        text: previous_text,
        styles: previous_styles,
      } = &mut record.operations[0]
      && *paragraph == caret.paragraph
      && *previous_styles == styles
      && *byte + previous_text.len() == caret.byte
      && let SemanticEditCommand::InsertText {
        at,
        text: semantic_text,
        styles: semantic_styles,
      } = &mut record.semantic_commands[0]
      && at.paragraph == caret.paragraph
      && *semantic_styles == styles
      && at.byte + semantic_text.len() == caret.byte
    {
      previous_text.push_str(text);
      semantic_text.push_str(text);
      record.after_selection = self.selection.clone();
      record.after_generation = after_generation;
      merged_into_previous = true;
    }

    if !merged_into_previous {
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
        semantic_commands: vec![SemanticEditCommand::InsertText {
          at: caret,
          text: text.to_string(),
          styles,
        }],
      });
    }
    if self.collab_capture && self.suppress_collab_capture == 0 {
      self.pending_collab_edits.push(CollaborationEdit {
        semantic_commands: vec![SemanticEditCommand::InsertText {
          at: caret,
          text: text.to_string(),
          styles,
        }],
        selection_after: Some(self.selection.clone()),
      });
    }
    if self.runtime_capture && self.suppress_collab_capture == 0 {
      self.pending_runtime_edits.push(CollaborationEdit {
        semantic_commands: vec![SemanticEditCommand::InsertText {
          at: caret,
          text: text.to_string(),
          styles,
        }],
        selection_after: Some(self.selection.clone()),
      });
    }
    self.redo_stack.clear();
    self.layout_invalidation_hint = Some(caret.paragraph..caret.paragraph + 1);
    self.suppress_mutation_notify += 1;
    self.after_text_mutation(cx);
    self.suppress_mutation_notify = self.suppress_mutation_notify.saturating_sub(1);
    self.mark_document_changed_with_ops(after_generation, false, None, cx);
    true
  }

  fn apply_document_edit_with_capture_range(
    &mut self,
    cx: &mut Context<Self>,
    capture_range: Option<Range<usize>>,
    edit: impl FnOnce(&mut Self, &mut Context<Self>),
  ) {
    let timing = Instant::now();
    let runtime_rollback = (self.suppress_collab_capture == 0 && (self.collab_capture || self.runtime_capture))
      .then(|| self.document.clone());
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
    self.finish_document_edit(
      before_span,
      before_selection,
      before_block_count,
      after_span,
      runtime_rollback,
      cx,
    );
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
    runtime_rollback: Option<DocumentProjection>,
    cx: &mut Context<Self>,
  ) {
    if before_span == after_span && before_selection == self.selection {
      return;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = self.semantic_commands_for_span_edit(&before_span, &after_span);
    let identity_shape_changed = before_span.paragraphs.len() != after_span.paragraphs.len() || before_block_count != self.document.blocks.len();
    if runtime_rollback.is_some() {
      self.pending_projection_rollback = runtime_rollback;
    }
    let record = EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: if self.pending_projection_rollback.is_some() {
        Vec::new()
      } else {
        vec![EditOperation::ReplaceParagraphSpan {
          before: before_span,
          after: after_span,
        }]
      },
      semantic_commands: semantic_commands.clone(),
    };
    self.undo_stack.push(record);
    self.redo_stack.clear();
    self.mark_document_changed_with_ops(after_generation, identity_shape_changed, Some(&semantic_commands), cx);
  }

  fn semantic_commands_for_span_edit(&self, before: &DocumentSpan, after: &DocumentSpan) -> Vec<SemanticEditCommand> {
    if before.paragraphs.len() != after.paragraphs.len() || before.start_paragraph != after.start_paragraph {
      return vec![SemanticEditCommand::ReplaceParagraphSpan {
        start: Some(DocumentOffset {
          paragraph: before.start_paragraph,
          byte: 0,
        }),
        before: before.clone(),
        after: after.clone(),
      }];
    }

    let before_texts = span_paragraph_texts_for_commands(before);
    let after_texts = span_paragraph_texts_for_commands(after);
    let mut commands = Vec::new();
    for paragraph_offset in 0..before.paragraphs.len() {
      let paragraph_ix = before.start_paragraph + paragraph_offset;
      let Some(paragraph_id) = self.identity_map.paragraph_id(paragraph_ix) else {
        return vec![SemanticEditCommand::ReplaceParagraphSpan {
          start: Some(DocumentOffset {
            paragraph: before.start_paragraph,
            byte: 0,
          }),
          before: before.clone(),
          after: after.clone(),
        }];
      };
      let before_paragraph = &before.paragraphs[paragraph_offset];
      let after_paragraph = &after.paragraphs[paragraph_offset];
      let before_text = &before_texts[paragraph_offset];
      let after_text = &after_texts[paragraph_offset];

      if before_text != after_text {
        let prefix = common_prefix_bytes(before_text, after_text);
        let suffix = common_suffix_bytes(before_text, after_text, prefix);
        let before_end = before_text.len().saturating_sub(suffix);
        let after_end = after_text.len().saturating_sub(suffix);
        if before_end > prefix {
          commands.push(SemanticEditCommand::DeleteRange {
            range: DocumentOffset {
              paragraph: paragraph_ix,
              byte: prefix,
            }..DocumentOffset {
              paragraph: paragraph_ix,
              byte: before_end,
            },
          });
        }
        let mut inserted = 0usize;
        for (text, styles) in input_segments_for_range(after_paragraph, after_text, prefix..after_end) {
          commands.push(SemanticEditCommand::InsertText {
            at: DocumentOffset {
              paragraph: paragraph_ix,
              byte: prefix + inserted,
            },
            text: text.clone(),
            styles,
          });
          inserted += text.len();
        }
      }

      if before_paragraph.style != after_paragraph.style {
        commands.push(SemanticEditCommand::SetParagraphStyle {
          paragraph: paragraph_id,
          style: after_paragraph.style,
        });
      }
      if before_paragraph.runs != after_paragraph.runs {
        let mut byte = 0usize;
        for run in &after_paragraph.runs {
          let end = byte.saturating_add(run.len).min(after_text.len());
          if end > byte {
            commands.push(SemanticEditCommand::SetRunStyles {
              paragraph: paragraph_id,
              range: byte..end,
              styles: run.styles,
            });
          }
          byte = end;
        }
      }
    }
    commands
  }

  fn insert_paragraph_break_at_caret(&mut self, caret: DocumentOffset, _block_ix: usize, cx: &mut Context<Self>) {
    let runtime_rollback = (self.suppress_collab_capture == 0 && (self.collab_capture || self.runtime_capture))
      .then(|| self.document.clone());
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
    self.identity_map.reconcile(&self.document);
    let after_span = capture_document_span(&self.document, caret.paragraph..caret.paragraph + 2);

    if before_span == after_span && before_selection == self.selection {
      return;
    }

    let inherited_style = before_span
      .paragraphs
      .first()
      .map(|paragraph| {
        if caret.byte >= paragraph_text_len(paragraph) {
          ParagraphStyle::Normal
        } else {
          paragraph.style
        }
      })
      .unwrap_or(ParagraphStyle::Normal);
    let semantic_commands = vec![SemanticEditCommand::SplitParagraph {
      at: caret,
      inherited_style,
    }];
    if runtime_rollback.is_some() {
      self.pending_projection_rollback = runtime_rollback;
    }
    let record = EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: if self.pending_projection_rollback.is_some() {
        Vec::new()
      } else {
        vec![EditOperation::ReplaceParagraphSpan {
          before: before_span.clone(),
          after: after_span.clone(),
        }]
      },
      semantic_commands: semantic_commands.clone(),
    };
    self.undo_stack.push(record);
    self.redo_stack.clear();
    self.mark_document_changed_with_ops(after_generation, false, Some(&semantic_commands), cx);
  }

  fn mark_document_changed_with_ops(
    &mut self,
    generation: u64,
    reconcile_identity: bool,
    semantic_commands: Option<&[SemanticEditCommand]>,
    cx: &mut Context<Self>,
  ) {
    let runtime_owned = self.suppress_collab_capture == 0
      && semantic_commands.is_some_and(|commands| !commands.is_empty())
      && (self.collab_capture || self.runtime_capture);
    if runtime_owned {
      let selection_after = self.selection.clone();
      let edit = CollaborationEdit {
        semantic_commands: semantic_commands.unwrap_or_default().to_vec(),
        selection_after: Some(selection_after.clone()),
      };
      if self.collab_capture {
        self.pending_collab_edits.push(edit.clone());
      }
      if self.runtime_capture {
        self.pending_runtime_edits.push(edit);
      }
      self.pending_command_selection = Some(selection_after);
      if let Some(before_document) = self.pending_projection_rollback.take() {
        self.document = before_document;
        self.selection = self
          .undo_stack
          .last()
          .map_or_else(EditorSelection::caret, |record| record.before_selection.clone());
        self.edit_generation = self
          .undo_stack
          .last()
          .map_or(self.edit_generation, |record| record.before_generation);
      } else if let Some(record) = self.undo_stack.last_mut() {
        for operation in record.operations.iter().rev() {
          operation.undo(&mut self.document);
        }
        self.selection = record.before_selection.clone();
        self.edit_generation = record.before_generation;
        record.operations.clear();
      }
      self.identity_map.reconcile(&self.document);
      self.invalidate_document_layout_caches();
      self.emit_selection_changed(cx);
      cx.notify();
      return;
    }

    self.edit_generation = generation;
    if reconcile_identity {
      self.identity_map.reconcile(&self.document);
    }
    if self.collab_capture
      && self.suppress_collab_capture == 0
      && let Some(semantic_commands) = semantic_commands
      && !semantic_commands.is_empty()
    {
      self.pending_collab_edits.push(CollaborationEdit {
        semantic_commands: semantic_commands.to_vec(),
        selection_after: Some(self.selection.clone()),
      });
    }
    if self.runtime_capture
      && self.suppress_collab_capture == 0
      && let Some(semantic_commands) = semantic_commands
      && !semantic_commands.is_empty()
    {
      self.pending_runtime_edits.push(CollaborationEdit {
        semantic_commands: semantic_commands.to_vec(),
        selection_after: Some(self.selection.clone()),
      });
    }
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

fn span_paragraph_texts_for_commands(span: &DocumentSpan) -> Vec<String> {
  let mut offset = 0usize;
  span
    .paragraphs
    .iter()
    .enumerate()
    .map(|(paragraph_ix, paragraph)| {
      if paragraph_ix > 0 && span.text.get(offset..).is_some_and(|text| text.starts_with('\n')) {
        offset += 1;
      }
      let len = paragraph_text_len(paragraph);
      let end = offset.saturating_add(len).min(span.text.len());
      let text = span.text.get(offset..end).unwrap_or_default().to_string();
      offset = end;
      text
    })
    .collect()
}

fn common_prefix_bytes(left: &str, right: &str) -> usize {
  let mut len = 0usize;
  for ((left_ix, left_ch), (_, right_ch)) in left.char_indices().zip(right.char_indices()) {
    if left_ch != right_ch {
      break;
    }
    len = left_ix + left_ch.len_utf8();
  }
  len
}

fn common_suffix_bytes(left: &str, right: &str, prefix: usize) -> usize {
  let mut len = 0usize;
  for ((left_ix, left_ch), (right_ix, right_ch)) in left.char_indices().rev().zip(right.char_indices().rev()) {
    if left_ix < prefix || right_ix < prefix || left_ch != right_ch {
      break;
    }
    len += left_ch.len_utf8();
  }
  len
}

fn input_segments_for_range(
  paragraph: &Paragraph,
  text: &str,
  range: Range<usize>,
) -> Vec<(String, RunStyles)> {
  if range.is_empty() {
    return Vec::new();
  }
  let mut segments = Vec::new();
  let mut byte = 0usize;
  for run in &paragraph.runs {
    let run_start = byte;
    let run_end = byte.saturating_add(run.len).min(text.len());
    byte = run_end;
    let start = run_start.max(range.start);
    let end = run_end.min(range.end);
    if start < end
      && let Some(segment) = text.get(start..end)
      && !segment.is_empty()
    {
      segments.push((segment.to_string(), run.styles));
    }
  }
  if segments.is_empty()
    && let Some(segment) = text.get(range)
    && !segment.is_empty()
  {
    segments.push((segment.to_string(), RunStyles::default()));
  }
  segments
}
