#[hotpath::measure_all]
impl RichTextEditor {
  // Asset bytes are transferred out-of-band; remote document content is applied
  // by replacing the Loro projection snapshot, not by feeding document patches.
  pub fn apply_collab_asset_records(&mut self, asset_records: &[(AssetId, AssetRecord)], cx: &mut Context<Self>) {
    if asset_records.is_empty() {
      return;
    }
    self.suppress_collab_capture = self.suppress_collab_capture.saturating_add(1);
    for (id, record) in asset_records {
      self.document.assets.assets.insert(*id, record.clone());
    }
    self.suppress_collab_capture = self.suppress_collab_capture.saturating_sub(1);
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed_with_ops(generation, false, None, cx);
    self.after_formatting_mutation(cx);
  }

  // Retained for local derived UI-diff helpers; network collaboration applies
  // Loro projection snapshots instead.
  pub fn apply_collab_patches(&mut self, patches: &[CollabPatch], cx: &mut Context<Self>) {
    self.apply_collab_patches_at_frontier(patches, self.document.frontier.clone(), cx);
  }

  pub fn apply_collab_patches_at_frontier(&mut self, patches: &[CollabPatch], frontier: Vec<u8>, cx: &mut Context<Self>) {
    if patches.is_empty() {
      self.document.frontier = frontier;
      return;
    }
    self.suppress_collab_capture = self.suppress_collab_capture.saturating_add(1);
    let mut invalidation: Option<Range<usize>> = None;
    for patch in patches {
      self.apply_one_collab_patch(patch, &mut invalidation);
    }
    self.document.frontier = frontier;
    self.suppress_collab_capture = self.suppress_collab_capture.saturating_sub(1);
    self.identity_map.reconcile(&self.document);
    self.layout_invalidation_hint = invalidation;
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed_with_ops(generation, false, None, cx);
    // Remote collaboration patches should update this editor in place, but
    // should not scroll the viewport as if the local user typed the change.
    self.after_formatting_mutation(cx);
    self.layout_invalidation_hint = None;
  }

  pub fn collab_apply_deferred(&self) -> bool {
    self.selecting
      || self.active_text_drag.is_some()
      || self.image_resize_drag.is_some()
      || self.table_column_resize_drag.is_some()
      || self.ime_composition_active()
  }

