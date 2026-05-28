impl RichTextEditor {
  fn toggle_underline_kind(&mut self, explicit_direct: Option<bool>, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      let paragraph_style = self
        .selected_table_cell_paragraph()
        .map(|paragraph| paragraph.paragraph.style)
        .unwrap_or(ParagraphStyle::Normal);
      let direct = explicit_direct.unwrap_or(matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic));
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| {
          let range = selection_range.clone();
          !range.is_empty()
            && table_cell_range_all_run_styles(paragraph, range, |styles| {
              if direct {
                styles.direct_underline
              } else {
                styles.semantic == RunSemanticStyle::Underline
              }
            })
        })
        .unwrap_or(false);
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
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| {
          if direct {
            styles.direct_underline = !all_selected;
          } else if all_selected {
            styles.semantic = RunSemanticStyle::Plain;
          } else {
            styles.semantic = RunSemanticStyle::Underline;
            styles.direct_underline = false;
          }
        });
      });
      return;
    }
    if self.selection.is_caret() {
      let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
      let direct = explicit_direct.unwrap_or(matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic));
      let mut styles = self.styles_at_caret();
      if direct {
        styles.direct_underline = !styles.direct_underline;
      } else {
        if styles.semantic == RunSemanticStyle::Underline {
          styles.semantic = RunSemanticStyle::Plain;
        } else {
          styles.semantic = RunSemanticStyle::Underline;
          styles.direct_underline = false;
        }
      }
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let direct = explicit_direct.unwrap_or_else(|| selection_prefers_direct_underline(&self.document, range.clone()));
    let all_selected = selection_all_underline_kind(&self.document, range.clone(), direct);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        if direct {
          styles.direct_underline = !all_selected;
        } else {
          let new_value = !all_selected;
          if new_value {
            styles.semantic = RunSemanticStyle::Underline;
            styles.direct_underline = false;
          } else {
            styles.semantic = RunSemanticStyle::Plain;
          }
        }
      });
      editor.after_formatting_mutation(cx);
    });
  }

  fn toggle_semantic_style(&mut self, semantic: RunSemanticStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| {
          let range = selection_range.clone();
          !range.is_empty() && table_cell_range_all_run_styles(paragraph, range, |styles| styles.semantic == semantic)
        })
        .unwrap_or(false);
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
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| {
          styles.semantic = if all_selected { RunSemanticStyle::Plain } else { semantic };
        });
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.semantic = if styles.semantic == semantic {
        RunSemanticStyle::Plain
      } else {
        semantic
      };
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let all_selected = selection_all_run_styles(&self.document, range.clone(), |styles| styles.semantic == semantic);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        styles.semantic = if all_selected { RunSemanticStyle::Plain } else { semantic };
      });
      editor.after_formatting_mutation(cx);
    });
  }

  fn set_highlight_internal(&mut self, highlight: Option<HighlightStyle>, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      let all_selected = self
        .selected_table_cell_paragraph()
        .and_then(|paragraph| {
          highlight.map(|highlight| {
            let range = selection_range.clone();
            !range.is_empty() && table_cell_range_all_run_styles(paragraph, range, |styles| styles.highlight == Some(highlight))
          })
        })
        .unwrap_or(false);
      let target_highlight = if all_selected { None } else { highlight };
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
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| styles.highlight = target_highlight);
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.highlight = highlight;
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let all_selected = if let Some(highlight) = highlight {
      selection_all_run_styles(&self.document, range.clone(), |styles| styles.highlight == Some(highlight))
    } else {
      false
    };
    let target_highlight = if all_selected { None } else { highlight };
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| styles.highlight = target_highlight);
      editor.after_formatting_mutation(cx);
    });
  }

  pub(super) fn styles_at_caret(&self) -> RunStyles {
    if let Some(styles) = self.pending_styles {
      return styles;
    }
    let caret = self.selection.head;
    let paragraph = &self.document.paragraphs[caret.paragraph];
    let (run_ix, _) = run_containing(paragraph, caret.byte);
    paragraph
      .runs
      .get(run_ix)
      .map(|run| run.styles)
      .unwrap_or_default()
  }

  // -------- Movement primitives ----------------------------------------

}
