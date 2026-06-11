#[hotpath::measure_all]
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
      let direct = explicit_direct.unwrap_or(matches!(paragraph_style, ParagraphStyle::Custom(3) | ParagraphStyle::Custom(4)));
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| {
          let range = selection_range.clone();
          !range.is_empty()
            && table_cell_range_all_run_styles(paragraph, range, |styles| {
              if direct {
                styles.direct_underline
              } else {
                styles.semantic == RunSemanticStyle::Custom(3)
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
            styles.semantic = RunSemanticStyle::Custom(3);
            styles.direct_underline = false;
          }
        });
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
      let direct = explicit_direct.unwrap_or(matches!(paragraph_style, ParagraphStyle::Custom(3) | ParagraphStyle::Custom(4)));
      let mut styles = self.styles_at_caret();
      if direct {
        styles.direct_underline = !styles.direct_underline;
      } else {
        if styles.semantic == RunSemanticStyle::Custom(3) {
          styles.semantic = RunSemanticStyle::Plain;
        } else {
          styles.semantic = RunSemanticStyle::Custom(3);
          styles.direct_underline = false;
        }
      }
      self.set_pending_styles(styles, cx);
      return;
    }

    let range = self.selection.normalized();
    let direct = explicit_direct.unwrap_or_else(|| selection_prefers_direct_underline(&self.document, range.clone()));
    let all_selected = selection_all_underline_kind(&self.document, range.clone(), direct);
    if self.apply_run_style_source_first(
      |mut styles| {
        if direct {
          styles.direct_underline = !all_selected;
        } else if all_selected {
          styles.semantic = RunSemanticStyle::Plain;
        } else {
          styles.semantic = RunSemanticStyle::Custom(3);
          styles.direct_underline = false;
        }
        styles
      },
      cx,
    ) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        if direct {
          styles.direct_underline = !all_selected;
        } else {
          let new_value = !all_selected;
          if new_value {
            styles.semantic = RunSemanticStyle::Custom(3);
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
      self.set_pending_styles(styles, cx);
      return;
    }

    let range = self.selection.normalized();
    let all_selected = selection_all_run_styles(&self.document, range.clone(), |styles| styles.semantic == semantic);
    if self.apply_run_style_source_first(
      |mut styles| {
        styles.semantic = if all_selected { RunSemanticStyle::Plain } else { semantic };
        styles
      },
      cx,
    ) {
      return;
    }
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
      self.set_pending_styles(styles, cx);
      return;
    }

    let range = self.selection.normalized();
    let all_selected = if let Some(highlight) = highlight {
      selection_all_run_styles(&self.document, range.clone(), |styles| styles.highlight == Some(highlight))
    } else {
      false
    };
    let target_highlight = if all_selected { None } else { highlight };
    if self.apply_run_style_source_first(
      |mut styles| {
        styles.highlight = target_highlight;
        styles
      },
      cx,
    ) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| styles.highlight = target_highlight);
      editor.after_formatting_mutation(cx);
    });
  }

  fn set_pending_styles(&mut self, styles: RunStyles, cx: &mut Context<Self>) {
    self.pending_styles = Some(styles);
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn apply_run_style_source_first(
    &mut self,
    transform: impl Fn(RunStyles) -> RunStyles,
    cx: &mut Context<Self>,
  ) -> bool {
    if self.authoritative_edit_controller.is_none() || self.selected_block.is_some() || self.selection.is_caret() {
      return false;
    }
    let Some(planned_selection) = self.authoritative_source_selection(&self.selection) else {
      return false;
    };
    let range = self.selection.normalized();
    let Some(operations) = self.run_style_source_operations(range, transform) else {
      return false;
    };
    if operations.is_empty() {
      return true;
    }
    self.apply_authoritative_source_operations(operations, planned_selection, cx)
  }

  fn run_style_source_operations(
    &self,
    range: Range<DocumentOffset>,
    transform: impl Fn(RunStyles) -> RunStyles,
  ) -> Option<Vec<AuthoritativeSourceOperation>> {
    let mut operations = Vec::new();
    for paragraph_ix in range.start.paragraph..=range.end.paragraph {
      let paragraph = self.document.paragraphs.get(paragraph_ix)?;
      let paragraph_id = paragraph_id_at(&self.document, paragraph_ix)?;
      let selection_start = if paragraph_ix == range.start.paragraph {
        range.start.byte
      } else {
        0
      };
      let selection_end = if paragraph_ix == range.end.paragraph {
        range.end.byte
      } else {
        paragraph_text_len(paragraph)
      };
      let mut run_start = 0;
      for run in &paragraph.runs {
        let run_end = run_start + run.len;
        let start = run_start.max(selection_start);
        let end = run_end.min(selection_end);
        let styles = transform(run.styles);
        if start < end && styles != run.styles {
          operations.push(AuthoritativeSourceOperation::SetRunStyles {
            paragraph: paragraph_id,
            range: start..end,
            styles,
          });
        }
        run_start = run_end;
      }
    }
    Some(operations)
  }


  pub(super) fn styles_at_caret(&self) -> RunStyles {
    if let Some(styles) = self.pending_styles {
      return styles;
    }
    let caret = self.selection.head;
    let Some(paragraph) = self.document.paragraphs.get(caret.paragraph) else {
      return RunStyles::default();
    };
    let (run_ix, _) = run_containing(paragraph, caret.byte);
    paragraph
      .runs
      .get(run_ix)
      .map(|run| run.styles)
      .unwrap_or_default()
  }

  // -------- Movement primitives ----------------------------------------
}