  fn apply_one_collab_patch(&mut self, patch: &CollabPatch, invalidation: &mut Option<Range<usize>>) {
    match patch {
      CollabPatch::ParagraphText { row, new, delta_utf8 } => {
        self.remap_object_text_selection_for_delta(*row, delta_utf8);
        if let Some(paragraph_ix) = self.paragraph_ix_for_block(*row) {
          remap_selection_for_text_delta(&mut self.selection, paragraph_ix, delta_utf8);
          replace_paragraph_content(&mut self.document, paragraph_ix, new);
          extend_invalidation(invalidation, paragraph_ix..paragraph_ix + 1);
        }
      },
      CollabPatch::ParagraphStyle { row, style } => {
        if let Some(paragraph_ix) = self.paragraph_ix_for_block(*row)
          && let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix)
        {
          paragraph.style = *style;
          bump_paragraph_version(paragraph);
          update_paragraph_block(&mut self.document, paragraph_ix);
          rebuild_document_sections(&mut self.document);
          extend_invalidation(invalidation, paragraph_ix..paragraph_ix + 1);
        }
      },
      CollabPatch::ParagraphRuns { row, runs } => {
        if let Some(paragraph_ix) = self.paragraph_ix_for_block(*row)
          && let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix)
        {
          paragraph.runs.clone_from(runs);
          bump_paragraph_version(paragraph);
          update_paragraph_block(&mut self.document, paragraph_ix);
          rebuild_document_sections(&mut self.document);
          extend_invalidation(invalidation, paragraph_ix..paragraph_ix + 1);
        }
      },
      CollabPatch::ReplaceObjectBlock { row, block } => {
        let mut blocks = collab_structural_blocks_from_document(&self.document);
        if *row < blocks.len() {
          if selected_block_in_range(self.selected_block, *row..row.saturating_add(1)) {
            self.clear_remote_object_editing_state();
          }
          blocks[*row] = block.clone();
          rebuild_document_from_collab_structural_blocks(&mut self.document, blocks);
          clamp_selection_to_document(&self.document, &mut self.selection);
          extend_invalidation(invalidation, 0..self.document.paragraphs.len());
        }
      },
      CollabPatch::InsertBlocks { row, blocks: inserted } => {
        let mut blocks = collab_structural_blocks_from_document(&self.document);
        let row = (*row).min(blocks.len());
        if selected_block_ix(self.selected_block).is_some_and(|block_ix| block_ix >= row) {
          self.clear_remote_object_editing_state();
        }
        blocks.splice(row..row, inserted.iter().cloned());
        rebuild_document_from_collab_structural_blocks(&mut self.document, blocks);
        clamp_selection_to_document(&self.document, &mut self.selection);
        extend_invalidation(invalidation, 0..self.document.paragraphs.len());
      },
      CollabPatch::DeleteBlocks { row, count } => {
        let mut blocks = collab_structural_blocks_from_document(&self.document);
        let start = (*row).min(blocks.len());
        let end = start.saturating_add(*count).min(blocks.len());
        if selected_block_ix(self.selected_block).is_some_and(|block_ix| block_ix >= start) {
          self.clear_remote_object_editing_state();
        }
        blocks.drain(start..end);
        rebuild_document_from_collab_structural_blocks(&mut self.document, blocks);
        clamp_selection_to_document(&self.document, &mut self.selection);
        extend_invalidation(invalidation, 0..self.document.paragraphs.len());
      },
      CollabPatch::MoveBlock { from, to } => {
        let mut blocks = collab_structural_blocks_from_document(&self.document);
        if *from < blocks.len() {
          let first = (*from).min(*to);
          let last = (*from).max(*to);
          if selected_block_ix(self.selected_block).is_some_and(|block_ix| (first..=last).contains(&block_ix)) {
            self.clear_remote_object_editing_state();
          }
          let block = blocks.remove(*from);
          blocks.insert((*to).min(blocks.len()), block);
          rebuild_document_from_collab_structural_blocks(&mut self.document, blocks);
          clamp_selection_to_document(&self.document, &mut self.selection);
          extend_invalidation(invalidation, 0..self.document.paragraphs.len());
        }
      },
      CollabPatch::AssetArrived { id, record } => {
        self.document.assets.assets.insert(*id, record.clone());
      },
    }
  }

  fn clear_remote_object_editing_state(&mut self) {
    self.selected_block = None;
    self.image_resize_drag = None;
    self.table_column_resize_drag = None;
    self.table_cell_block_ix = 0;
    self.table_cell_anchor = 0;
    self.table_cell_caret = 0;
    self.equation_source_anchor = 0;
    self.equation_source_caret = 0;
  }

  fn remap_object_text_selection_for_delta(&mut self, row: usize, delta: &[CollabTextDelta]) {
    match self.selected_block {
      Some(BlockSelection::TableCell { block_ix, .. }) if block_ix == row => {
        self.table_cell_anchor = remap_byte(self.table_cell_anchor, delta);
        self.table_cell_caret = remap_byte(self.table_cell_caret, delta);
      },
      Some(BlockSelection::Equation(block_ix)) if block_ix == row => {
        self.equation_source_anchor = remap_byte(self.equation_source_anchor, delta);
        self.equation_source_caret = remap_byte(self.equation_source_caret, delta);
      },
      Some(BlockSelection::Image(_) | BlockSelection::Equation(_) | BlockSelection::Table(_) | BlockSelection::TableCell { .. }) | None => {},
    }
  }
}

