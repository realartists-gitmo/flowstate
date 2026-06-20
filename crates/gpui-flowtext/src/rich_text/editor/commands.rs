#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RichTextEditorCommand {
  MoveLeft,
  MoveRight,
  MoveUp,
  MoveDown,
  MoveLineStart,
  MoveLineEnd,
  SelectLeft,
  SelectRight,
  SelectUp,
  SelectDown,
  SelectLineStart,
  SelectLineEnd,
  SelectAll,
  MoveWordLeft,
  MoveWordRight,
  SelectWordLeft,
  SelectWordRight,
  DeleteWordBackward,
  DeleteWordForward,
  PageUp,
  PageDown,
  SelectPageUp,
  SelectPageDown,
  MoveDocumentStart,
  MoveDocumentEnd,
  SelectDocumentStart,
  SelectDocumentEnd,
  Copy,
  Cut,
  Paste,
  Undo,
  Redo,
  SetParagraphStyle(u8),
  ToggleSemanticStyle(u8),
  ToggleUnderline,
  ToggleStrikethrough,
  SetHighlightStyle(u8),
  ApplyHighlightToSelection,
  ClearFormatting,
  ClearHighlight,
  InsertImage,
  InsertTable,
  InsertEquation,
  ZoomIn,
  ZoomOut,
  Backspace,
  Delete,
  InsertNewline,
  InsertSoftLineBreak,
}

