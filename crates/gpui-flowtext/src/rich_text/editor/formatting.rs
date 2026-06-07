#[hotpath::measure_all]
impl RichTextEditor {
  pub fn toggle_underline(&mut self, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Underline, cx) {
      return;
    }
    self.toggle_underline_kind(None, cx);
  }

  pub fn toggle_strikethrough(&mut self, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Strikethrough, cx) {
      return;
    }
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        self.armed_inline_tool = Some(ArmedInlineTool::Strikethrough);
        cx.notify();
        return;
      };
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| table_cell_range_all_run_styles(paragraph, selection_range.clone(), |styles| styles.strikethrough))
        .unwrap_or(false);
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.paragraph.runs.is_empty() && !paragraph.text.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range, |styles| styles.strikethrough = !all_selected);
      });
      return;
    }
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.strikethrough = !styles.strikethrough;
      self.pending_styles = Some(styles);
      cx.notify();
      return;
    }
    let range = self.selection.normalized();
    let all_selected = selection_all_run_styles(&self.document, range.clone(), |styles| styles.strikethrough);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| styles.strikethrough = !all_selected);
      editor.after_formatting_mutation(cx);
    });
  }

  /// Toggle any semantic inline style for the current selection or caret.
  ///
  pub fn toggle_semantic_style_for_selection(&mut self, semantic: RunSemanticStyle, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Semantic(semantic), cx) {
      return;
    }
    self.toggle_semantic_style(semantic, cx);
  }

  pub fn set_highlight(&mut self, highlight: HighlightStyle, cx: &mut Context<Self>) {
    self.current_highlight_style = highlight;
    self.current_highlight_choice = Some(highlight);
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Highlight(highlight), cx) {
      return;
    }
    self.set_highlight_internal(Some(highlight), cx);
  }

  /// Set or clear the highlight style for the current selection or caret.
  ///
  /// `None` clears highlights. `Some(...)` applies the requested highlight, or
  /// toggles it off when the whole selection already has that highlight.
  pub fn set_highlight_for_selection(&mut self, highlight: Option<HighlightStyle>, cx: &mut Context<Self>) {
    self.set_highlight_internal(highlight, cx);
  }

  pub fn clear_highlight(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_internal(None, cx);
  }

  pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        paragraph.paragraph.style = ParagraphStyle::Normal;
        for run in &mut paragraph.paragraph.runs {
          run.styles = RunStyles::default();
        }
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let paragraph_ix = editor.selection.head.paragraph;
        clear_whole_paragraph_formatting(&mut editor.document, paragraph_ix);
      } else {
        let range = editor.selection.normalized();
        if selection_contains_whole_paragraph(&editor.document, range.clone()) {
          for paragraph_ix in range.start.paragraph..=range.end.paragraph {
            clear_whole_paragraph_formatting(&mut editor.document, paragraph_ix);
          }
        } else {
          mutate_runs_in_range(&mut editor.document, range, |styles| *styles = RunStyles::default());
        }
      }
      rebuild_document_sections(&mut editor.document);
      editor.pending_styles = None;
      editor.after_formatting_mutation(cx);
    });
  }

  pub fn apply_run_style_to_selection(&mut self, style: RunStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| styles.apply(style));
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      let range = editor.selection.normalized();
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
        let end = if paragraph_ix == range.end.paragraph {
          range.end.byte
        } else {
          paragraph_text_len(&editor.document.paragraphs[paragraph_ix])
        };
        apply_style_to_paragraph_range(&mut editor.document, paragraph_ix, start..end, style);
      }
      editor.after_formatting_mutation(cx);
    });
  }

  pub fn set_paragraph_style_for_selection(&mut self, style: ParagraphStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.paragraph.style != style {
          paragraph.paragraph.style = style;
          paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
        }
      });
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      let range = editor.selection.normalized();
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        if let Some(paragraph) = paragraphs_mut(&mut editor.document).get_mut(paragraph_ix)
          && paragraph.style != style
        {
          paragraph.style = style;
          bump_paragraph_version(paragraph);
        }
      }
      rebuild_document_sections(&mut editor.document);
      editor.after_formatting_mutation(cx);
    });
  }

  // -------- Action handlers (bound to keystrokes in main.rs) -----------
  // Each handler delegates to a movement/edit primitive defined below.
  // The signatures all match what `cx.listener(...)` expects:
  //   fn(&mut Self, &Action, &mut Window, &mut Context<Self>).

}