pub fn apply_projection_patches(document: &mut DocumentProjection, patches: &[CollabPatch]) {
  for patch in patches {
    match patch {
      CollabPatch::ParagraphText { row, new, .. } => {
        if let Some(paragraph_ix) = paragraph_ix_for_block_row(document, *row) {
          replace_paragraph_content(document, paragraph_ix, new);
        }
      },
      CollabPatch::ParagraphStyle { row, style } => {
        if let Some(paragraph_ix) = paragraph_ix_for_block_row(document, *row)
          && let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix)
        {
          paragraph.style = *style;
          bump_paragraph_version(paragraph);
          update_paragraph_block(document, paragraph_ix);
          rebuild_document_sections(document);
        }
      },
      CollabPatch::ParagraphRuns { row, runs } => {
        if let Some(paragraph_ix) = paragraph_ix_for_block_row(document, *row)
          && let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix)
        {
          paragraph.runs.clone_from(runs);
          bump_paragraph_version(paragraph);
          update_paragraph_block(document, paragraph_ix);
          rebuild_document_sections(document);
        }
      },
      CollabPatch::ReplaceObjectBlock { row, block } => {
        let mut blocks = collab_structural_blocks_from_document(document);
        if *row < blocks.len() {
          blocks[*row] = block.clone();
          rebuild_document_from_collab_structural_blocks(document, blocks);
        }
      },
      CollabPatch::InsertBlocks { row, blocks: inserted } => {
        let mut blocks = collab_structural_blocks_from_document(document);
        let row = (*row).min(blocks.len());
        blocks.splice(row..row, inserted.iter().cloned());
        rebuild_document_from_collab_structural_blocks(document, blocks);
      },
      CollabPatch::DeleteBlocks { row, count } => {
        let mut blocks = collab_structural_blocks_from_document(document);
        let start = (*row).min(blocks.len());
        let end = start.saturating_add(*count).min(blocks.len());
        blocks.drain(start..end);
        rebuild_document_from_collab_structural_blocks(document, blocks);
      },
      CollabPatch::MoveBlock { from, to } => {
        let mut blocks = collab_structural_blocks_from_document(document);
        if *from < blocks.len() {
          let block = blocks.remove(*from);
          blocks.insert((*to).min(blocks.len()), block);
          rebuild_document_from_collab_structural_blocks(document, blocks);
        }
      },
      CollabPatch::AssetArrived { id, record } => {
        document.assets.assets.insert(*id, record.clone());
      },
    }
  }
}

fn paragraph_ix_for_block_row(document: &DocumentProjection, row: usize) -> Option<usize> {
  matches!(document.blocks.get(row), Some(Block::Paragraph(_))).then(|| {
    document
      .blocks
      .iter()
      .take(row)
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count()
  })
}

#[hotpath::measure]
fn selected_block_ix(selection: Option<BlockSelection>) -> Option<usize> {
  match selection {
    Some(BlockSelection::Image(block_ix)
    | BlockSelection::Equation(block_ix)
    | BlockSelection::Table(block_ix)
    | BlockSelection::TableCell { block_ix, .. }) => Some(block_ix),
    None => None,
  }
}

#[hotpath::measure]
fn selected_block_in_range(selection: Option<BlockSelection>, range: Range<usize>) -> bool {
  selected_block_ix(selection).is_some_and(|block_ix| range.contains(&block_ix))
}

#[hotpath::measure]
fn replace_paragraph_content(document: &mut DocumentProjection, paragraph_ix: usize, paragraph: &InputParagraph) {
  if paragraph_ix >= document.paragraphs.len() {
    return;
  }
  let text = input_paragraph_text(paragraph);
  let byte_range = paragraph_byte_range(document, paragraph_ix);
  document.text.delete(byte_range.clone());
  document.text.insert(byte_range.start, &text);
  let old_style = document.paragraphs[paragraph_ix].style;
  let mut replacement = paragraph_from_input_paragraph(paragraph);
  replacement.version = document.paragraphs[paragraph_ix].version.wrapping_add(1);
  replacement.byte_range = byte_range.clone();
  paragraphs_mut(document)[paragraph_ix] = replacement;
  // Single in-place paragraph update (count unchanged): shift the offset index
  // and the block in place. The section outline can only change if this
  // paragraph's style changed.
  update_paragraph_offsets_after_len_change(document, paragraph_ix);
  if old_style != paragraph.style {
    rebuild_document_sections(document);
  }
}

