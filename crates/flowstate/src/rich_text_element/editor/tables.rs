#[hotpath::measure_all]
impl RichTextEditor {
  pub fn insert_default_table(&mut self, rows: usize, columns: usize, cx: &mut Context<Self>) {
    let rows = rows.clamp(1, 20);
    let columns = columns.clamp(1, 12);
    let table = TableBlock {
      rows: (0..rows)
        .map(|_| TableRow {
          cells: (0..columns)
            .map(|_| TableCell {
              blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
                paragraph: Paragraph {
                  style: ParagraphStyle::Normal,
                  byte_range: 0..0,
                  runs: Vec::new(),
                  version: 0,
                },
                text: String::new(),
              })],
              row_span: 1,
              col_span: 1,
            })
            .collect(),
        })
        .collect(),
      column_widths: (0..columns)
        .map(|_| TableColumnWidth::Fraction(1))
        .collect(),
      style: TableStyle { header_row: false },
      version: 0,
    };
    self.insert_blocks_after_caret(vec![Block::Table(table)], cx);
  }

  pub fn insert_row_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      let columns = table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1)
        .max(table.column_widths.len())
        .max(1);
      let insert_ix = target_row
        .map(|row| row + 1)
        .unwrap_or(table.rows.len())
        .min(table.rows.len());
      table.rows.insert(insert_ix, default_table_row(columns));
    });
  }

  pub fn delete_last_row_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      if table.rows.len() > 1 {
        let row_ix = target_row
          .unwrap_or(table.rows.len() - 1)
          .min(table.rows.len() - 1);
        table.rows.remove(row_ix);
      }
    });
  }

  pub fn insert_column_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      let insert_ix = target_column
        .map(|column| column + 1)
        .unwrap_or(table.column_widths.len())
        .min(table.column_widths.len());
      table
        .column_widths
        .insert(insert_ix, TableColumnWidth::Fraction(1));
      for row in &mut table.rows {
        let cell_ix = insert_ix.min(row.cells.len());
        row.cells.insert(cell_ix, default_table_cell());
      }
    });
  }

  pub fn delete_last_column_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      if table.column_widths.len() > 1 {
        let column_ix = target_column
          .unwrap_or(table.column_widths.len() - 1)
          .min(table.column_widths.len() - 1);
        table.column_widths.remove(column_ix);
        for row in &mut table.rows {
          if row.cells.len() > 1 {
            let cell_ix = column_ix.min(row.cells.len() - 1);
            row.cells.remove(cell_ix);
          }
        }
      } else {
        for row in &mut table.rows {
          if row.cells.len() > 1 {
            let cell_ix = target_column
              .unwrap_or(row.cells.len() - 1)
              .min(row.cells.len() - 1);
            row.cells.remove(cell_ix);
          }
        }
      }
    });
  }

  pub fn widen_selected_table_column(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_table_column_width(24, cx);
  }

  pub fn narrow_selected_table_column(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_table_column_width(-24, cx);
  }

  fn adjust_selected_table_column_width(&mut self, delta_px: i32, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => cell_ix,
      _ => return,
    };
    self.edit_selected_table(cx, |table| {
      if target_column >= table.column_widths.len() {
        return;
      }
      let current = match table.column_widths[target_column] {
        TableColumnWidth::FixedPx(width) => width as i32,
        TableColumnWidth::Fraction(_) | TableColumnWidth::Auto => 120,
      };
      table.column_widths[target_column] = TableColumnWidth::FixedPx((current + delta_px).clamp(32, 1600) as u32);
    });
  }

  fn edit_selected_table(&mut self, cx: &mut Context<Self>, update: impl FnOnce(&mut TableBlock)) {
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    update(&mut updated);
    if updated == table {
      return;
    }
    updated.version = updated.version.wrapping_add(1);
    let before = Block::Table(table);
    let after = Block::Table(updated.clone());
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

  fn selected_table_block_ix(&self) -> Option<usize> {
    match self.selected_block {
      Some(BlockSelection::Table(block_ix) | BlockSelection::TableCell { block_ix, .. }) => Some(block_ix),
      _ => None,
    }
  }

  pub fn selected_block_kind(&self) -> Option<&'static str> {
    match self.selected_block {
      Some(BlockSelection::Image(_)) => Some("image"),
      Some(BlockSelection::Equation(_)) => Some("equation"),
      Some(BlockSelection::Table(_)) => Some("table"),
      Some(BlockSelection::TableCell { .. }) => Some("table-cell"),
      None => None,
    }
  }

}
