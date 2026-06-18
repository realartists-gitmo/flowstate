#[hotpath::measure_all]
impl RichTextEditor {
  fn select_block(&mut self, selection: BlockSelection, cx: &mut Context<Self>) {
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    self.selected_block = Some(selection);
    self.table_cell_block_ix = 0;
    self.table_cell_caret = self
      .selected_table_cell_text()
      .map(|text| text.len())
      .unwrap_or(0);
    self.table_cell_anchor = self.table_cell_caret;
    let equation_source_len = self
      .selected_equation_source()
      .map(|source| source.len())
      .unwrap_or(0);
    self.equation_source_caret = equation_source_len;
    self.equation_source_anchor = equation_source_len;
    self.selecting = false;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.scroll_block_into_view(block_ix);
    cx.notify();
  }

  #[cfg(test)]
  pub(super) fn select_equation_block_for_test(&mut self, block_ix: usize, cx: &mut Context<Self>) {
    self.select_block(BlockSelection::Equation(block_ix), cx);
  }

  fn select_block_from_click(
    &mut self,
    block_ix: usize,
    fallback: BlockSelection,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    window.focus(&self.focus_handle);
    if let Some((selection, paragraph_block_ix, byte)) = self.table_cell_selection_at(block_ix, position, window, cx) {
      self.selected_block = Some(selection);
      self.table_cell_block_ix = paragraph_block_ix;
      self.table_cell_anchor = byte;
      self.table_cell_caret = byte;
      self.selecting = false;
      self.drag_anchor = None;
      self.pending_text_drag = None;
      self.active_text_drag = None;
      self.goal_x = None;
      self.reset_caret_blink(cx);
      cx.notify();
    } else {
      self.select_block(fallback, cx);
      if matches!(fallback, BlockSelection::Equation(_)) {
        let byte = self
          .equation_source_byte_at(block_ix, position, window, cx)
          .unwrap_or(self.equation_source_caret);
        self.equation_source_anchor = byte;
        self.equation_source_caret = byte;
        self.reset_caret_blink(cx);
        cx.notify();
      }
    }
  }

  fn equation_source_byte_at(&mut self, block_ix: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
    let Block::Equation(equation) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    let width = self.current_layout_width();
    let block_top = self.block_top_for_index(block_ix)?;
    let layout = layout_structural_block_at(&self.document, block_ix, width, block_top, window, cx)?;
    let LaidOutBlock::Equation(object) = layout else {
      return None;
    };
    let viewport = self.scroll_handle.bounds();
    let document_point = point(position.x - viewport.left(), position.y - viewport.top() - self.scroll_handle.offset().y);
    let source_height = px(22.0);
    let source_top = object.bounds.bottom() - self.document.theme.paragraph_after - source_height;
    if document_point.y < source_top || document_point.y > object.bounds.bottom() {
      return None;
    }
    let strip_left = object.bounds.left() + px(8.0);
    let char_width = px(7.0);
    let delta: f32 = (document_point.x - strip_left).max(px(0.0)).into();
    let char_width: f32 = char_width.into();
    let target_char = (delta / char_width).round() as usize;
    Some(byte_for_char_index(&equation.source, target_char))
  }

