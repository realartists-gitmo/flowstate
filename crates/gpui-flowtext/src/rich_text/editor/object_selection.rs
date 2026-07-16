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
          let (row_id, column_id) = table_cell_ids_at(&self.document, block_ix, row_ix, cell_ix);
          let selection = BlockSelection::TableCell {
            block_ix,
            row_ix,
            cell_ix,
            row_id,
            column_id,
          };
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

  /// Track a live column-resize drag. Loro-first (spec §5): the drag never
  /// touches THE projection — it only accumulates the target width, and the
  /// commit at drag end is a batch of typed `SetColumnWidth` intents.
  ///
  /// The accumulated width rides in the slot appended past the per-column
  /// `start_widths` (the drag struct carries no dedicated field for it); a
  /// drag that never moved has no slot and commits nothing.
  fn update_table_column_resize_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.table_column_resize_drag.as_ref() else {
      return false;
    };
    let base_columns = table_column_count(&drag.before).max(1);
    let column_ix = drag.column_ix;
    let start_width = drag.start_widths.get(column_ix).copied();
    let delta: f32 = (position.x - drag.start_position.x).into();
    let Some(start_width) = start_width else {
      self.table_column_resize_drag = None;
      return true;
    };
    let width = (start_width as f32 + delta).clamp(32.0, 1600.0).round() as u32;
    if let Some(drag) = self.table_column_resize_drag.as_mut() {
      drag.start_widths.truncate(base_columns);
      drag.start_widths.push(width);
    }
    cx.notify();
    true
  }

  /// Commit the column resize at drag end through the ONE write path: one
  /// typed `SetColumnWidth` intent per changed column. Every column pins to
  /// its measured fixed width (freezing the table layout, matching the
  /// pre-Loro-first behavior), grouped as a single undo unit. The runtime's
  /// patches advance THE projection — no direct write, no history snapshot.
  fn finish_table_column_resize_drag(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.table_column_resize_drag.take() else {
      return false;
    };
    let base_columns = table_column_count(&drag.before).max(1);
    let Some(final_width) = drag.start_widths.get(base_columns).copied() else {
      // The drag never moved: nothing to commit.
      cx.notify();
      return true;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(drag.block_ix).cloned() else {
      cx.notify();
      return true;
    };
    let Some(table_id) = self.semantic_block_id(drag.block_ix) else {
      tracing::warn!(
        block_ix = drag.block_ix,
        "refusing table column resize: projection block has no durable id, so no intent can address it",
      );
      cx.notify();
      return true;
    };
    let mut targets = drag.start_widths[..base_columns].to_vec();
    if let Some(slot) = targets.get_mut(drag.column_ix) {
      *slot = final_width;
    }
    let mut changed = Vec::new();
    for (column_ix, column) in table.columns.iter().enumerate() {
      let Some(width) = targets.get(column_ix).copied() else {
        break;
      };
      if input_table_column_width_from_table_column_width(&column.width) != InputTableColumnWidth::FixedPx(width) {
        changed.push((column.id, width));
      }
    }
    if changed.is_empty() {
      cx.notify();
      return true;
    }
    let grouped = changed.len() > 1;
    if grouped {
      self.begin_undo_group();
    }
    for (column, width) in changed {
      self.write_intent(
        LocalIntent::Table(crate::local_intents::TableIntent::SetColumnWidth {
          table: table_id,
          column,
          width: InputTableColumnWidth::FixedPx(width),
        }),
        cx,
      );
    }
    if grouped {
      self.end_undo_group();
    }
    cx.notify();
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
    // B-S1: a document-edge selection includes the edge OBJECTS. Text
    // selections live in paragraph space, so a leading image (before the
    // first paragraph) or a trailing one could never satisfy the old
    // strictly-between test — select-all + copy/cut silently dropped them.
    // Blocks outside the first/last paragraph's block are objects by
    // construction (an earlier paragraph would BE the first paragraph).
    let doc_start_selected = range.start.paragraph == 0 && range.start.byte == 0;
    let last_paragraph_ix = self.document.paragraphs.len().saturating_sub(1);
    let doc_end_selected = range.end.paragraph == last_paragraph_ix
      && range.end.byte == paragraph_text_len(&self.document.paragraphs[last_paragraph_ix]);
    let scan_start = if doc_start_selected { 0 } else { start_block };
    let scan_end = if doc_end_selected {
      self.document.blocks.len().saturating_sub(1)
    } else {
      end_block
    };
    let has_object = self
      .document
      .blocks
      .range(scan_start.min(scan_end)..scan_start.max(scan_end) + 1)
      .any(|block| !matches!(block, Block::Paragraph(_)));
    if !has_object {
      return None;
    }
    let mut blocks = Vec::new();
    let mut assets = Vec::new();
    for block_ix in scan_start..=scan_end {
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
          let interior = block_ix > start_block && block_ix < end_block;
          let leading_edge = doc_start_selected && block_ix < start_block;
          let trailing_edge = doc_end_selected && block_ix > end_block;
          if interior || leading_edge || trailing_edge {
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

  /// Delete the selected object block through the ONE write path: a typed
  /// `DeleteBlocks` intent addressed by the block's durable identity. The
  /// runtime retires the block canonically and its returned patches advance
  /// THE projection — no direct block removal, no snapshot history.
  /// B-S11: move the selected object block one position up or down — the
  /// keyboard half of block movement (`MoveBlock` finally has a caller). The
  /// block stays selected so repeated presses walk it through the document.
  pub fn move_selected_block(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
    let block_ix = match self.selected_block {
      Some(BlockSelection::Image(ix) | BlockSelection::Equation(ix) | BlockSelection::Table(ix)) => ix,
      _ => return false,
    };
    let Some(block_id) = self.semantic_block_id(block_ix) else {
      return false;
    };
    if down && block_ix + 1 >= self.document.blocks.len() {
      return false; // already last
    }
    if !down && block_ix == 0 {
      return false; // already first
    }
    // Destination: UP lands before the previous block; DOWN lands before the
    // block after next (`None` = document end).
    let before_ix = if down { block_ix + 2 } else { block_ix - 1 };
    let before = if before_ix < self.document.blocks.len() {
      match self.semantic_block_id(before_ix) {
        Some(id) => Some(id),
        None => return false,
      }
    } else {
      None
    };
    let moved = self
      .write_intent(
        LocalIntent::MoveBlock(crate::local_intents::MoveBlockIntent { block: block_id, before }),
        cx,
      )
      .is_some();
    if moved {
      // Re-select the block at its new position (the projection advanced).
      let new_ix = if down { block_ix + 1 } else { block_ix - 1 };
      let reselect = match self.selected_block {
        Some(BlockSelection::Image(_)) => BlockSelection::Image(new_ix),
        Some(BlockSelection::Equation(_)) => BlockSelection::Equation(new_ix),
        _ => BlockSelection::Table(new_ix),
      };
      self.select_block(reselect, cx);
    }
    moved
  }

  fn delete_selected_block(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(selection) = self.selected_block else {
      return false;
    };
    if matches!(selection, BlockSelection::TableCell { .. }) {
      return false;
    }
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    if !matches!(
      self.document.blocks.get(block_ix),
      Some(Block::Image(_) | Block::Equation(_) | Block::Table(_))
    ) {
      return false;
    }
    let Some(block_id) = self.identity_map.block_id(block_ix) else {
      tracing::warn!(block_ix, "refusing selected-block deletion: projection block has no durable id, so no intent can address it");
      return false;
    };
    let committed = self
      .write_intent(
        LocalIntent::DeleteBlocks(crate::local_intents::DeleteBlocksIntent { blocks: vec![block_id] }),
        cx,
      )
      .is_some();
    if committed {
      self.clear_block_selection();
      cx.notify();
    }
    committed
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

  /// O-S2 hybrid tracking: the (top, bottom) paragraph indexes currently in
  /// the viewport, from the virtual-list height index. `None` before layout.
  pub fn viewport_paragraph_range(&self) -> Option<(usize, usize)> {
    let bounds_height = self.scroll_handle.bounds().size.height;
    if bounds_height <= px(1.0) {
      return None;
    }
    let cache = self.item_sizes_cache.as_ref()?;
    if self.height_prefix_index.len() != cache.item_count || cache.item_count == 0 {
      return None;
    }
    let top_y = (-self.scroll_handle.offset().y).max(px(0.0));
    let bottom_y = top_y + bounds_height;
    let paragraph_at = |y: Pixels| -> Option<usize> {
      let item_ix = self.height_prefix_index.lower_bound(y).min(cache.item_count - 1);
      match cache.items.get(item_ix)? {
        VirtualItem::ParagraphChunk { paragraph_ix, .. } | VirtualItem::ParagraphRemainder { paragraph_ix, .. } => Some(*paragraph_ix),
        VirtualItem::HiddenBlock { block_ix } | VirtualItem::StructuralBlock { block_ix } => self.paragraph_ix_for_block(*block_ix),
      }
    };
    Some((paragraph_at(top_y)?, paragraph_at(bottom_y)?))
  }

  pub(super) fn drag_source_selection(&self) -> Option<EditorSelection> {
    self
      .active_text_drag
      .as_ref()
      .map(|drag| EditorSelection::range(drag.source_range.start, drag.source_range.end))
  }

  pub(super) fn caret_paint_width(&self) -> Pixels {
    if self.active_text_drag.is_some() { px(2.0) } else { px(1.0) }
  }

  pub(super) fn table_cell_caret_for_paint(&self, window: &Window) -> Option<TableCellCaret> {
    if !self.focus_handle.is_focused(window) {
      return None;
    }
    let BlockSelection::TableCell {
      block_ix,
      row_ix,
      cell_ix,
      row_id,
      column_id,
    } = self.selected_block?
    else {
      return None;
    };
    Some(TableCellCaret {
      block_ix,
      row_ix,
      cell_ix,
      row_id,
      column_id,
      paragraph_block_ix: self.table_cell_block_ix,
      anchor: self.table_cell_anchor,
      byte: self.table_cell_caret,
      caret_visible: self.caret_visible,
    })
  }

}
