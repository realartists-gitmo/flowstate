#[hotpath::measure_all]
impl RichTextEditor {
  fn insert_text_into_selected_object_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    self.insert_text_into_selected_table_cell(text, cx)
  }

  /// B-S4: build a positional, hash-pinned cell-text intent for the selected
  /// cell and commit it. Returns `None` when the cell can't take the fine
  /// path (nested tables, missing ids) — callers fall back to the whole-cell
  /// `ReplaceCell` rewrite.
  /// B-S7: apply run styles to EVERY cell of the rectangular range — one
  /// undo group of per-cell mark ops (the B-S4 fine path; cells that refuse
  /// it — nested tables — are skipped, and the skip is reported). Returns
  /// false when no multi-cell range is active.
  pub fn apply_run_styles_to_cell_range(&mut self, styles: crate::RunStyles, cx: &mut Context<Self>) -> bool {
    let Some(range) = self.cell_range.filter(CellRangeSelection::is_multi) else {
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(range.block_ix) else {
      return false;
    };
    // Collect per-cell ops FIRST (immutable pass), then commit as one group.
    let mut ops = Vec::new();
    let mut skipped = 0usize;
    for row_ix in range.rows() {
      let Some(row) = table.rows.get(row_ix) else { continue };
      for cell_ix in range.cells() {
        let Some(cell) = row.cells.get(cell_ix) else { continue };
        if cell.blocks.iter().any(|block| matches!(block, TableCellBlock::Table(_))) {
          skipped += 1;
          continue;
        }
        let paragraph_texts: Vec<&str> = cell
          .blocks
          .iter()
          .filter_map(|block| match block {
            TableCellBlock::Paragraph(paragraph) => Some(paragraph.text.as_str()),
            TableCellBlock::Table(_) => None,
          })
          .collect();
        let last_paragraph = paragraph_texts.len().saturating_sub(1);
        let last_len = paragraph_texts.last().map_or(0, |text| text.len());
        if paragraph_texts.iter().all(|text| text.is_empty()) {
          continue;
        }
        let flow = crate::local_intents::table_cell_flow_string(&paragraph_texts);
        ops.push(crate::local_intents::TableCellTextIntent {
          table: match self.semantic_block_id(range.block_ix) {
            Some(id) => id,
            None => return false,
          },
          cell: CellId::from_coordinate(cell.row_id, cell.column_id),
          expected_text_hash: crate::local_intents::table_cell_text_hash(&flow),
          op: crate::local_intents::TableCellTextOp::SetMarks {
            start: (0, 0),
            end: (last_paragraph, last_len),
            styles,
          },
        });
      }
    }
    if ops.is_empty() {
      return false;
    }
    let grouped = ops.len() > 1;
    if grouped {
      self.begin_undo_group();
    }
    for intent in ops {
      let _ = self.write_intent(crate::local_intents::LocalIntent::TableCellText(intent), cx);
    }
    if grouped {
      self.end_undo_group();
    }
    if skipped > 0 {
      cx.emit(EditorEvent::Refused {
        message: format!("{skipped} cell(s) with nested tables were left unstyled.").into(),
      });
    }
    cx.notify();
    true
  }

  /// B-S7: the rectangular range as a clipboard fragment — one sub-table
  /// block carrying the ranged rows/cells (the write path mints fresh
  /// identity on paste-as-block) plus a tab/newline plain-text mirror.
  pub(super) fn cell_range_fragment(&self) -> Option<(RichClipboardFragment, String)> {
    let range = self.cell_range.filter(CellRangeSelection::is_multi)?;
    let Block::Table(table) = self.document.blocks.get(range.block_ix)? else {
      return None;
    };
    let mut rows = Vec::new();
    let mut text_rows = Vec::new();
    for row_ix in range.rows() {
      let row = table.rows.get(row_ix)?;
      let mut cells = Vec::new();
      let mut texts = Vec::new();
      for cell_ix in range.cells() {
        let cell = row.cells.get(cell_ix)?;
        cells.push(input_table_cell_from_table_cell(cell));
        texts.push(
          cell
            .blocks
            .iter()
            .filter_map(|block| match block {
              TableCellBlock::Paragraph(paragraph) => Some(paragraph.text.as_str()),
              TableCellBlock::Table(_) => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        );
      }
      rows.push(crate::InputTableRow { id: row.id, cells });
      text_rows.push(texts.join("	"));
    }
    let columns = range
      .cells()
      .filter_map(|cell_ix| table.columns.get(cell_ix))
      .map(|column| crate::InputTableColumn {
        id: column.id,
        width: input_table_column_width_from_table_column_width(&column.width),
      })
      .collect();
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: Vec::new(),
      blocks: vec![crate::InputBlock::Table(crate::InputTableBlock {
        rows,
        columns,
        style: crate::InputTableStyle { header_row: false },
      })],
      assets: Vec::new(),
    };
    Some((fragment, text_rows.join("
")))
  }

  /// B-S7: pasting a table fragment while a cell is selected overlays the
  /// fragment's grid starting at that cell — per-target `ReplaceCell` in one
  /// undo group; cells past the table's edge are skipped OUT LOUD.
  pub(super) fn paste_table_fragment_as_cell_range(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    if !fragment.paragraphs.is_empty() || fragment.blocks.len() != 1 {
      return false;
    }
    let crate::InputBlock::Table(source) = &fragment.blocks[0] else {
      return false;
    };
    let Some(Block::Table(target)) = self.document.blocks.get(block_ix) else {
      return false;
    };
    let Some(table_id) = self.semantic_block_id(block_ix) else {
      return false;
    };
    let mut ops = Vec::new();
    let mut skipped = 0usize;
    for (row_offset, source_row) in source.rows.iter().enumerate() {
      let Some(target_row) = target.rows.get(row_ix + row_offset) else {
        skipped += source_row.cells.len();
        continue;
      };
      for (cell_offset, source_cell) in source_row.cells.iter().enumerate() {
        let Some(target_cell) = target_row.cells.get(cell_ix + cell_offset) else {
          skipped += 1;
          continue;
        };
        ops.push(crate::local_intents::TableIntent::ReplaceCell {
          table: table_id,
          row: target_cell.row_id,
          column: target_cell.column_id,
          cell: crate::InputTableCell {
            id: target_cell.id,
            row_id: target_cell.row_id,
            column_id: target_cell.column_id,
            blocks: source_cell.blocks.clone(),
            row_span: source_cell.row_span,
            col_span: source_cell.col_span,
          },
        });
      }
    }
    if ops.is_empty() {
      return false;
    }
    let grouped = ops.len() > 1;
    if grouped {
      self.begin_undo_group();
    }
    for op in ops {
      let _ = self.write_intent(crate::local_intents::LocalIntent::Table(op), cx);
    }
    if grouped {
      self.end_undo_group();
    }
    if skipped > 0 {
      cx.emit(EditorEvent::Refused {
        message: format!("The pasted range didn't fit — {skipped} cell(s) past the table's edge were skipped.").into(),
      });
    }
    cx.notify();
    true
  }

  /// B-S7: merge the rectangular cell range — ONE `SetCellSpan` on the
  /// range's top-left cell (topology hides the covered cells; the CRDT op
  /// existed since §P2b, buried without UI). Refuses out loud on ragged
  /// ranges the topology can't express.
  pub fn merge_cell_range(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(range) = self.cell_range.filter(CellRangeSelection::is_multi) else {
      cx.emit(EditorEvent::Refused {
        message: "Select a rectangle of cells first (drag across cells or Shift+arrows).".into(),
      });
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(range.block_ix) else {
      return false;
    };
    let top_row = *range.rows().start();
    let left_cell = *range.cells().start();
    let Some(cell) = table.rows.get(top_row).and_then(|row| row.cells.get(left_cell)) else {
      return false;
    };
    let Some(table_id) = self.semantic_block_id(range.block_ix) else {
      return false;
    };
    let row_span = u16::try_from(range.rows().count()).unwrap_or(u16::MAX);
    let column_span = u16::try_from(range.cells().count()).unwrap_or(u16::MAX);
    let merged = self
      .write_intent(
        crate::local_intents::LocalIntent::Table(crate::local_intents::TableIntent::SetCellSpan {
          table: table_id,
          row: cell.row_id,
          column: cell.column_id,
          row_span,
          column_span,
        }),
        cx,
      )
      .is_some();
    if merged {
      self.cell_range = None;
      cx.notify();
    }
    merged
  }

  /// B-S7: split the selected merged cell back to 1×1 spans.
  pub fn split_selected_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_id, column_id, .. }) = self.selected_block else {
      cx.emit(EditorEvent::Refused {
        message: "Select the merged cell first.".into(),
      });
      return false;
    };
    let Some(table_id) = self.semantic_block_id(block_ix) else {
      return false;
    };
    self
      .write_intent(
        crate::local_intents::LocalIntent::Table(crate::local_intents::TableIntent::SetCellSpan {
          table: table_id,
          row: row_id,
          column: column_id,
          row_span: 1,
          column_span: 1,
        }),
        cx,
      )
      .is_some()
  }

  fn write_table_cell_text(&mut self, op: crate::local_intents::TableCellTextOp, cx: &mut Context<Self>) -> Option<bool> {
    let Some(BlockSelection::TableCell {
      block_ix, row_ix, cell_ix, ..
    }) = self.selected_block
    else {
      return None;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return None;
    };
    let cell = table.rows.get(row_ix)?.cells.get(cell_ix)?;
    // Nested tables make positional addressing unsound — whole-cell path.
    if cell.blocks.iter().any(|block| matches!(block, TableCellBlock::Table(_))) {
      return None;
    }
    let paragraph_texts: Vec<&str> = cell
      .blocks
      .iter()
      .filter_map(|block| match block {
        TableCellBlock::Paragraph(paragraph) => Some(paragraph.text.as_str()),
        TableCellBlock::Table(_) => None,
      })
      .collect();
    let flow = crate::local_intents::table_cell_flow_string(&paragraph_texts);
    let table_id = self.semantic_block_id(block_ix)?;
    let intent = crate::local_intents::TableCellTextIntent {
      table: table_id,
      cell: CellId::from_coordinate(cell.row_id, cell.column_id),
      expected_text_hash: crate::local_intents::table_cell_text_hash(&flow),
      op,
    };
    Some(self.write_intent(LocalIntent::TableCellText(intent), cx).is_some())
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
    // B-S4: the fine-grained path — one insert op (plus a preceding delete op
    // for selection-replace typing, grouped as one undo unit). Concurrent
    // same-cell typing now merges char-level instead of LWW-losing a peer.
    let paragraph_ix = self.table_cell_block_ix;
    // `None` = cell ineligible for the fine path (fall back to whole-cell);
    // `Some(committed)` = the fine path handled it (even a rejected op stays
    // on the fine path — falling back after a partial commit would
    // double-apply).
    let fine = 'fine: {
      let grouped = selection_range.is_some();
      if grouped {
        self.begin_undo_group();
      }
      let mut deleted_ok = true;
      if let Some(range) = selection_range.clone() {
        match self.write_table_cell_text(
          crate::local_intents::TableCellTextOp::Delete {
            start: (paragraph_ix, range.start),
            end: (paragraph_ix, range.end),
          },
          cx,
        ) {
          Some(true) => {},
          Some(false) => deleted_ok = false,
          None => {
            if grouped {
              self.end_undo_group();
            }
            break 'fine None;
          },
        }
      }
      let result = if deleted_ok {
        self.write_table_cell_text(
          crate::local_intents::TableCellTextOp::Insert {
            paragraph: paragraph_ix,
            byte: insert_at,
            text: text.to_string(),
            style_override: (styles != RunStyles::default()).then_some(styles),
          },
          cx,
        )
      } else {
        Some(false)
      };
      if grouped {
        self.end_undo_group();
      }
      result
    };
    if let Some(committed) = fine {
      if committed {
        self.table_cell_caret = insert_at.saturating_add(text.len());
        self.table_cell_anchor = self.table_cell_caret;
      }
      return true;
    }
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
    // B-S4: fine-grained boundary split.
    let inherited_style = self
      .selected_table_cell_paragraph()
      .map(|paragraph| paragraph.paragraph.style)
      .unwrap_or(ParagraphStyle::Normal);
    if let Some(committed) = self.write_table_cell_text(
      crate::local_intents::TableCellTextOp::Split {
        at: (paragraph_ix, caret),
        inherited_style,
      },
      cx,
    ) {
      if committed {
        self.table_cell_block_ix = paragraph_ix + 1;
        self.table_cell_caret = 0;
        cx.notify();
      }
      return true;
    }
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

  fn backspace_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    let caret = self.table_cell_caret;
    // B-S4: fine-grained path — a one-char delete op / a boundary join op.
    if caret == 0 && self.table_cell_block_ix > 0 {
      let second = self.table_cell_block_ix;
      let previous_len = self
        .selected_table_cell_paragraph_text(second - 1)
        .map(|text| text.len())
        .unwrap_or(0);
      if let Some(committed) = self.write_table_cell_text(crate::local_intents::TableCellTextOp::Join { second }, cx) {
        if committed {
          self.table_cell_block_ix = second - 1;
          self.table_cell_caret = previous_len;
          cx.notify();
        }
        return true;
      }
    } else if caret > 0
      && let Some(text) = self.selected_table_cell_text()
    {
      let caret_clamped = caret.min(text.len());
      let prev = text[..caret_clamped]
        .char_indices()
        .next_back()
        .map(|(byte, _)| byte)
        .unwrap_or(0);
      let paragraph_ix = self.table_cell_block_ix;
      if let Some(committed) = self.write_table_cell_text(
        crate::local_intents::TableCellTextOp::Delete {
          start: (paragraph_ix, prev),
          end: (paragraph_ix, caret_clamped),
        },
        cx,
      ) {
        if committed {
          self.table_cell_caret = prev;
        }
        return true;
      }
    }
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
      // B-S4: fine-grained one-char delete.
      let paragraph_ix = self.table_cell_block_ix;
      if let Some(committed) = self.write_table_cell_text(
        crate::local_intents::TableCellTextOp::Delete {
          start: (paragraph_ix, caret),
          end: (paragraph_ix, next),
        },
        cx,
      ) {
        if committed {
          self.table_cell_caret = caret;
        }
        return true;
      }
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

  /// B-S8: the composer's commit — replace the WHOLE source of `equation`
  /// (one identity-addressed range intent; undo restores the prior source).
  pub fn replace_equation_source(&mut self, equation: BlockId, source: &str, cx: &mut Context<Self>) -> bool {
    let Some(block_ix) = self
      .document
      .blocks
      .iter()
      .enumerate()
      .position(|(block_ix, block)| matches!(block, Block::Equation(_)) && self.semantic_block_id(block_ix) == Some(equation))
    else {
      return false;
    };
    let Some(Block::Equation(current)) = self.document.blocks.get(block_ix) else {
      return false;
    };
    let len = current.source.len();
    self.edit_selected_equation_source_range(block_ix, 0..len, source, cx)
  }

  /// B-S8: ask the host to open the equation composer. With an equation
  /// selected this is a REOPEN (existing id + source + anchored frame);
  /// otherwise it is a compose-new request at the caret.
  pub fn request_equation_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(BlockSelection::Equation(block_ix)) = self.selected_block
      && let Some(Block::Equation(equation)) = self.document.blocks.get(block_ix)
    {
      let source: gpui::SharedString = equation.source.clone();
      let equation_id = self.semantic_block_id(block_ix);
      let anchor = self.equation_screen_bounds(block_ix, window, cx);
      cx.emit(EditorEvent::EquationComposerRequested {
        equation: equation_id,
        source,
        anchor,
      });
      return;
    }
    cx.emit(EditorEvent::EquationComposerRequested {
      equation: None,
      source: gpui::SharedString::default(),
      anchor: None,
    });
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