#[hotpath::measure]
fn collab_structural_blocks_from_document(document: &DocumentProjection) -> Vec<CollabStructuralBlock> {
  let mut paragraph_ix = 0;
  document
    .blocks
    .iter()
    .enumerate()
    .map(|(block_ix, block)| match block {
      Block::Paragraph(paragraph) => {
        let input = input_paragraph_from_document_paragraph(document, paragraph_ix, paragraph);
        let structural = CollabStructuralBlock {
          block_id: document.ids.block_ids.get(block_ix).copied().unwrap_or_else(new_block_id),
          paragraph_id: Some(
            document
              .ids
              .paragraph_ids
              .get(paragraph_ix)
              .copied()
              .unwrap_or_else(new_paragraph_id),
          ),
          block: InputBlock::Paragraph(input),
        };
        paragraph_ix += 1;
        structural
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => CollabStructuralBlock {
        block_id: document.ids.block_ids.get(block_ix).copied().unwrap_or_else(new_block_id),
        paragraph_id: None,
        block: input_block_from_block(block),
      },
    })
    .collect()
}

#[hotpath::measure]
fn input_paragraph_from_document_paragraph(document: &DocumentProjection, paragraph_ix: usize, paragraph: &Paragraph) -> InputParagraph {
  let text = paragraph_text(document, paragraph_ix);
  let mut byte = 0;
  InputParagraph {
    style: paragraph.style,
    runs: paragraph
      .runs
      .iter()
      .map(|run| {
        let start = byte;
        let end = (start + run.len).min(text.len());
        byte = end;
        InputRun {
          text: text.get(start..end).unwrap_or("").to_string(),
          styles: run.styles,
        }
      })
      .collect(),
  }
}

#[hotpath::measure]
fn rebuild_document_from_collab_structural_blocks(document: &mut DocumentProjection, blocks: Vec<CollabStructuralBlock>) {
  let assets = document.assets.clone();
  let theme = document.theme.clone();
  let document_id = document.ids.document_id;
  let input_blocks = blocks
    .iter()
    .map(|block| block.block.clone())
    .collect::<Vec<_>>();
  let block_ids = blocks.iter().map(|block| block.block_id).collect::<Vec<_>>();
  let paragraph_ids = blocks
    .iter()
    .filter_map(|block| match &block.block {
      InputBlock::Paragraph(_) => Some(block.paragraph_id.unwrap_or_else(new_paragraph_id)),
      InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
    })
    .collect::<Vec<_>>();
  let mut rebuilt = document_from_input_blocks(theme, input_blocks);
  rebuilt.assets = assets;
  rebuilt.ids.document_id = document_id;
  rebuilt.ids.block_ids = block_ids;
  rebuilt.ids.paragraph_ids = paragraph_ids;
  debug_assert_eq!(rebuilt.ids.block_ids.len(), rebuilt.blocks.len());
  debug_assert_eq!(rebuilt.ids.paragraph_ids.len(), rebuilt.paragraphs.len());
  rebuild_document_sections(&mut rebuilt);
  *document = rebuilt;
}

#[hotpath::measure]
fn remap_selection_for_text_delta(selection: &mut EditorSelection, paragraph_ix: usize, delta: &[CollabTextDelta]) {
  if selection.anchor.paragraph == paragraph_ix {
    selection.anchor.byte = remap_byte(selection.anchor.byte, delta);
  }
  if selection.head.paragraph == paragraph_ix {
    selection.head.byte = remap_byte(selection.head.byte, delta);
  }
}

#[hotpath::measure]
fn remap_byte(byte: usize, delta: &[CollabTextDelta]) -> usize {
  let mut old = 0usize;
  let mut new = 0usize;
  for item in delta {
    match *item {
      CollabTextDelta::Retain(len) => {
        if byte <= old + len {
          return new + (byte - old);
        }
        old += len;
        new += len;
      },
      CollabTextDelta::Insert(len) => {
        new += len;
      },
      CollabTextDelta::Delete(len) => {
        if byte <= old + len {
          return new;
        }
        old += len;
      },
    }
  }
  new + byte.saturating_sub(old)
}

#[hotpath::measure]
fn clamp_selection_to_document(document: &DocumentProjection, selection: &mut EditorSelection) {
  selection.anchor = clamp_offset_to_document(document, selection.anchor);
  selection.head = clamp_offset_to_document(document, selection.head);
}

#[hotpath::measure]
fn clamp_offset_to_document(document: &DocumentProjection, offset: DocumentOffset) -> DocumentOffset {
  let paragraph = offset.paragraph.min(document.paragraphs.len().saturating_sub(1));
  let byte = document.paragraphs.get(paragraph).map_or(0, paragraph_text_len).min(offset.byte);
  DocumentOffset { paragraph, byte }
}

#[hotpath::measure]
fn extend_invalidation(target: &mut Option<Range<usize>>, range: Range<usize>) {
  if range.is_empty() {
    return;
  }
  match target {
    Some(existing) => {
      existing.start = existing.start.min(range.start);
      existing.end = existing.end.max(range.end);
    },
    None => *target = Some(range),
  }
}