fn rich_text_mutation_command(command: RichTextEditorCommand) -> bool {
  #[allow(clippy::enum_glob_use, reason = "matches all command variants in a dispatch table")]
  use RichTextEditorCommand::*;
  matches!(
    command,
    DeleteWordBackward
      | DeleteWordForward
      | Cut
      | Paste
      | Undo
      | Redo
      | SetParagraphStyle(_)
      | ToggleSemanticStyle(_)
      | ToggleUnderline
      | ToggleStrikethrough
      | SetHighlightStyle(_)
      | ApplyHighlightToSelection
      | ClearFormatting
      | ClearHighlight
      | InsertImage
      | InsertTable
      | InsertEquation
      | Backspace
      | Delete
      | InsertNewline
      | InsertSoftLineBreak
  )
}

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn dispatch_window_command(&mut self, command: RichTextEditorCommand, window: &mut Window, cx: &mut Context<Self>) {
    if !self.can_write_collaboration() && rich_text_mutation_command(command) {
      cx.notify();
      return;
    }
    #[allow(clippy::enum_glob_use, reason = "matches all command variants in a dispatch table")]
    use RichTextEditorCommand::*;

    match command {
      MoveLeft => self.move_left(window, cx),
      MoveRight => self.move_right(window, cx),
      MoveUp => self.move_up(window, cx),
      MoveDown => self.move_down(window, cx),
      MoveLineStart => self.move_line_start(cx),
      MoveLineEnd => self.move_line_end(cx),
      SelectLeft => self.select_left(window, cx),
      SelectRight => self.select_right(window, cx),
      SelectUp => self.select_up(window, cx),
      SelectDown => self.select_down(window, cx),
      SelectLineStart => self.select_line_start(cx),
      SelectLineEnd => self.select_line_end(cx),
      SelectAll => self.select_all(cx),
      MoveWordLeft => self.move_word_left(cx),
      MoveWordRight => self.move_word_right(cx),
      SelectWordLeft => self.select_word_left(cx),
      SelectWordRight => self.select_word_right(cx),
      DeleteWordBackward => self.delete_word_backward_command(cx),
      DeleteWordForward => self.delete_word_forward_command(cx),
      PageUp => self.page_up(cx),
      PageDown => self.page_down(cx),
      SelectPageUp => self.select_page_up(cx),
      SelectPageDown => self.select_page_down(cx),
      MoveDocumentStart => self.move_document_start(cx),
      MoveDocumentEnd => self.move_document_end(cx),
      SelectDocumentStart => self.select_document_start(cx),
      SelectDocumentEnd => self.select_document_end(cx),
      Copy => self.copy(cx),
      Cut => self.cut(cx),
      Paste => self.paste(cx),
      Undo => self.undo(cx),
      Redo => self.redo(cx),
      SetParagraphStyle(slot) => self.set_paragraph_style_for_selection(ParagraphStyle::Custom(slot), cx),
      ToggleSemanticStyle(slot) => self.toggle_semantic_style_for_selection(RunSemanticStyle::Custom(slot), cx),
      ToggleUnderline => self.toggle_underline(cx),
      ToggleStrikethrough => self.toggle_strikethrough(cx),
      SetHighlightStyle(slot) => self.set_highlight(HighlightStyle::Custom(slot), cx),
      ApplyHighlightToSelection => self.apply_current_highlight_to_selection(cx),
      ClearFormatting => self.clear_formatting(cx),
      ClearHighlight => self.clear_highlight(cx),
      InsertImage => self.prompt_insert_image(cx),
      InsertTable => self.insert_default_table(2, 2, cx),
      InsertEquation => self.insert_equation("x^2 + y^2 = z^2", cx),
      ZoomIn => self.zoom_in(cx),
      ZoomOut => self.zoom_out(cx),
      Backspace => self.backspace_command(cx),
      Delete => self.delete_forward_command(cx),
      InsertNewline => {
        if !self.split_selected_table_cell_paragraph(cx) {
          self.insert_paragraph_break_command(cx);
        }
      },
      InsertSoftLineBreak => {
        if self.insert_text_into_selected_table_cell(SOFT_LINE_BREAK_STR, cx) {
          return;
        }
        if self.insert_text_into_selected_equation(SOFT_LINE_BREAK_STR, cx) {
          return;
        }
        self.insert_text_command(SOFT_LINE_BREAK_STR, cx);
      },
    }
  }

  pub fn scroll_to_paragraph(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if paragraph_ix < self.document.paragraphs.len() {
      // Outline navigation should place the insertion caret at the start of
      // the target paragraph, matching what the user just selected in the nav.
      let before_selection = self.selection.clone();
      self.selection = EditorSelection {
        anchor: DocumentOffset {
          paragraph: paragraph_ix,
          byte: 0,
        },
        head: DocumentOffset {
          paragraph: paragraph_ix,
          byte: 0,
        },
      };
      self.goal_x = None;
      self.reset_caret_blink(cx);
      if self.selection != before_selection {
        self.emit_selection_changed(cx);
      }

      let width = self.current_layout_width();
      let start = paragraph_ix.saturating_sub(2);
      let end = (paragraph_ix + 6).min(self.document.paragraphs.len());
      for ix in start..end {
        self.ensure_next_paragraph_chunk(ix, width, window, cx);
      }
      let target_anchor = self.paragraph_start_anchor(paragraph_ix);
      self.item_sizes_cache = None;
      let _ = self.rebuild_item_sizes_cache(width, target_anchor.clone(), window, cx);
      let _ = self.materialize_visible_remainders_for_scroll(width, target_anchor.clone(), window, cx);
      self.restore_scroll_anchor(target_anchor);
      self.pending_snap_to_paragraph = None;
      cx.notify();
    }
  }

  pub fn undo(&mut self, cx: &mut Context<Self>) {
    if let Some(hook) = self.native_undo_hook.clone() {
      let pending_edits = self.take_pending_semantic_edits();
      let assets = self.document.assets.assets.values().cloned().collect();
      let fallback_selection = self.selection.clone();
      let generation = self.next_edit_generation;
      self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
      cx.spawn(async move |editor, cx| {
        let result = hook(UndoRedirect::Undo, pending_edits, assets).await;
        let _ = editor.update(cx, |editor, cx| match result {
          Ok(Some(result)) => {
            editor.document = result.document;
            editor.identity_map.reconcile(&editor.document);
            editor.selection = result.selection.unwrap_or(fallback_selection);
            editor.emit_selection_changed(cx);
            editor.edit_generation = generation;
            editor.undo_stack.clear();
            editor.redo_stack.clear();
            editor.after_history_restore(cx);
          },
          Ok(None) => {},
          Err(error) => {
            eprintln!("runtime undo failed: {error}");
          },
        });
      })
      .detach();
      return;
    }
    if let Some(hook) = self.session_undo_redirect.clone() {
      hook(UndoRedirect::Undo);
      return;
    }
    let Some(record) = self.undo_stack.pop() else {
      return;
    };
    let restored_generation = record.before_generation;
    for operation in record.operations.iter().rev() {
      operation.undo(&mut self.document);
    }
    self.selection = record.before_selection.clone();
    self.emit_selection_changed(cx);
    self.edit_generation = restored_generation;
    self.redo_stack.push(record);
    self.after_history_restore(cx);
  }

  pub fn redo(&mut self, cx: &mut Context<Self>) {
    if let Some(hook) = self.native_undo_hook.clone() {
      let pending_edits = self.take_pending_semantic_edits();
      let assets = self.document.assets.assets.values().cloned().collect();
      let fallback_selection = self.selection.clone();
      let generation = self.next_edit_generation;
      self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
      cx.spawn(async move |editor, cx| {
        let result = hook(UndoRedirect::Redo, pending_edits, assets).await;
        let _ = editor.update(cx, |editor, cx| match result {
          Ok(Some(result)) => {
            editor.document = result.document;
            editor.identity_map.reconcile(&editor.document);
            editor.selection = result.selection.unwrap_or(fallback_selection);
            editor.emit_selection_changed(cx);
            editor.edit_generation = generation;
            editor.undo_stack.clear();
            editor.redo_stack.clear();
            editor.after_history_restore(cx);
          },
          Ok(None) => {},
          Err(error) => {
            eprintln!("runtime redo failed: {error}");
          },
        });
      })
      .detach();
      return;
    }
    if let Some(hook) = self.session_undo_redirect.clone() {
      hook(UndoRedirect::Redo);
      return;
    }
    let Some(record) = self.redo_stack.pop() else {
      return;
    };
    let restored_generation = record.after_generation;
    for operation in &record.operations {
      operation.redo(&mut self.document);
    }
    self.selection = record.after_selection.clone();
    self.emit_selection_changed(cx);
    self.edit_generation = restored_generation;
    self.undo_stack.push(record);
    self.after_history_restore(cx);
  }

  pub fn can_undo(&self) -> bool {
    self.native_undo_hook.is_some() || self.session_undo_redirect.is_some() || !self.undo_stack.is_empty()
  }

  pub fn can_redo(&self) -> bool {
    self.native_undo_hook.is_some() || self.session_undo_redirect.is_some() || !self.redo_stack.is_empty()
  }

  pub fn move_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Left, false, window, cx);
  }

  pub fn move_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Right, false, window, cx);
  }

  pub fn move_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Up, false, window, cx);
  }

  pub fn move_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Down, false, window, cx);
  }

  pub fn move_line_start(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(true, false, cx);
  }

  pub fn move_line_end(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(false, false, cx);
  }

  pub fn select_left(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Left, true, window, cx);
  }

  pub fn select_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Right, true, window, cx);
  }

  pub fn select_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Up, true, window, cx);
  }

  pub fn select_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Down, true, window, cx);
  }

  pub fn select_line_start(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(true, true, cx);
  }

  pub fn select_line_end(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(false, true, cx);
  }

  pub fn select_all(&mut self, cx: &mut Context<Self>) {
    if let Some(BlockSelection::Equation(_)) = self.selected_block {
      if let Some(source) = self.selected_equation_source() {
        self.equation_source_anchor = 0;
        self.equation_source_caret = source.len();
        self.goal_x = None;
        self.reset_caret_blink(cx);
        cx.notify();
      }
      return;
    }
    if self.document.paragraphs.is_empty() {
      return;
    }
    let last = self.document.paragraphs.len() - 1;
    let last_len = paragraph_text_len(&self.document.paragraphs[last]);
    let selection = EditorSelection {
      anchor: DocumentOffset { paragraph: 0, byte: 0 },
      head: DocumentOffset {
        paragraph: last,
        byte: last_len,
      },
    };
    if self.selection == selection {
      self.goal_x = None;
      return;
    }
    self.selection = selection;
    self.goal_x = None;
    self.reset_caret_blink(cx);
    self.emit_selection_changed(cx);
    cx.notify();
  }

  pub fn move_word_left(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_left(self.selection.head), false, cx);
  }

  pub fn move_word_right(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_right(self.selection.head), false, cx);
  }

  pub fn select_word_left(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_left(self.selection.head), true, cx);
  }

  pub fn select_word_right(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_right(self.selection.head), true, cx);
  }

  pub fn page_up(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Up, false, cx);
  }

  pub fn page_down(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Down, false, cx);
  }

  pub fn select_page_up(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Up, true, cx);
  }

  pub fn select_page_down(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Down, true, cx);
  }

  pub fn move_document_start(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(DocumentOffset::default(), false, cx);
  }

  pub fn move_document_end(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(document_end(&self.document), false, cx);
  }

  pub fn select_document_start(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(DocumentOffset::default(), true, cx);
  }

  pub fn select_document_end(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(document_end(&self.document), true, cx);
  }

  pub fn insert_text_command(&mut self, text: &str, cx: &mut Context<Self>) {
    if !self.can_write_collaboration() {
      cx.notify();
      return;
    }
    let before = self.selection.clone();
    clamp_selection_to_document(&self.document, &mut self.selection);
    if self.selection != before {
      self.emit_selection_changed(cx);
    }
    if self.insert_single_grapheme_fast_path(text, cx) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.insert_text(text, cx));
  }

  pub fn backspace_command(&mut self, cx: &mut Context<Self>) {
    if !self.can_write_collaboration() {
      cx.notify();
      return;
    }
    if !self.selection.is_caret() && self.selection_crosses_object_blocks(self.selection.normalized()) {
      let _ = self.delete_selection_with_document_snapshot(cx);
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.backspace(cx));
  }

  pub fn delete_forward_command(&mut self, cx: &mut Context<Self>) {
    if !self.can_write_collaboration() {
      cx.notify();
      return;
    }
    if !self.selection.is_caret() && self.selection_crosses_object_blocks(self.selection.normalized()) {
      let _ = self.delete_selection_with_document_snapshot(cx);
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.delete_forward(cx));
  }

  pub fn insert_paragraph_break_command(&mut self, cx: &mut Context<Self>) {
    if !self.selection.is_caret() {
      self.apply_document_edit(cx, |editor, cx| editor.insert_paragraph_break(cx));
      return;
    }

    let caret = self.selection.head;
    let Some(block_ix) = block_ix_for_paragraph(&self.document, caret.paragraph) else {
      self.apply_document_edit(cx, |editor, cx| editor.insert_paragraph_break(cx));
      return;
    };
    self.insert_paragraph_break_at_caret(caret, block_ix, cx);
  }

  pub fn delete_word_backward_command(&mut self, cx: &mut Context<Self>) {
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let head = editor.selection.head;
        let anchor = editor.word_left(head);
        editor.selection = EditorSelection { anchor, head };
      }
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn delete_word_forward_command(&mut self, cx: &mut Context<Self>) {
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let anchor = editor.selection.head;
        let head = editor.word_right(anchor);
        editor.selection = EditorSelection { anchor, head };
      }
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn copy(&mut self, cx: &mut Context<Self>) {
    if let Some(text) = self.selected_equation_source_text() {
      cx.write_to_clipboard(ClipboardItem::new_string(text));
      self.paste_cache = None;
      return;
    }
    if let Some(fragment) = self.selected_table_cell_fragment() {
      let text = block_fragment_plain_text(&fragment);
      cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
      self.paste_cache = None;
      return;
    }
    if let Some(fragment) = self.selected_block_fragment() {
      let text = block_fragment_plain_text(&fragment);
      cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
      self.paste_cache = None;
      return;
    }
    if self.selection.is_caret() {
      return;
    }
    if let Some(fragment) = self.selected_ordered_fragment(self.selection.normalized()) {
      let text = block_fragment_plain_text(&fragment);
      cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
      self.paste_cache = None;
      return;
    }
    let text = selected_plain_text(&self.document, self.selection.normalized());
    let fragment = selected_rich_fragment(&self.document, self.selection.normalized());
    cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
    self.paste_cache = None;
  }

  pub fn cut(&mut self, cx: &mut Context<Self>) {
    self.copy(cx);
    if self.clear_selected_table_cell(cx) {
      return;
    }
    if self.selected_block.is_some() {
      self.apply_document_edit(cx, |editor, cx| {
        let _ = editor.delete_selected_block(cx);
      });
      return;
    }
    if self.delete_selection_with_document_snapshot(cx) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn paste(&mut self, cx: &mut Context<Self>) {
    let Some(item) = cx.read_from_clipboard() else {
      return;
    };
    if matches!(self.selected_block, Some(BlockSelection::Equation(_))) {
      if let Some(text) = item.text() {
        self.insert_text_into_selected_equation(&text, cx);
      }
      return;
    }
    if let Some(image) = item.entries().iter().find_map(|entry| match entry {
      ClipboardEntry::Image(image) => Some(image.clone()),
      ClipboardEntry::String(_) => None,
    }) {
      self.insert_clipboard_image(image, cx);
      return;
    }
    if let Some(metadata) = item.metadata() {
      if let Some(PasteCache::Rich {
        metadata: cached_metadata,
        fragment,
      }) = &self.paste_cache
        && cached_metadata == metadata
      {
        let fragment = fragment.clone();
        if self.insert_rich_fragment_into_selected_table_cell(&fragment, cx) {
          return;
        }
        if self.insert_rich_fragment_paste_at_caret(&fragment, cx) {
          return;
        }
        if fragment.blocks.is_empty() {
          self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
        } else {
          self.insert_rich_fragment(fragment, cx);
        }
        return;
      }
      if let Some(fragment) = serde_json::from_str::<RichClipboardFragment>(metadata)
        .ok()
        .filter(|fragment| rich_text_clipboard_format_is_supported(&fragment.format))
      {
        self.paste_cache = Some(PasteCache::Rich {
          metadata: metadata.to_string(),
          fragment: fragment.clone(),
        });
        if self.insert_rich_fragment_into_selected_table_cell(&fragment, cx) {
          return;
        }
        if self.insert_rich_fragment_paste_at_caret(&fragment, cx) {
          return;
        }
        if fragment.blocks.is_empty() {
          self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
        } else {
          self.insert_rich_fragment(fragment, cx);
        }
        return;
      }
    }
    if let Some(text) = item.text() {
      if let Some(PasteCache::Plain { text: cached_text }) = &self.paste_cache
        && cached_text == &text
      {
        let text = cached_text.clone();
        if self.insert_plain_text_into_selected_table_cell(&text, cx) {
          return;
        }
        if self.insert_plain_text_paste_at_caret(&text, cx) {
          return;
        }
        self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
        return;
      }
      self.paste_cache = Some(PasteCache::Plain { text: text.clone() });
      if self.insert_plain_text_into_selected_table_cell(&text, cx) {
        return;
      }
      if self.insert_plain_text_paste_at_caret(&text, cx) {
        return;
      }
      self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
    }
  }

  pub fn insert_plain_text_from_toolkit(&mut self, text: &str, cx: &mut Context<Self>) {
    if text.trim().is_empty() {
      return;
    }
    if self.insert_text_into_selected_equation(text, cx) {
      return;
    }
    if self.insert_plain_text_into_selected_table_cell(text, cx) {
      return;
    }
    if self.insert_plain_text_paste_at_caret(text, cx) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(text, cx));
  }

  fn on_toolkit_text_drop(&mut self, drag: &ToolkitTextDrag, _: &mut Window, cx: &mut Context<Self>) {
    self.clear_drop_preview();
    self.insert_toolkit_paragraphs_as_blocks(drag.paragraphs.clone(), cx);
  }
}
