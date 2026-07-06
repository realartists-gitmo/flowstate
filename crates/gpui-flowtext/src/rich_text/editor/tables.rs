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
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    let columns = updated
      .rows
      .iter()
      .map(|row| row.cells.len())
      .max()
      .unwrap_or(1)
      .max(updated.column_widths.len())
      .max(1);
    let insert_ix = target_row
      .map(|row| row + 1)
      .unwrap_or(updated.rows.len())
      .min(updated.rows.len());
    let row = default_table_row(columns);
    updated.rows.insert(insert_ix, row.clone());
    updated.version = updated.version.wrapping_add(1);
    let semantic_commands = if let Some(table_id) = self.semantic_block_id(block_ix) {
      vec![SemanticEditCommand::InsertTableRow {
        table: table_id,
        row_ix: insert_ix,
        row: input_table_row_from_table_row(&row),
      }]
    } else {
      self.missing_table_identity_semantic_commands(block_ix, "row insert")
    };
    self.finish_selected_table_edit(block_ix, table, updated, semantic_commands, cx);
  }

  pub fn delete_last_row_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    if table.rows.len() <= 1 {
      return;
    }
    let row_ix = target_row
      .unwrap_or(table.rows.len() - 1)
      .min(table.rows.len() - 1);
    let mut updated = table.clone();
    updated.rows.remove(row_ix);
    updated.version = updated.version.wrapping_add(1);
    let semantic_commands = if let Some(table_id) = self.semantic_block_id(block_ix) {
      vec![SemanticEditCommand::DeleteTableRow { table: table_id, row_ix }]
    } else {
      self.missing_table_identity_semantic_commands(block_ix, "row delete")
    };
    self.finish_selected_table_edit(block_ix, table, updated, semantic_commands, cx);
  }

  pub fn insert_column_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    let insert_ix = target_column
      .map(|column| column + 1)
      .unwrap_or(updated.column_widths.len())
      .min(updated.column_widths.len());
    let width = TableColumnWidth::Fraction(1);
    updated.column_widths.insert(insert_ix, width.clone());
    let mut inserted_cells = Vec::with_capacity(updated.rows.len());
    for row in &mut updated.rows {
      let cell = default_table_cell();
      inserted_cells.push(input_table_cell_from_table_cell(&cell));
      let cell_ix = insert_ix.min(row.cells.len());
      row.cells.insert(cell_ix, cell);
    }
    updated.version = updated.version.wrapping_add(1);
    let semantic_commands = if let Some(table_id) = self.semantic_block_id(block_ix) {
      vec![SemanticEditCommand::InsertTableColumn {
        table: table_id,
        column_ix: insert_ix,
        width: input_table_column_width_from_table_column_width(&width),
        cells: inserted_cells,
      }]
    } else {
      self.missing_table_identity_semantic_commands(block_ix, "column insert")
    };
    self.finish_selected_table_edit(block_ix, table, updated, semantic_commands, cx);
  }

  pub fn delete_last_column_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    let mut structured_column_ix = None;
    if updated.column_widths.len() > 1 {
      let column_ix = target_column
        .unwrap_or(updated.column_widths.len() - 1)
        .min(updated.column_widths.len() - 1);
      updated.column_widths.remove(column_ix);
      for row in &mut updated.rows {
        if row.cells.len() > 1 {
          let cell_ix = column_ix.min(row.cells.len() - 1);
          row.cells.remove(cell_ix);
        }
      }
      structured_column_ix = Some(column_ix);
    } else {
      for row in &mut updated.rows {
        if row.cells.len() > 1 {
          let cell_ix = target_column
            .unwrap_or(row.cells.len() - 1)
            .min(row.cells.len() - 1);
          row.cells.remove(cell_ix);
        }
      }
    }
    if updated == table {
      return;
    }
    updated.version = updated.version.wrapping_add(1);
    let semantic_commands = if let (Some(table_id), Some(column_ix)) = (self.semantic_block_id(block_ix), structured_column_ix) {
      vec![SemanticEditCommand::DeleteTableColumn { table: table_id, column_ix }]
    } else {
      self.missing_table_identity_semantic_commands(block_ix, "column delete")
    };
    self.finish_selected_table_edit(block_ix, table, updated, semantic_commands, cx);
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
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    if target_column >= table.column_widths.len() {
      return;
    }
    let mut updated = table.clone();
    let current = match updated.column_widths[target_column] {
      TableColumnWidth::FixedPx(width) => width as i32,
      TableColumnWidth::Fraction(_) | TableColumnWidth::Auto => 120,
    };
    updated.column_widths[target_column] = TableColumnWidth::FixedPx((current + delta_px).clamp(32, 1600) as u32);
    if updated == table {
      return;
    }
    updated.version = updated.version.wrapping_add(1);
    let semantic_commands = if let Some(table_id) = self.semantic_block_id(block_ix) {
      vec![SemanticEditCommand::SetTableColumnWidth {
        table: table_id,
        column_ix: target_column,
        width: input_table_column_width_from_table_column_width(&updated.column_widths[target_column]),
      }]
    } else {
      self.missing_table_identity_semantic_commands(block_ix, "column width")
    };
    self.finish_selected_table_edit(block_ix, table, updated, semantic_commands, cx);
  }

  fn finish_selected_table_edit(
    &mut self,
    block_ix: usize,
    before_table: TableBlock,
    updated: TableBlock,
    semantic_commands: Vec<SemanticEditCommand>,
    cx: &mut Context<Self>,
  ) {
    self.finish_table_edit(
      block_ix,
      Block::Table(before_table),
      Block::Table(updated),
      semantic_commands,
      cx,
    );
  }

  fn finish_table_edit(
    &mut self,
    block_ix: usize,
    before: Block,
    after: Block,
    semantic_commands: Vec<SemanticEditCommand>,
    cx: &mut Context<Self>,
  ) {
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.record_local_history(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      semantic_commands: semantic_commands.clone(),
    });
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
  }

  fn missing_table_identity_semantic_commands(&self, block_ix: usize, operation: &'static str) -> Vec<SemanticEditCommand> {
    tracing::warn!(block_ix, operation, "dropping table semantic command: projection block has no durable id; local and canonical state will diverge until repair");
    Vec::new()
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
