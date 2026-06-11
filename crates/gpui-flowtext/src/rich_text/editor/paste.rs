#[hotpath::measure_all]
impl RichTextEditor {
  fn insert_plain_text_paste_at_caret(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    if self.selected_block.is_some() || text.is_empty() {
      return false;
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: normalized
        .split('\n')
        .map(|text| InputParagraph {
          style: paragraph_style,
          runs: if text.is_empty() {
            Vec::new()
          } else {
            vec![InputRun {
                text: text.to_string(),
                styles: self.styles_at_caret(),
            }]
          },
        })
        .collect(),
      blocks: Vec::new(),
      assets: Vec::new(),
    };
    self.insert_rich_fragment_paste_at_caret(&fragment, cx)
  }

  fn insert_rich_fragment_paste_at_caret(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    if self.selected_block.is_some() || !fragment.blocks.is_empty() || !fragment.assets.is_empty() || fragment.paragraphs.is_empty() {
      return false;
    }
    if self.authoritative_edit_controller.is_some() {
      let range = self.selection.normalized();
      if self.selection_crosses_object_blocks(range.clone()) {
        return false;
      }
      let Some(at) = self.authoritative_source_position(range.start) else {
        return false;
      };
      let mut operations = Vec::with_capacity(2);
      if !self.selection.is_caret() {
        let Some(end) = self.authoritative_source_position(range.end) else {
          return false;
        };
        operations.push(AuthoritativeSourceOperation::DeleteText { start: at, end });
      }
      let new_paragraphs = fragment
        .paragraphs
        .iter()
        .skip(1)
        .map(|_| new_paragraph_id())
        .collect::<Vec<_>>();
      let last_len = fragment
        .paragraphs
        .last()
        .map(|paragraph| paragraph.runs.iter().map(|run| run.text.len()).sum())
        .unwrap_or(0);
      let after = if let Some(last) = new_paragraphs.last() {
        AuthoritativeSourcePosition {
          paragraph: *last,
          byte: last_len,
        }
      } else {
        AuthoritativeSourcePosition {
          paragraph: at.paragraph,
          byte: at.byte + last_len,
        }
      };
      let contains_text = fragment
        .paragraphs
        .iter()
        .any(|paragraph| paragraph.runs.iter().any(|run| !run.text.is_empty()));
      if contains_text || !new_paragraphs.is_empty() {
        operations.push(AuthoritativeSourceOperation::InsertParagraphFragment {
          at,
          paragraphs: fragment.paragraphs.clone(),
          new_paragraphs,
        });
      }
      if operations.is_empty() {
        return true;
      }
      return self.apply_authoritative_source_operations(
        operations,
        AuthoritativeSourceSelection {
          anchor: after,
          head: after,
        },
        cx,
      );
    }
    if !self.selection.is_caret() || fragment.paragraphs.len() != 1 {
      return false;
    }
    let Some(paragraph_id) = self
      .identity_map
      .paragraph_id(self.selection.head.paragraph)
    else {
      return false;
    };
    let paragraph = &fragment.paragraphs[0];
    if paragraph.runs.iter().all(|run| run.text.is_empty()) {
      return true;
    }

    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let offset = self.selection.head;
    let inserted_end = insert_rich_fragment_at(&mut self.document, offset, fragment);
    self.selection = EditorSelection {
      anchor: inserted_end,
      head: inserted_end,
    };
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::InsertRichFragment {
        offset,
        inserted_end,
        fragment: fragment.clone(),
      }],
      canonical_operations: canonical_insert_text_operations(paragraph_id, offset.byte, paragraph),
    });
    self.redo_stack.clear();
    self.layout_invalidation_hint = Some(offset.paragraph..offset.paragraph + 1);
    self.after_text_mutation(cx);
    self.mark_document_changed_with_reconcile(after_generation, false, cx);
    true
  }

  fn selected_table_cell_fragment(&self) -> Option<RichClipboardFragment> {
    let cell = self.selected_table_cell()?;
    if let (Some(range), Some(paragraph)) = (self.table_cell_selection_range(), self.selected_table_cell_paragraph()) {
      return Some(RichClipboardFragment {
        format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
        paragraphs: vec![input_paragraph_from_table_cell_range(paragraph, range)],
        blocks: Vec::new(),
        assets: Vec::new(),
      });
    }
    let paragraphs = cell
      .blocks
      .iter()
      .filter_map(|block| match block {
        TableCellBlock::Paragraph(paragraph) => Some(input_paragraph_from_table_cell_paragraph(paragraph)),
        TableCellBlock::Table(_) => None,
      })
      .collect::<Vec<_>>();
    (!paragraphs.is_empty()).then_some(RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs,
      blocks: Vec::new(),
      assets: Vec::new(),
    })
  }

  fn insert_plain_text_into_selected_table_cell(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return matches!(self.selected_block, Some(BlockSelection::TableCell { .. }));
    }
    let styles = self
      .selected_table_cell_paragraph()
      .map(|paragraph| table_cell_styles_at(paragraph, self.table_cell_caret))
      .unwrap_or_default();
    let paragraphs = normalized
      .split('\n')
      .map(|line| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: if line.is_empty() {
          Vec::new()
        } else {
          vec![InputRun {
            text: line.to_string(),
            styles,
          }]
        },
      })
      .collect::<Vec<_>>();
    self.insert_paragraphs_into_selected_table_cell(&paragraphs, cx)
  }

  fn insert_rich_fragment_into_selected_table_cell(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    if fragment
      .blocks
      .iter()
      .any(|block| !matches!(block, InputBlock::Paragraph(_)))
    {
      return false;
    }
    if !fragment.blocks.is_empty() {
      let paragraphs = fragment
        .blocks
        .iter()
        .filter_map(|block| match block {
          InputBlock::Paragraph(paragraph) => Some(paragraph.clone()),
          InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
        })
        .collect::<Vec<_>>();
      return self.insert_paragraphs_into_selected_table_cell(&paragraphs, cx);
    }
    self.insert_paragraphs_into_selected_table_cell(&fragment.paragraphs, cx)
  }

  fn insert_paragraphs_into_selected_table_cell(&mut self, paragraphs: &[InputParagraph], cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell {
      block_ix,
      row_ix,
      cell_ix,
    }) = self.selected_block
    else {
      return false;
    };
    if paragraphs.is_empty() {
      return true;
    }
    let current_paragraph_ix = self.table_cell_block_ix;
    let caret = self.table_cell_caret;
    if self.authoritative_edit_controller.is_some() {
      let Some(paragraph) = self.table_cell_paragraph_id(block_ix, row_ix, cell_ix, current_paragraph_ix) else {
        return false;
      };
      let range = self.table_cell_selection_range();
      let at = AuthoritativeSourcePosition {
        paragraph,
        byte: range.as_ref().map_or(caret, |range| range.start),
      };
      let mut operations = Vec::with_capacity(2);
      if let Some(range) = range {
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
      let new_paragraphs = paragraphs
        .iter()
        .skip(1)
        .map(|_| new_paragraph_id())
        .collect::<Vec<_>>();
      let last_len = paragraphs
        .last()
        .map(|paragraph| paragraph.runs.iter().map(|run| run.text.len()).sum())
        .unwrap_or(0);
      let contains_text = paragraphs
        .iter()
        .any(|paragraph| paragraph.runs.iter().any(|run| !run.text.is_empty()));
      if contains_text || !new_paragraphs.is_empty() {
        operations.push(AuthoritativeSourceOperation::InsertParagraphFragment {
          at,
          paragraphs: paragraphs.to_vec(),
          new_paragraphs,
        });
      }
      if operations.is_empty() {
        return true;
      }
      let Some(planned_selection) = self.authoritative_source_selection(&self.selection) else {
        return false;
      };
      self.apply_authoritative_source_operations(operations, planned_selection, cx);
      self.table_cell_block_ix = current_paragraph_ix + paragraphs.len().saturating_sub(1);
      self.table_cell_caret = at.byte + usize::from(paragraphs.len() == 1) * last_len;
      if paragraphs.len() > 1 {
        self.table_cell_caret = last_len;
      }
      self.table_cell_anchor = self.table_cell_caret;
      cx.notify();
      return true;
    }
    let mut new_caret = None;
    self.edit_selected_table(cx, |table| {
      let Some(cell) = table
        .rows
        .get_mut(row_ix)
        .and_then(|row| row.cells.get_mut(cell_ix))
      else {
        return;
      };
      new_caret = insert_table_cell_paragraphs_at(cell, current_paragraph_ix, caret, paragraphs);
    });
    if let Some((paragraph_ix, byte)) = new_caret {
      self.table_cell_block_ix = paragraph_ix;
      self.table_cell_caret = byte;
      cx.notify();
    }
    true
  }

  fn selected_table_cell_text(&self) -> Option<String> {
    self
      .selected_table_cell_paragraph()
      .map(|paragraph| paragraph.text.clone())
  }

  fn selected_equation_source(&self) -> Option<String> {
    let BlockSelection::Equation(block_ix) = self.selected_block? else {
      return None;
    };
    let Block::Equation(equation) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    Some(equation.source.to_string())
  }

  fn equation_source_selection_range(&self) -> Option<Range<usize>> {
    if self.equation_source_anchor == self.equation_source_caret {
      return None;
    }
    Some(self.equation_source_anchor.min(self.equation_source_caret)..self.equation_source_anchor.max(self.equation_source_caret))
  }

  fn selected_equation_source_text(&self) -> Option<String> {
    let source = self.selected_equation_source()?;
    let range = self.equation_source_selection_range()?;
    Some(source.get(range).unwrap_or("").to_string())
  }

  fn equation_source_selection_for_render(&self, block_ix: usize) -> Option<EquationSourceSelection> {
    if self.selected_block != Some(BlockSelection::Equation(block_ix)) {
      return None;
    }
    Some(EquationSourceSelection {
      anchor: self.equation_source_anchor,
      caret: self.equation_source_caret,
      caret_visible: self.caret_visible,
    })
  }

  pub(super) fn table_cell_selection_range(&self) -> Option<Range<usize>> {
    if self.table_cell_anchor == self.table_cell_caret {
      return None;
    }
    Some(self.table_cell_anchor.min(self.table_cell_caret)..self.table_cell_anchor.max(self.table_cell_caret))
  }

  fn selected_table_cell_paragraph(&self) -> Option<&TableCellParagraph> {
    let cell = self.selected_table_cell()?;
    let paragraph_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix)?;
    let TableCellBlock::Paragraph(paragraph) = cell.blocks.get(paragraph_ix)? else {
      return None;
    };
    Some(paragraph)
  }

  fn selected_table_cell(&self) -> Option<&TableCell> {
    let BlockSelection::TableCell { block_ix, row_ix, cell_ix } = self.selected_block? else {
      return None;
    };
    let Block::Table(table) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    table.rows.get(row_ix)?.cells.get(cell_ix)
  }

  fn adjacent_selected_table_cell_paragraph(&self, forward: bool) -> Option<(usize, usize)> {
    let cell = self.selected_table_cell()?;
    let current_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix)?;
    let paragraph_ix = if forward {
      next_table_cell_paragraph_block_ix(cell, current_ix)?
    } else {
      previous_table_cell_paragraph_block_ix(cell, current_ix)?
    };
    let TableCellBlock::Paragraph(paragraph) = cell.blocks.get(paragraph_ix)? else {
      return None;
    };
    Some((paragraph_ix, paragraph.text.len()))
  }

  fn clear_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      paragraph.text.clear();
      paragraph.paragraph.byte_range = 0..0;
      paragraph.paragraph.runs.clear();
      paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
    });
    true
  }

  fn move_selected_table_cell(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return false;
    };
    let mut positions = Vec::new();
    for (row, table_row) in table.rows.iter().enumerate() {
      for cell in 0..table_row.cells.len() {
        positions.push((row, cell));
      }
    }
    let Some(current) = positions
      .iter()
      .position(|&(row, cell)| row == row_ix && cell == cell_ix)
    else {
      return false;
    };
    let next = if forward { current + 1 } else { current.saturating_sub(1) };
    let Some(&(row_ix, cell_ix)) = positions.get(next) else {
      return false;
    };
    self.selected_block = Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix });
    self.table_cell_block_ix = 0;
    self.table_cell_caret = self
      .selected_table_cell_text()
      .map(|text| text.len())
      .unwrap_or(0);
    cx.notify();
    true
  }

}