  fn table_cell_selection_at(
    &mut self,
    block_ix: usize,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<(BlockSelection, usize, usize)> {
    let Block::Table(_) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    let width = self.current_layout_width();
    let block_top = self.block_top_for_index(block_ix)?;
    let layout = layout_structural_block_at(&self.document, block_ix, width, block_top, window, cx)?;
    let LaidOutBlock::Table(table) = layout else {
      return None;
    };
    let viewport = self.scroll_handle.bounds();
    let document_point = point(position.x - viewport.left(), position.y - viewport.top() - self.scroll_handle.offset().y);
    for (row_ix, row) in table.rows.iter().enumerate() {
      for (cell_ix, cell) in row.cells.iter().enumerate() {
        if cell.bounds.contains(&document_point) {
          let selection = BlockSelection::TableCell { block_ix, row_ix, cell_ix };
          let mut fallback = (selection, 0, 0);
          for block in &cell.blocks {
            if let LaidOutBlock::Paragraph(paragraph) = block {
              fallback = (selection, paragraph.index, paragraph.len);
              if document_point.y <= paragraph.bottom {
                let offset = paragraph.hit_test(document_point);
                return Some((selection, paragraph.index, offset.byte));
              }
            }
          }
          return Some(fallback);
        }
      }
    }
    None
  }

  fn start_table_column_resize_if_hit(&mut self, block_ix: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some((column_ix, widths, before)) = self.table_column_resize_hit_at(block_ix, position, window, cx) else {
      return false;
    };
    window.focus(&self.focus_handle);
    self.selected_block = Some(BlockSelection::Table(block_ix));
    self.table_column_resize_drag = Some(TableColumnResizeDrag {
      block_ix,
      column_ix,
      start_position: position,
      start_widths: widths,
      before,
    });
    self.selecting = false;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.goal_x = None;
    cx.notify();
    true
  }

  fn table_column_resize_hit_at(
    &mut self,
    block_ix: usize,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<(usize, Vec<u32>, TableBlock)> {
    let Block::Table(table) = self.document.blocks.get(block_ix)?.clone() else {
      return None;
    };
    let width = self.current_layout_width();
    let block_top = self.block_top_for_index(block_ix)?;
    let layout = layout_structural_block_at(&self.document, block_ix, width, block_top, window, cx)?;
    let LaidOutBlock::Table(laid_out) = layout else {
      return None;
    };
    let viewport = self.scroll_handle.bounds();
    let document_point = point(position.x - viewport.left(), position.y - viewport.top() - self.scroll_handle.offset().y);
    if !laid_out.bounds.contains(&document_point) {
      return None;
    }

    let tolerance = 5.0;
    let first_row = laid_out.rows.first()?;
    let data_row = table.rows.first()?;
    let mut logical_column_ix = 0usize;
    for (cell_ix, cell_layout) in first_row.cells.iter().enumerate() {
      let span = data_row
        .cells
        .get(cell_ix)
        .map(|cell| cell.col_span.max(1) as usize)
        .unwrap_or(1);
      let border_column_ix = logical_column_ix.saturating_add(span).saturating_sub(1);
      let delta: f32 = (document_point.x - cell_layout.bounds.right()).into();
      if delta.abs() <= tolerance && border_column_ix < table_column_count(&table) {
        return Some((border_column_ix, fixed_table_column_widths_from_layout(&table, &laid_out), table));
      }
      logical_column_ix = logical_column_ix.saturating_add(span);
    }
    None
  }

  fn update_table_column_resize_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.table_column_resize_drag.clone() else {
      return false;
    };
    let Some(Block::Table(table)) = Arc::make_mut(&mut self.document.blocks).get_mut(drag.block_ix) else {
      self.table_column_resize_drag = None;
      return true;
    };
    let delta: f32 = (position.x - drag.start_position.x).into();
    let column_count = drag
      .start_widths
      .len()
      .max(table_column_count(table))
      .max(1);
    while table.column_widths.len() < column_count {
      table.column_widths.push(TableColumnWidth::FixedPx(120));
    }
    for (ix, width) in drag.start_widths.iter().copied().enumerate() {
      if ix < table.column_widths.len() {
        table.column_widths[ix] = TableColumnWidth::FixedPx(width);
      }
    }
    let Some(start_width) = drag.start_widths.get(drag.column_ix).copied() else {
      self.table_column_resize_drag = None;
      return true;
    };
    table.column_widths[drag.column_ix] = TableColumnWidth::FixedPx((start_width as f32 + delta).clamp(32.0, 1600.0).round() as u32);
    table.version = drag.before.version.wrapping_add(1);
    self.invalidate_document_layout_caches();
    cx.notify();
    true
  }

  fn finish_table_column_resize_drag(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.table_column_resize_drag.take() else {
      return false;
    };
    let Some(Block::Table(after)) = self.document.blocks.get(drag.block_ix).cloned() else {
      cx.notify();
      return true;
    };
    if after == drag.before {
      cx.notify();
      return true;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = vec![SemanticEditCommand::ReplaceBlock {
      block: self.identity_map.block_id(drag.block_ix),
    }];
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock {
        block_ix: drag.block_ix,
        before: Block::Table(drag.before),
        after: Block::Table(after),
      }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
    true
  }

  fn block_top_for_index(&self, block_ix: usize) -> Option<Pixels> {
    if let Some(cache) = &self.item_sizes_cache
      && self.height_prefix_index.len() == cache.item_count
      && let Some(range) = cache.block_item_ranges.get(block_ix)
    {
      return Some(self.height_prefix_index.item_top(range.start));
    }
    None
  }

  fn scroll_block_into_view(&self, block_ix: usize) {
    let Some(sizes) = &self.item_sizes_cache else {
      return;
    };
    let Some(row_height) = sizes.block_heights.get(block_ix).copied() else {
      return;
    };
    let Some(top) = self.block_top_for_index(block_ix) else {
      return;
    };
    let viewport = self.scroll_handle.bounds();
    let rect = Bounds::new(
      point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + top),
      size(viewport.size.width, row_height),
    );
    scroll_rect_into_view(&self.scroll_handle, rect, px(8.0));
  }

  fn clear_block_selection(&mut self) {
    self.selected_block = None;
    self.table_cell_block_ix = 0;
    self.table_cell_anchor = 0;
    self.table_cell_caret = 0;
    self.equation_source_anchor = 0;
    self.equation_source_caret = 0;
  }

  fn selected_block_fragment(&self) -> Option<RichClipboardFragment> {
    let selection = self.selected_block?;
    if matches!(selection, BlockSelection::TableCell { .. }) {
      return None;
    }
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    let block = self.document.blocks.get(block_ix)?;
    let mut assets = Vec::new();
    collect_block_assets(block, &self.document.assets, &mut assets);
    Some(RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: Vec::new(),
      blocks: vec![input_block_from_block(block)],
      assets,
    })
  }

