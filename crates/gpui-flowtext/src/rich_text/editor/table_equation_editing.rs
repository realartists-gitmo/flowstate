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
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
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
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      if let Some(range) = selection_range.clone() {
        delete_range_in_table_cell_paragraph(paragraph, range);
      }
      insert_text_in_table_cell_paragraph(paragraph, insert_at, text, styles);
    });
    self.table_cell_caret = insert_at.saturating_add(text.len());
    self.table_cell_anchor = self.table_cell_caret;
    true
  }

  fn split_selected_table_cell_paragraph(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return false;
    };
    let mut updated = table.clone();
    let Some(cell) = updated
      .rows
      .get_mut(row_ix)
      .and_then(|row| row.cells.get_mut(cell_ix))
    else {
      return false;
    };
    let Some(new_paragraph_ix) = split_table_cell_paragraph_at(cell, self.table_cell_block_ix, self.table_cell_caret) else {
      return true;
    };
    if updated == table {
      return true;
    }
    updated.version = updated.version.wrapping_add(1);
    let before = Block::Table(table);
    let after = Block::Table(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = vec![SemanticEditCommand::ReplaceBlock {
      block: self.identity_map.block_id(block_ix),
      block_ix,
      after: input_block_from_block(&after),
    }];
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.table_cell_block_ix = new_paragraph_ix;
    self.table_cell_caret = 0;
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
    true
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
    self.edit_selected_equation(block_ix, cx, |equation| {
      let mut source = equation.source.to_string();
      let insert_at = insert_at.min(source.len());
      if !source.is_char_boundary(insert_at) {
        return;
      }
      if let Some(range) = selection_range.clone()
        && range.start <= range.end
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
      {
        source.replace_range(range, "");
      }
      source.insert_str(insert_at, text);
      equation.source = source.into();
      equation.version = equation.version.wrapping_add(1);
    });
    self.equation_source_caret = insert_at.saturating_add(text.len());
    self.equation_source_anchor = self.equation_source_caret;
    true
  }

  fn backspace_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let caret = self.table_cell_caret;
    if caret == 0 {
      let mut merged_caret = None;
      let current_paragraph_ix = self.table_cell_block_ix;
      self.edit_selected_table(cx, |table| {
        let Some(cell) = table
          .rows
          .get_mut(row_ix)
          .and_then(|row| row.cells.get_mut(cell_ix))
        else {
          return;
        };
        merged_caret = merge_table_cell_paragraph_with_previous(cell, current_paragraph_ix);
      });
      if let Some((paragraph_ix, byte)) = merged_caret {
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
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
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
    self.table_cell_caret = new_caret;
    true
  }

  fn delete_forward_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
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
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        delete_range_in_table_cell_paragraph(paragraph, caret..next);
      });
    } else {
      let mut merged_caret = None;
      let current_paragraph_ix = self.table_cell_block_ix;
      self.edit_selected_table(cx, |table| {
        let Some(cell) = table
          .rows
          .get_mut(row_ix)
          .and_then(|row| row.cells.get_mut(cell_ix))
        else {
          return;
        };
        merged_caret = merge_table_cell_paragraph_with_next(cell, current_paragraph_ix);
      });
      if let Some((paragraph_ix, byte)) = merged_caret {
        self.table_cell_block_ix = paragraph_ix;
        self.table_cell_caret = byte;
        cx.notify();
      }
      return true;
    }
    self.table_cell_caret = caret;
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
    let mut next_caret = caret;
    self.edit_selected_equation(block_ix, cx, |equation| {
      let mut source = equation.source.to_string();
      if let Some(range) = selection_range.clone()
        && range.start <= range.end
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
      {
        source.replace_range(range.clone(), "");
        next_caret = range.start;
        equation.source = source.into();
        equation.version = equation.version.wrapping_add(1);
        return;
      }
      let caret = caret.min(source.len());
      if caret > 0
        && source.is_char_boundary(caret)
        && let Some((byte, _)) = source[..caret].char_indices().next_back()
      {
        source.replace_range(byte..caret, "");
        next_caret = byte;
        equation.source = source.into();
        equation.version = equation.version.wrapping_add(1);
      }
    });
    self.equation_source_caret = next_caret;
    self.equation_source_anchor = next_caret;
    true
  }

  fn edit_selected_equation(&mut self, block_ix: usize, cx: &mut Context<Self>, update: impl FnOnce(&mut EquationBlock)) {
    let Some(Block::Equation(equation)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = equation.clone();
    update(&mut updated);
    if updated == equation {
      return;
    }
    let before = Block::Equation(equation);
    let after = Block::Equation(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = vec![SemanticEditCommand::ReplaceBlock {
      block: self.identity_map.block_id(block_ix),
      block_ix,
      after: input_block_from_block(&after),
    }];
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
  }

  pub(super) fn edit_table_cell_paragraph(
    &mut self,
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    cx: &mut Context<Self>,
    update: impl FnOnce(&mut TableCellParagraph),
  ) {
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    let Some(cell) = updated
      .rows
      .get_mut(row_ix)
      .and_then(|row| row.cells.get_mut(cell_ix))
    else {
      return;
    };
    let paragraph_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix).unwrap_or_else(|| {
      cell
        .blocks
        .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
      cell.blocks.len() - 1
    });
    let TableCellBlock::Paragraph(paragraph) = &mut cell.blocks[paragraph_ix] else {
      return;
    };
    update(paragraph);
    if updated == table {
      return;
    }
    updated.version = updated.version.wrapping_add(1);
    let before = Block::Table(table);
    let after = Block::Table(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = vec![SemanticEditCommand::ReplaceBlock {
      block: self.identity_map.block_id(block_ix),
      block_ix,
      after: input_block_from_block(&after),
    }];
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
  }

}
