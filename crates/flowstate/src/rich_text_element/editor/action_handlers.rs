#[hotpath::measure_all]
impl RichTextEditor {
  fn on_move_left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
    self.move_left(window, cx);
  }
  fn on_move_right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
    self.move_right(window, cx);
  }
  fn on_move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
    self.move_up(window, cx);
  }
  fn on_move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
    self.move_down(window, cx);
  }
  fn on_move_line_start(&mut self, _: &MoveLineStart, _: &mut Window, cx: &mut Context<Self>) {
    self.move_line_start(cx);
  }
  fn on_move_line_end(&mut self, _: &MoveLineEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.move_line_end(cx);
  }
  fn on_select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
    self.select_left(window, cx);
  }
  fn on_select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
    self.select_right(window, cx);
  }
  fn on_select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
    self.select_up(window, cx);
  }
  fn on_select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
    self.select_down(window, cx);
  }
  fn on_select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
    self.select_line_start(cx);
  }
  fn on_select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.select_line_end(cx);
  }
  fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
    self.select_all(cx);
  }
  fn on_move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) {
    self.move_word_left(cx);
  }
  fn on_move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) {
    self.move_word_right(cx);
  }
  fn on_select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
    self.select_word_left(cx);
  }
  fn on_select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
    self.select_word_right(cx);
  }
  fn on_delete_word_backward(&mut self, _: &DeleteWordBackward, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_word_backward_command(cx);
  }
  fn on_delete_word_forward(&mut self, _: &DeleteWordForward, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_word_forward_command(cx);
  }
  fn on_page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
    self.page_up(cx);
  }
  fn on_page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
    self.page_down(cx);
  }
  fn on_select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) {
    self.select_page_up(cx);
  }
  fn on_select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) {
    self.select_page_down(cx);
  }
  fn on_move_document_start(&mut self, _: &MoveDocumentStart, _: &mut Window, cx: &mut Context<Self>) {
    self.move_document_start(cx);
  }
  fn on_move_document_end(&mut self, _: &MoveDocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.move_document_end(cx);
  }
  fn on_select_document_start(&mut self, _: &SelectDocumentStart, _: &mut Window, cx: &mut Context<Self>) {
    self.select_document_start(cx);
  }
  fn on_select_document_end(&mut self, _: &SelectDocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.select_document_end(cx);
  }
  fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
    self.copy(cx);
  }
  fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
    self.cut(cx);
  }
  fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
    self.paste(cx);
  }
  fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
    self.undo(cx);
  }
  fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
    self.redo(cx);
  }
  fn on_set_paragraph_pocket(&mut self, _: &SetParagraphPocket, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Pocket, cx);
  }
  fn on_set_paragraph_hat(&mut self, _: &SetParagraphHat, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Hat, cx);
  }
  fn on_set_paragraph_block(&mut self, _: &SetParagraphBlock, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Block, cx);
  }
  fn on_set_paragraph_tag(&mut self, _: &SetParagraphTag, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Tag, cx);
  }
  fn on_set_paragraph_analytic(&mut self, _: &SetParagraphAnalytic, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Analytic, cx);
  }
  fn on_set_paragraph_undertag(&mut self, _: &SetParagraphUndertag, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Undertag, cx);
  }
  fn on_toggle_cite(&mut self, _: &ToggleCite, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_cite(cx);
  }
  fn on_toggle_underline(&mut self, _: &ToggleUnderline, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_underline(cx);
  }
  fn on_toggle_strikethrough(&mut self, _: &ToggleStrikethrough, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_strikethrough(cx);
  }
  fn on_toggle_emphasis(&mut self, _: &ToggleEmphasis, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_emphasis(cx);
  }
  fn on_set_highlight_spoken(&mut self, _: &SetHighlightSpoken, _: &mut Window, cx: &mut Context<Self>) {
    self.set_highlight(HighlightStyle::Spoken, cx);
  }
  fn on_apply_highlight_to_selection(&mut self, _: &ApplyHighlightToSelection, _: &mut Window, cx: &mut Context<Self>) {
    self.apply_current_highlight_to_selection(cx);
  }
  fn on_clear_formatting(&mut self, _: &ClearFormatting, _: &mut Window, cx: &mut Context<Self>) {
    self.clear_formatting(cx);
  }
  fn on_clear_highlight(&mut self, _: &ClearHighlight, _: &mut Window, cx: &mut Context<Self>) {
    self.clear_highlight(cx);
  }
  fn on_insert_image(&mut self, _: &InsertImage, _: &mut Window, cx: &mut Context<Self>) {
    self.prompt_insert_image(cx);
  }
  fn on_insert_table(&mut self, _: &InsertTable, _: &mut Window, cx: &mut Context<Self>) {
    self.insert_default_table(2, 2, cx);
  }
  fn on_insert_equation(&mut self, _: &InsertEquation, _: &mut Window, cx: &mut Context<Self>) {
    self.insert_equation("x^2 + y^2 = z^2", cx);
  }
  fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
    self.zoom_in(cx);
  }
  fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
    self.zoom_out(cx);
  }
  fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
    self.backspace_command(cx);
  }
  fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_forward_command(cx);
  }
  fn on_insert_newline(&mut self, _: &InsertNewline, _: &mut Window, cx: &mut Context<Self>) {
    if self.split_selected_table_cell_paragraph(cx) {
      return;
    }
    self.insert_paragraph_break_command(cx);
  }
  fn on_insert_soft_line_break(&mut self, _: &InsertSoftLineBreak, _: &mut Window, cx: &mut Context<Self>) {
    if self.insert_text_into_selected_table_cell(SOFT_LINE_BREAK_STR, cx) {
      return;
    }
    if self.insert_text_into_selected_equation(SOFT_LINE_BREAK_STR, cx) {
      return;
    }
    self.insert_text_command(SOFT_LINE_BREAK_STR, cx);
  }

  // Raw key handler: routes printable characters to `insert_text`. Non-
  // printable keys (arrows, Backspace, etc.) carry `key_char = None` and are
  // ignored here — they are routed via the action system above instead.
  fn on_key_down_event(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    // If the user is holding a modifier that turns the key into a shortcut
    // (Ctrl/Cmd), don't insert the character. Shift and Alt remain available
    // for things like capital letters and option-letter accented chars.
    let m = &event.keystroke.modifiers;
    if m.control || m.platform {
      return;
    }
    if event.keystroke.key == "tab"
      && self.move_selected_table_cell(!m.shift, cx)
    {
      return;
    }
    #[cfg(target_os = "windows")]
    let key_char = event
      .keystroke
      .key_char
      .as_deref()
      .or_else(|| (event.keystroke.key == "space" && !m.alt && !m.function).then_some(" "));

    #[cfg(not(target_os = "windows"))]
    let key_char = event.keystroke.key_char.as_deref();

    let Some(key_char) = key_char else {
      return;
    };
    if key_char.is_empty() {
      return;
    }
    #[cfg(target_os = "windows")]
    {
      let key_char = if window.capslock().on {
        windows_apply_capslock(key_char)
      } else {
        key_char.to_string()
      };
      if self.insert_text_into_selected_table_cell(&key_char, cx) {
        return;
      }
      if self.insert_text_into_selected_equation(&key_char, cx) {
        return;
      }
      self.insert_text_command(&key_char, cx);
    }

    #[cfg(not(target_os = "windows"))]
    {
      let _ = window;
      if self.insert_text_into_selected_table_cell(key_char, cx) {
        return;
      }
      if self.insert_text_into_selected_equation(key_char, cx) {
        return;
      }
      self.insert_text_command(key_char, cx);
    }
  }

  pub(super) fn apply_document_edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Self, &mut Context<Self>)) {
    self.apply_document_edit_with_capture_range(cx, None, edit);
  }

}
