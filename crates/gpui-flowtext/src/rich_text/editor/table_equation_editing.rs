#[hotpath::measure_all]
impl RichTextEditor {
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
    if self.authoritative_edit_controller.is_some()
      && let Some(paragraph) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, self.table_cell_block_ix)
      && let Some(planned_selection) = self.authoritative_source_selection(&self.selection)
    {
      let mut operations = Vec::with_capacity(2);
      if let Some(range) = selection_range.clone() {
        operations.push(AuthoritativeSourceOperation::DeleteText {
          start: AuthoritativeSourcePosition {
            paragraph,
            byte: range.start,
          },
          end: AuthoritativeSourcePosition {
            paragraph,
            byte: range.end,
          },
        });
      }
      operations.push(AuthoritativeSourceOperation::InsertText {
        at: AuthoritativeSourcePosition {
          paragraph,
          byte: insert_at,
        },
        text: text.to_string(),
        styles,
      });
      self.apply_authoritative_source_operations(operations, planned_selection, cx);
      self.table_cell_caret = insert_at.saturating_add(text.len());
      self.table_cell_anchor = self.table_cell_caret;
      return true;
    }
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
      self.selected_block = None;
      self.table_cell_block_ix = 0;
      self.table_cell_anchor = 0;
      self.table_cell_caret = 0;
      cx.notify();
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
    if self.authoritative_edit_controller.is_some() {
      if let Some(paragraph) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, self.table_cell_block_ix)
        && let Some(planned_selection) = self.authoritative_source_selection(&self.selection)
      {
        self.apply_authoritative_source_operations(
          vec![AuthoritativeSourceOperation::SplitParagraph {
            at: AuthoritativeSourcePosition {
              paragraph,
              byte: self.table_cell_caret,
            },
            new_paragraph: new_paragraph_id(),
            style: ParagraphStyle::Normal,
          }],
          planned_selection,
          cx,
        );
        self.table_cell_block_ix = new_paragraph_ix;
        self.table_cell_caret = 0;
        self.table_cell_anchor = 0;
        return true;
      }
      self.reject_projection_first_edit("Table-cell paragraph split", cx);
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
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      canonical_operations: vec![CanonicalOperation::ReplaceBlock {
        block: self.identity_map.block_id(block_ix),
      }],
    });
    self.redo_stack.clear();
    self.table_cell_block_ix = new_paragraph_ix;
    self.table_cell_caret = 0;
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
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
      if self.authoritative_edit_controller.is_some() {
        let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
          return true;
        };
        let Some(cell) = table.rows.get(row_ix).and_then(|row| row.cells.get(cell_ix)) else {
          return true;
        };
        let Some(current_ix) = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix) else {
          return true;
        };
        let Some(previous_ix) = previous_table_cell_paragraph_block_ix(cell, current_ix) else {
          return true;
        };
        let Some(TableCellBlock::Paragraph(previous)) = cell.blocks.get(previous_ix) else {
          return true;
        };
        let Some(current) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, current_ix) else {
          return true;
        };
        let Some(planned_selection) = self.authoritative_source_selection(&self.selection) else {
          return true;
        };
        let merged_caret = previous.text.len();
        self.apply_authoritative_source_operations(
          vec![AuthoritativeSourceOperation::JoinParagraph {
            second_paragraph: current,
          }],
          planned_selection,
          cx,
        );
        self.table_cell_block_ix = previous_ix;
        self.table_cell_caret = merged_caret;
        self.table_cell_anchor = merged_caret;
        return true;
      }
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
    if self.authoritative_edit_controller.is_some()
      && let Some(paragraph) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, self.table_cell_block_ix)
      && let Some(planned_selection) = self.authoritative_source_selection(&self.selection)
    {
      self.apply_authoritative_source_operations(
        vec![AuthoritativeSourceOperation::DeleteText {
          start: AuthoritativeSourcePosition {
            paragraph,
            byte: new_caret,
          },
          end: AuthoritativeSourcePosition { paragraph, byte: caret },
        }],
        planned_selection,
        cx,
      );
      self.table_cell_caret = new_caret;
      self.table_cell_anchor = new_caret;
      return true;
    }
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
      if self.authoritative_edit_controller.is_some()
        && let Some(paragraph) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, self.table_cell_block_ix)
        && let Some(planned_selection) = self.authoritative_source_selection(&self.selection)
      {
        self.apply_authoritative_source_operations(
          vec![AuthoritativeSourceOperation::DeleteText {
            start: AuthoritativeSourcePosition { paragraph, byte: caret },
            end: AuthoritativeSourcePosition { paragraph, byte: next },
          }],
          planned_selection,
          cx,
        );
        self.table_cell_caret = caret;
        self.table_cell_anchor = caret;
        return true;
      }
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        delete_range_in_table_cell_paragraph(paragraph, caret..next);
      });
    } else {
      if self.authoritative_edit_controller.is_some() {
        let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
          return true;
        };
        let Some(cell) = table.rows.get(row_ix).and_then(|row| row.cells.get(cell_ix)) else {
          return true;
        };
        let Some(current_ix) = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix) else {
          return true;
        };
        let Some(next_ix) = next_table_cell_paragraph_block_ix(cell, current_ix) else {
          return true;
        };
        let Some(next) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, next_ix) else {
          return true;
        };
        let Some(TableCellBlock::Paragraph(current)) = cell.blocks.get(current_ix) else {
          return true;
        };
        let Some(planned_selection) = self.authoritative_source_selection(&self.selection) else {
          return true;
        };
        let merged_caret = current.text.len();
        self.apply_authoritative_source_operations(
          vec![AuthoritativeSourceOperation::JoinParagraph {
            second_paragraph: next,
          }],
          planned_selection,
          cx,
        );
        self.table_cell_block_ix = current_ix;
        self.table_cell_caret = merged_caret;
        self.table_cell_anchor = merged_caret;
        return true;
      }
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
    self.equation_source_anchor = self.equation_source_caret;
    true
  }

  fn edit_selected_equation(&mut self, block_ix: usize, cx: &mut Context<Self>, update: impl FnOnce(&mut EquationBlock)) {
    let Some(Block::Equation(equation)) = self.document.blocks.get(block_ix).cloned() else {
      self.selected_block = None;
      self.equation_source_anchor = 0;
      self.equation_source_caret = 0;
      cx.notify();
      return;
    };
    let mut updated = equation.clone();
    update(&mut updated);
    if updated == equation {
      return;
    }
    if self.authoritative_edit_controller.is_some() {
      if updated.syntax == equation.syntax
        && updated.display == equation.display
        && let Some(block_id) = self.document.ids.block_ids.get(block_ix).copied()
        && let Some(planned_selection) = self.authoritative_source_selection(&self.selection)
      {
        self.apply_authoritative_source_operations(
          vec![AuthoritativeSourceOperation::SetEquationSource {
            block_id,
            source: updated.source.to_string(),
          }],
          planned_selection,
          cx,
        );
      }
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
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      canonical_operations: vec![CanonicalOperation::ReplaceBlock {
        block: self.identity_map.block_id(block_ix),
      }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
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
      self.selected_block = None;
      self.table_cell_block_ix = 0;
      self.table_cell_anchor = 0;
      self.table_cell_caret = 0;
      cx.notify();
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
    if self.authoritative_edit_controller.is_some() {
      let Some(TableCellBlock::Paragraph(before_paragraph)) = table
        .rows
        .get(row_ix)
        .and_then(|row| row.cells.get(cell_ix))
        .and_then(|cell| cell.blocks.get(paragraph_ix))
      else {
        return;
      };
      let Some(TableCellBlock::Paragraph(after_paragraph)) = updated
        .rows
        .get(row_ix)
        .and_then(|row| row.cells.get(cell_ix))
        .and_then(|cell| cell.blocks.get(paragraph_ix))
      else {
        return;
      };
      let Some(paragraph_id) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, paragraph_ix) else {
        return;
      };
      let Some(planned_selection) = self.authoritative_source_selection(&self.selection) else {
        return;
      };
      let operations = table_cell_source_operations(paragraph_id, before_paragraph, after_paragraph);
      if !operations.is_empty() {
        self.apply_authoritative_source_operations(operations, planned_selection, cx);
      }
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
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      canonical_operations: vec![CanonicalOperation::ReplaceBlock {
        block: self.identity_map.block_id(block_ix),
      }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  fn table_cell_paragraph_id(
    &self,
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    cell_block_ix: usize,
  ) -> Option<ParagraphId> {
    let block_id = *self.document.ids.block_ids.get(block_ix)?;
    let RichBlockIdentity::Table(table) = self.document.ids.rich_block_ids.get(&block_id)? else {
      return None;
    };
    let cell = table.rows.get(row_ix)?.cells.get(cell_ix)?;
    match cell.blocks.get(cell_block_ix)? {
      TableCellBlockIdentity::Paragraph(paragraph) => Some(*paragraph),
      TableCellBlockIdentity::Table { .. } => None,
    }
  }

}

fn table_cell_source_operations(
  paragraph: ParagraphId,
  before: &TableCellParagraph,
  after: &TableCellParagraph,
) -> Vec<AuthoritativeSourceOperation> {
  let mut operations = Vec::new();
  if before.paragraph.style != after.paragraph.style {
    operations.push(AuthoritativeSourceOperation::SetParagraphStyle {
      paragraph,
      style: after.paragraph.style,
    });
  }
  if before.text != after.text {
    let (prefix, before_end, after_end) = changed_text_span(&before.text, &after.text);
    if prefix < before_end {
      operations.push(AuthoritativeSourceOperation::DeleteText {
        start: AuthoritativeSourcePosition {
          paragraph,
          byte: prefix,
        },
        end: AuthoritativeSourcePosition {
          paragraph,
          byte: before_end,
        },
      });
    }
    if prefix < after_end {
      operations.push(AuthoritativeSourceOperation::InsertText {
        at: AuthoritativeSourcePosition {
          paragraph,
          byte: prefix,
        },
        text: after.text[prefix..after_end].to_string(),
        styles: table_cell_styles_at(after, prefix),
      });
    }
  } else if before.paragraph.runs != after.paragraph.runs && !after.text.is_empty() {
    if after.paragraph.runs.is_empty() {
      operations.push(AuthoritativeSourceOperation::SetRunStyles {
        paragraph,
        range: 0..after.text.len(),
        patch: RunStylePatch::replace(RunStyles::default()),
      });
    } else {
      let mut start = 0;
      for run in &after.paragraph.runs {
        let end = start + run.len;
        operations.push(AuthoritativeSourceOperation::SetRunStyles {
          paragraph,
          range: start..end,
          patch: RunStylePatch::replace(run.styles),
        });
        start = end;
      }
    }
  }
  operations
}

fn changed_text_span(before: &str, after: &str) -> (usize, usize, usize) {
  let mut prefix = before
    .as_bytes()
    .iter()
    .zip(after.as_bytes())
    .take_while(|(left, right)| left == right)
    .count();
  while prefix > 0 && (!before.is_char_boundary(prefix) || !after.is_char_boundary(prefix)) {
    prefix -= 1;
  }
  let max_suffix = before.len().saturating_sub(prefix).min(after.len().saturating_sub(prefix));
  let mut suffix = before
    .as_bytes()
    .iter()
    .rev()
    .zip(after.as_bytes().iter().rev())
    .take(max_suffix)
    .take_while(|(left, right)| left == right)
    .count();
  while suffix > 0 && (!before.is_char_boundary(before.len() - suffix) || !after.is_char_boundary(after.len() - suffix)) {
    suffix -= 1;
  }
  (prefix, before.len() - suffix, after.len() - suffix)
}

#[cfg(test)]
mod table_equation_editing_tests {
  #[test]
  #[ignore = "target state: remote table and equation edits must fail deterministically on missing ids or out-of-bounds ranges"]
  fn remote_table_and_equation_edits_must_be_deterministic() {}
}
