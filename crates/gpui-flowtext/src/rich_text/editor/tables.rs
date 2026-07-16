#[hotpath::measure_all]
impl RichTextEditor {
  pub fn insert_default_table(&mut self, rows: usize, columns: usize, cx: &mut Context<Self>) {
    let rows = rows.clamp(1, 20);
    let columns = columns.clamp(1, 12);
    let column_ids: Vec<ColumnId> = (0..columns).map(|_| ColumnId(uuid::Uuid::new_v4().as_u128())).collect();
    let table = TableBlock {
      rows: (0..rows)
        .map(|_| default_table_row(RowId(uuid::Uuid::new_v4().as_u128()), &column_ids))
        .collect(),
      columns: column_ids
        .iter()
        .map(|&id| TableColumn {
          id,
          width: TableColumnWidth::Fraction(1),
        })
        .collect(),
      style: TableStyle { header_row: false },
      version: 0,
    };
    // Loro-first (spec §5): the table enters the document as one InsertObject
    // intent; the block's durable identity is minted by the write path while
    // the row/column ids above stay editor-minted (they ride in the payload).
    self.write_insert_object_at_caret(input_block_from_block(&Block::Table(table)), cx);
  }

  pub fn insert_row_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    let Some((block_ix, table_id)) = self.selected_table_identity() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return;
    };
    let insert_ix = target_row
      .map(|row| row + 1)
      .unwrap_or(table.rows.len())
      .min(table.rows.len());
    // Durable anchor: the row currently before the insertion point in the
    // projection (`None` when inserting at the head).
    let after_row = insert_ix
      .checked_sub(1)
      .and_then(|ix| table.rows.get(ix))
      .map(|row| row.id);
    let column_ids: Vec<ColumnId> = table.columns.iter().map(|column| column.id).collect();
    // The new row's identity is editor-minted (InputTableRow carries its id);
    // the runtime materializes the row and returns the projection patches.
    let row = default_table_row(RowId(uuid::Uuid::new_v4().as_u128()), &column_ids);
    self.write_intent(
      LocalIntent::Table(crate::local_intents::TableIntent::InsertRow {
        table: table_id,
        after_row,
        row: input_table_row_from_table_row(&row),
      }),
      cx,
    );
  }

  /// B-S1: deletes the SELECTED cell's row (falling back to the last row when
  /// only the table is selected) — the old name and menu label said "last"
  /// while doing exactly this, a user-facing lie.
  pub fn delete_row_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    let Some((block_ix, table_id)) = self.selected_table_identity() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return;
    };
    if table.rows.len() <= 1 {
      return;
    }
    let row_ix = target_row
      .unwrap_or(table.rows.len() - 1)
      .min(table.rows.len() - 1);
    let row = table.rows[row_ix].id;
    self.write_intent(LocalIntent::Table(crate::local_intents::TableIntent::DeleteRow { table: table_id, row }), cx);
  }

  pub fn insert_column_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    let Some((block_ix, table_id)) = self.selected_table_identity() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return;
    };
    let insert_ix = target_column
      .map(|column| column + 1)
      .unwrap_or(table.columns.len())
      .min(table.columns.len());
    // Durable anchor: the column currently before the insertion point (`None`
    // at the head). The new column's identity AND its empty cells are minted
    // by the runtime — the intent carries only the anchor and the width.
    let after_column = insert_ix
      .checked_sub(1)
      .and_then(|ix| table.columns.get(ix))
      .map(|column| column.id);
    self.write_intent(
      LocalIntent::Table(crate::local_intents::TableIntent::InsertColumn {
        table: table_id,
        after_column,
        width: InputTableColumnWidth::Fraction(1),
      }),
      cx,
    );
  }

  /// B-S1: deletes the SELECTED cell's column (falling back to the last).
  pub fn delete_column_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    let Some((block_ix, table_id)) = self.selected_table_identity() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return;
    };
    // Deleting the last remaining column is refused, symmetric with rows. (The
    // old positional per-row cell trim for degenerate id-less tables has no
    // intent form; topology repair is the materializer's job now.)
    if table.columns.len() <= 1 {
      return;
    }
    let column_ix = target_column
      .unwrap_or(table.columns.len() - 1)
      .min(table.columns.len() - 1);
    let column = table.columns[column_ix].id;
    self.write_intent(LocalIntent::Table(crate::local_intents::TableIntent::DeleteColumn { table: table_id, column }), cx);
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
    let Some((block_ix, table_id)) = self.selected_table_identity() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return;
    };
    // SetColumnWidth is identity-addressed: resolve the durable column id from
    // the projected table's columns at the selected index.
    let Some(target) = table.columns.get(target_column) else {
      return;
    };
    let current = match target.width {
      TableColumnWidth::FixedPx(width) => width as i32,
      TableColumnWidth::Fraction(_) | TableColumnWidth::Auto => 120,
    };
    let width = TableColumnWidth::FixedPx((current + delta_px).clamp(32, 1600) as u32);
    if width == target.width {
      return;
    }
    let column = target.id;
    self.write_intent(
      LocalIntent::Table(crate::local_intents::TableIntent::SetColumnWidth {
        table: table_id,
        column,
        width: input_table_column_width_from_table_column_width(&width),
      }),
      cx,
    );
  }

  /// Resolve the selected table's `(block_ix, durable BlockId)` pair. A table
  /// without a durable identity cannot be addressed by an intent; the edit is
  /// refused loudly instead of diverging local state (spec I-2).
  fn selected_table_identity(&self) -> Option<(usize, BlockId)> {
    let block_ix = self.selected_table_block_ix()?;
    match self.semantic_block_id(block_ix) {
      Some(table_id) => Some((block_ix, table_id)),
      None => {
        tracing::warn!(block_ix, "refusing table edit: projection block has no durable id, so no intent can address it");
        None
      },
    }
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
