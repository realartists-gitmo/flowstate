#[hotpath::measure_all]
impl RichTextEditor {
  fn insert_text_into_selected_object_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    if self.insert_text_into_selected_table_cell(text, cx) {
      return true;
    }
    if self.insert_text_into_selected_equation(text, cx) {
      return true;
    }
    false
  }

  fn insert_text_into_selected_table_cell(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    if text.is_empty() {
      return true;
    }
    let selection_range = self.table_cell_selection_range();
    let insert_at = selection_range
      .as_ref()
      .map(|range| range.start)
      .unwrap_or(self.table_cell_caret);
    let styles = self
      .selected_table_cell_paragraph()
      .map(|paragraph| table_cell_styles_at(paragraph, insert_at))
      .unwrap_or_default();
    let committed = self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      if let Some(range) = selection_range.clone() {
        delete_range_in_table_cell_paragraph(paragraph, range);
      }
      insert_text_in_table_cell_paragraph(paragraph, insert_at, text, styles);
    });
    if committed {
      // Local caret state inside the cell stays editor-side (spec §5); the
      // projection itself advanced through the intent's returned patches.
      self.table_cell_caret = insert_at.saturating_add(text.len());
      self.table_cell_anchor = self.table_cell_caret;
    }
    true
  }

  fn split_selected_table_cell_paragraph(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    let paragraph_ix = self.table_cell_block_ix;
    let caret = self.table_cell_caret;
    let mut new_paragraph_ix = None;
    let committed = self.edit_table_cell(block_ix, row_ix, cell_ix, cx, |cell| {
      new_paragraph_ix = split_table_cell_paragraph_at(cell, paragraph_ix, caret);
    });
    if committed && let Some(paragraph_ix) = new_paragraph_ix {
      self.table_cell_block_ix = paragraph_ix;
      self.table_cell_caret = 0;
      cx.notify();
    }
    true
  }

  /// Table-cell text edits keep whole-cell granularity: build the after-cell
  /// from the projected cell (via `update`) and issue ONE
  /// `TableIntent::ReplaceCell`. The document is never mutated directly — the
  /// intent's returned patches advance the projection inside `write_intent`.
  fn edit_table_cell(
    &mut self,
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    cx: &mut Context<Self>,
    update: impl FnOnce(&mut TableCell),
  ) -> bool {
    let before = match self.document.blocks.get(block_ix) {
      Some(Block::Table(table)) => table
        .rows
        .get(row_ix)
        .and_then(|row| row.cells.get(cell_ix))
        .cloned(),
      _ => None,
    };
    let Some(before) = before else {
      return false;
    };
    let mut after = before.clone();
    update(&mut after);
    if after == before {
      return false;
    }
    let Some(table_id) = self.semantic_block_id(block_ix) else {
      tracing::warn!(block_ix, "refusing table-cell edit: projection block has no durable id, so no intent can address it");
      return false;
    };
    // Durable coordinate from the id-bearing model at the edited cell's
    // position (§P2b); the cell already carries these ids.
    let row = after.row_id;
    let column = after.column_id;
    self
      .write_intent(
        LocalIntent::Table(crate::local_intents::TableIntent::ReplaceCell {
          table: table_id,
          row,
          column,
          cell: input_table_cell_from_table_cell(&after),
        }),
        cx,
      )
      .is_some()
  }

  fn insert_text_into_selected_equation(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::Equation(block_ix)) = self.selected_block else {
      return false;
    };
    if text.is_empty() {
      return true;
    }
    let selection_range = self.equation_source_selection_range();
    let insert_at = selection_range
      .as_ref()
      .map(|range| range.start)
      .unwrap_or(self.equation_source_caret);
    let range = selection_range.unwrap_or(insert_at..insert_at);
    if self.edit_selected_equation_source_range(block_ix, range, text, cx) {
      self.equation_source_caret = insert_at.saturating_add(text.len());
      self.equation_source_anchor = self.equation_source_caret;
    }
    true
  }

  fn backspace_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    let caret = self.table_cell_caret;
    if caret == 0 {
      let mut merged_caret = None;
      let current_paragraph_ix = self.table_cell_block_ix;
      let committed = self.edit_table_cell(block_ix, row_ix, cell_ix, cx, |cell| {
        merged_caret = merge_table_cell_paragraph_with_previous(cell, current_paragraph_ix);
      });
      if committed && let Some((paragraph_ix, byte)) = merged_caret {
        self.table_cell_block_ix = paragraph_ix;
        self.table_cell_caret = byte;
        cx.notify();
      }
      return true;
    }
    let new_caret = self
      .selected_table_cell_text()
      .and_then(|text| {
        let caret = caret.min(text.len());
        (caret > 0).then(|| {
          text[..caret]
            .char_indices()
            .next_back()
            .map(|(byte, _)| byte)
            .unwrap_or(0)
        })
      })
      .unwrap_or(caret);
    let committed = self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      let caret = caret.min(paragraph.text.len());
      if caret == 0 {
        return;
      }
      let prev = paragraph.text[..caret]
        .char_indices()
        .next_back()
        .map(|(byte, _)| byte)
        .unwrap_or(0);
      delete_range_in_table_cell_paragraph(paragraph, prev..caret);
    });
    if committed {
      self.table_cell_caret = new_caret;
    }
    true
  }

  fn delete_forward_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    let Some(text) = self.selected_table_cell_text() else {
      return true;
    };
    let caret = self.table_cell_caret.min(text.len());
    let next = if caret < text.len() {
      text[caret..]
        .char_indices()
        .nth(1)
        .map(|(byte, _)| caret + byte)
        .unwrap_or(text.len())
    } else {
      caret
    };
    if next > caret {
      let committed = self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        delete_range_in_table_cell_paragraph(paragraph, caret..next);
      });
      if committed {
        self.table_cell_caret = caret;
      }
    } else {
      let mut merged_caret = None;
      let current_paragraph_ix = self.table_cell_block_ix;
      let committed = self.edit_table_cell(block_ix, row_ix, cell_ix, cx, |cell| {
        merged_caret = merge_table_cell_paragraph_with_next(cell, current_paragraph_ix);
      });
      if committed && let Some((paragraph_ix, byte)) = merged_caret {
        self.table_cell_block_ix = paragraph_ix;
        self.table_cell_caret = byte;
        cx.notify();
      }
    }
    true
  }

  fn backspace_selected_equation(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::Equation(block_ix)) = self.selected_block else {
      return false;
    };
    if self
      .selected_equation_source()
      .map(|source| source.is_empty())
      .unwrap_or(false)
      && self.equation_source_selection_range().is_none()
    {
      return self.delete_selected_block(cx);
    }
    let selection_range = self.equation_source_selection_range();
    let caret = self.equation_source_caret;
    let Some(source) = self.selected_equation_source() else {
      return true;
    };
    let (range, next_caret) = if let Some(range) = selection_range
      && range.start <= range.end
      && range.end <= source.len()
      && source.is_char_boundary(range.start)
      && source.is_char_boundary(range.end)
    {
      let next_caret = range.start;
      (range, next_caret)
    } else {
      let caret = caret.min(source.len());
      let Some(byte) = (caret > 0 && source.is_char_boundary(caret))
        .then(|| source[..caret].char_indices().next_back().map(|(byte, _)| byte))
        .flatten()
      else {
        return true;
      };
      (byte..caret, byte)
    };
    if self.edit_selected_equation_source_range(block_ix, range, "", cx) {
      self.equation_source_caret = next_caret;
      self.equation_source_anchor = next_caret;
    }
    true
  }

  /// Equation source edits are identity-addressed range replacements: one
  /// `ReplaceEquationSourceRangeIntent` through the write authority. No direct
  /// projection mutation, no history record — the intent's patches are the
  /// only way the projection changes.
  fn edit_selected_equation_source_range(&mut self, block_ix: usize, range: Range<usize>, text: &str, cx: &mut Context<Self>) -> bool {
    let Some(Block::Equation(equation)) = self.document.blocks.get(block_ix) else {
      return false;
    };
    let source: &str = &equation.source;
    if range.start > range.end
      || range.end > source.len()
      || !source.is_char_boundary(range.start)
      || !source.is_char_boundary(range.end)
    {
      return false;
    }
    if &source[range.clone()] == text {
      // No-op replacement; don't emit an intent.
      return false;
    }
    let Some(equation_id) = self.semantic_block_id(block_ix) else {
      tracing::warn!(block_ix, "refusing equation source edit: projection block has no durable id, so no intent can address it");
      return false;
    };
    self
      .write_intent(
        LocalIntent::ReplaceEquationSourceRange(crate::local_intents::ReplaceEquationSourceRangeIntent {
          equation: equation_id,
          range,
          text: text.to_string(),
        }),
        cx,
      )
      .is_some()
  }

  pub(super) fn edit_table_cell_paragraph(
    &mut self,
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    cx: &mut Context<Self>,
    update: impl FnOnce(&mut TableCellParagraph),
  ) -> bool {
    let preferred_paragraph_ix = self.table_cell_block_ix;
    self.edit_table_cell(block_ix, row_ix, cell_ix, cx, |cell| {
      let paragraph_ix = table_cell_paragraph_block_ix(cell, preferred_paragraph_ix).unwrap_or_else(|| {
        cell
          .blocks
          .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
        cell.blocks.len() - 1
      });
      let TableCellBlock::Paragraph(paragraph) = &mut cell.blocks[paragraph_ix] else {
        return;
      };
      update(paragraph);
    })
  }

}