  fn selected_ordered_fragment(&self, range: Range<DocumentOffset>) -> Option<RichClipboardFragment> {
    if !self.document_has_object_blocks() {
      return None;
    }
    let start_block = self.block_ix_for_paragraph(range.start.paragraph)?;
    let end_block = self.block_ix_for_paragraph(range.end.paragraph)?;
    let has_object = self.document.blocks[start_block.min(end_block)..=start_block.max(end_block)]
      .iter()
      .any(|block| !matches!(block, Block::Paragraph(_)));
    if !has_object {
      return None;
    }
    let mut blocks = Vec::new();
    let mut assets = Vec::new();
    for block_ix in start_block..=end_block {
      match self.document.blocks.get(block_ix)? {
        Block::Paragraph(_) => {
          let Some(paragraph_ix) = self.paragraph_ix_for_block(block_ix) else {
            continue;
          };
          if paragraph_ix < range.start.paragraph || paragraph_ix > range.end.paragraph {
            continue;
          }
          let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
          let end = if paragraph_ix == range.end.paragraph {
            range.end.byte
          } else {
            paragraph_text_len(&self.document.paragraphs[paragraph_ix])
          };
          if start < end || (paragraph_ix > range.start.paragraph && paragraph_ix < range.end.paragraph) {
            blocks.push(InputBlock::Paragraph(input_paragraph_from_document_range(
              &self.document,
              paragraph_ix,
              start..end,
            )));
          }
        },
        block @ (Block::Image(_) | Block::Equation(_) | Block::Table(_)) => {
          if block_ix > start_block && block_ix < end_block {
            collect_block_assets(block, &self.document.assets, &mut assets);
            blocks.push(input_block_from_block(block));
          }
        },
      }
    }
    (!blocks.is_empty()).then_some(RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: Vec::new(),
      blocks,
      assets,
    })
  }

  fn selection_crosses_object_blocks(&self, range: Range<DocumentOffset>) -> bool {
    if !self.document_has_object_blocks() {
      return false;
    }
    let Some(start_block) = self.block_ix_for_paragraph(range.start.paragraph) else {
      return false;
    };
    let Some(end_block) = self.block_ix_for_paragraph(range.end.paragraph) else {
      return false;
    };
    self.document.blocks[start_block.min(end_block)..=start_block.max(end_block)]
      .iter()
      .any(|block| !matches!(block, Block::Paragraph(_)))
  }

  pub(super) fn block_is_inside_text_selection(&self, block_ix: usize) -> bool {
    if self.selected_block.is_some() || self.selection.is_caret() {
      return false;
    }
    if !self.document_has_object_blocks() {
      return false;
    }
    let range = self.selection.normalized();
    let Some(start_block) = self.block_ix_for_paragraph(range.start.paragraph) else {
      return false;
    };
    let Some(end_block) = self.block_ix_for_paragraph(range.end.paragraph) else {
      return false;
    };
    block_ix > start_block.min(end_block) && block_ix < start_block.max(end_block)
  }

  fn object_block_indices_in_text_range(&self, range: Range<DocumentOffset>) -> Vec<usize> {
    if !self.document_has_object_blocks() {
      return Vec::new();
    }
    let Some(start_block) = self.block_ix_for_paragraph(range.start.paragraph) else {
      return Vec::new();
    };
    let Some(end_block) = self.block_ix_for_paragraph(range.end.paragraph) else {
      return Vec::new();
    };
    ((start_block + 1)..end_block)
      .filter(|block_ix| {
        self
          .document
          .blocks
          .get(*block_ix)
          .is_some_and(|block| !matches!(block, Block::Paragraph(_)))
      })
      .collect()
  }

  fn delete_selection_with_document_snapshot(&mut self, cx: &mut Context<Self>) -> bool {
    if self.selection.is_caret() {
      return false;
    }
    let range = self.selection.normalized();
    let object_indices = self.object_block_indices_in_text_range(range.clone());
    if object_indices.is_empty() {
      return false;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    {
      let blocks = Arc::make_mut(&mut self.document.blocks);
      for block_ix in object_indices.iter().copied().rev() {
        if block_ix < blocks.len() {
          blocks.remove(block_ix);
        }
      }
    }
    for block_ix in object_indices.into_iter().rev() {
      remove_block_ids(&mut self.document, block_ix..block_ix + 1);
    }
    self.delete_selection_internal();
    let after_document = self.document.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = vec![SemanticEditCommand::ReplaceDocument];
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceDocument {
        before: Box::new(before_document),
        after: Box::new(after_document),
      }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
    true
  }

  fn delete_selected_block(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(selection) = self.selected_block.take() else {
      return false;
    };
    if matches!(selection, BlockSelection::TableCell { .. }) {
      self.selected_block = Some(selection);
      return false;
    }
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    if block_ix >= self.document.blocks.len() {
      return false;
    }
    let block_id = self.identity_map.block_id(block_ix);
    let blocks = Arc::make_mut(&mut self.document.blocks);
    if matches!(blocks.get(block_ix), Some(Block::Paragraph(_))) {
      return false;
    }
    let block = blocks.remove(block_ix);
    remove_block_ids(&mut self.document, block_ix..block_ix + 1);
    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = block_id.map_or_else(
      || vec![SemanticEditCommand::ReplaceDocument],
      |block| vec![SemanticEditCommand::DeleteBlock { block }],
    );
    self.undo_stack.push(EditRecord {
      before_selection: before_selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::DeleteBlock { block_ix, block }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.clear_layout_work_caches();
    self.item_sizes_cache = None;
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
    true
  }

  pub fn caret_paragraph(&self) -> usize {
    self.selection.head.paragraph
  }

  pub fn caret_paragraph_style(&self) -> ParagraphStyle {
    self
      .document
      .paragraphs
      .get(self.selection.head.paragraph)
      .map(|p| p.style)
      .unwrap_or(ParagraphStyle::Normal)
  }

  pub fn viewport_anchor_paragraph(&self) -> Option<usize> {
    if self.scroll_handle.bounds().size.height <= px(1.0) {
      return None;
    }
    if let Some(paragraph_ix) = self.capture_visible_chunk_scroll_anchor().and_then(|anchor| anchor.paragraph_ix()) {
      return Some(paragraph_ix);
    }
    let cache = self.item_sizes_cache.as_ref()?;
    if self.height_prefix_index.len() != cache.item_count || cache.item_count == 0 {
      return None;
    }
    let content_y = (-self.scroll_handle.offset().y).max(px(0.0));
    let item_ix = self.height_prefix_index.lower_bound(content_y);
    match cache.items.get(item_ix)? {
      VirtualItem::ParagraphChunk { paragraph_ix, .. } | VirtualItem::ParagraphRemainder { paragraph_ix, .. } => Some(*paragraph_ix),
      VirtualItem::HiddenBlock { block_ix } | VirtualItem::StructuralBlock { block_ix } => self.paragraph_ix_for_block(*block_ix),
    }
  }

  pub(super) fn drag_source_selection(&self) -> Option<EditorSelection> {
    self.active_text_drag.as_ref().map(|drag| EditorSelection {
      anchor: drag.source_range.start,
      head: drag.source_range.end,
    })
  }

  pub(super) fn caret_paint_width(&self) -> Pixels {
    if self.active_text_drag.is_some() { px(2.0) } else { px(1.0) }
  }

  pub(super) fn table_cell_caret_for_paint(&self, window: &Window) -> Option<TableCellCaret> {
    if !self.focus_handle.is_focused(window) {
      return None;
    }
    let BlockSelection::TableCell { block_ix, row_ix, cell_ix } = self.selected_block? else {
      return None;
    };
    Some(TableCellCaret {
      block_ix,
      row_ix,
      cell_ix,
      paragraph_block_ix: self.table_cell_block_ix,
      anchor: self.table_cell_anchor,
      byte: self.table_cell_caret,
      caret_visible: self.caret_visible,
    })
  }

}
