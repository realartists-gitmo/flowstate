#[hotpath::measure_all]
impl RichTextEditor {
  // Asset bytes are transferred out-of-band; remote document content is applied
  // by replacing the Loro projection snapshot, not by feeding document patches.
  pub fn apply_synced_asset_records(&mut self, asset_records: &[(AssetId, AssetRecord)], cx: &mut Context<Self>) {
    if asset_records.is_empty() {
      return;
    }
    self.suppress_command_capture = self.suppress_command_capture.saturating_add(1);
    for (id, record) in asset_records {
      self.document.assets.assets.insert(*id, record.clone());
      self.committed_document.assets.assets.insert(*id, record.clone());
    }
    self.suppress_command_capture = self.suppress_command_capture.saturating_sub(1);
    // Asset availability is out-of-band cache state: repaint, but never dirty
    // the document or advance the edit generation.
    self.after_formatting_mutation(cx);
  }

  pub fn apply_projection_patch_batch(&mut self, batch: &ProjectionPatchBatch, cx: &mut Context<Self>) -> Result<(), ProjectionApplyError> {
    if self.committed_document.frontier != batch.base_frontier {
      flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Frontier, "apply-stale-frontier", || {
        format!(
          "txn={} expected_len={} actual_len={}",
          batch.transaction_id,
          batch.base_frontier.len(),
          self.committed_document.frontier.len(),
        )
      });
      return Err(ProjectionApplyError::StaleFrontier {
        expected: batch.base_frontier.clone(),
        actual: self.committed_document.frontier.clone(),
      });
    }
    flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Frontier, "apply-frontier-ok", || {
      format!("txn={} frontier_len={} patches={}", batch.transaction_id, batch.base_frontier.len(), batch.patches.len())
    });
    // Fidelity: snapshot the pre-apply caret and document size so the whole
    // apply (rebuild + stable-selection re-resolve) can be checked for a
    // backward caret jump.
    let fid_sel_before = self.fidelity_caret_before();
    let fid_size_before = flowstate_fidelity::enabled().then(|| self.fidelity_document_size());
    let stable_selection = StableEditorSelection::capture(&self.document, &self.selection);
    let mut document = self.committed_document.clone();
    let mut invalidation: Option<Range<usize>> = None;
    if !batch.is_empty() {
      apply_projection_patches_to_document(&mut document, &batch.patches, Some(&mut invalidation))?;
    }
    document.frontier.clone_from(&batch.new_frontier);
    self.suppress_command_capture = self.suppress_command_capture.saturating_add(1);
    self.committed_document = document;
    self.rebuild_visible_from_committed(Vec::new(), None, cx);
    if self.runtime_edit_selection_epoch.is_none()
      && self.pending_semantic_edits.is_empty()
      && let Some(selection) = stable_selection
    {
      let fid_before = self.fidelity_caret_before();
      self.selection = selection.resolve(&self.document);
      self.fidelity_caret_set("apply_projection_patch_batch/stable-resolve", &fid_before);
      self.emit_selection_changed(cx);
    }
    self.suppress_command_capture = self.suppress_command_capture.saturating_sub(1);
    self.identity_map.reconcile(&self.document);
    self.layout_invalidation_hint = invalidation;
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed_with_ops(generation, false, None, cx);
    // Remote collaboration patches should update this editor in place, but
    // should not scroll the viewport as if the local user typed the change.
    self.after_formatting_mutation(cx);
    self.pending_scroll_head_after_layout = false;
    self.layout_invalidation_hint = None;
    if let Some((pre_paras, pre_len)) = fid_size_before {
      let (post_paras, post_len) = self.fidelity_document_size();
      let shrank = post_paras < pre_paras || post_len < pre_len;
      self.fidelity_check_caret_not_regressed("apply_projection_patch_batch", &fid_sel_before, shrank);
    }
    Ok(())
  }

  pub fn projection_apply_deferred(&self) -> bool {
    self.selecting
      || self.active_text_drag.is_some()
      || self.image_resize_drag.is_some()
      || self.table_column_resize_drag.is_some()
      || self.ime_composition_active()
  }

}

pub fn apply_projection_patch_batch(document: &mut DocumentProjection, batch: &ProjectionPatchBatch) -> Result<(), ProjectionApplyError> {
  if document.frontier != batch.base_frontier {
    return Err(ProjectionApplyError::StaleFrontier {
      expected: batch.base_frontier.clone(),
      actual: document.frontier.clone(),
    });
  }
  let mut candidate = document.clone();
  apply_projection_patches_to_document(&mut candidate, &batch.patches, None)?;
  candidate.frontier.clone_from(&batch.new_frontier);
  *document = candidate;
  Ok(())
}

fn apply_projection_patches_to_document(
  document: &mut DocumentProjection,
  patches: &[ProjectionPatch],
  invalidation: Option<&mut Option<Range<usize>>>,
) -> Result<(), ProjectionApplyError> {
  let original_outline = document.outline.clone();
  let mut structural_blocks: Option<Vec<ProjectionStructuralBlock>> = None;
  let mut structural_changed = false;
  let mut outline_dirty = false;
  let mut paragraph_invalidation: Option<Range<usize>> = None;

  for patch in patches {
    match patch {
      ProjectionPatch::ParagraphText {
        block_id,
        paragraph_id,
        row_hint,
        new,
        ..
      } => {
        let paragraph_ix = if let Some(blocks) = structural_blocks.as_mut() {
          let row = structural_block_ix_for_patch(blocks, *block_id, *row_hint)?;
          let paragraph_ix = paragraph_ix_for_structural_row(blocks, row);
          let target = blocks
            .get_mut(row)
            .ok_or(ProjectionApplyError::MissingBlock {
              block_id: *block_id,
              row_hint: *row_hint,
            })?;
          if target.paragraph_id != Some(*paragraph_id) {
            return Err(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            });
          }
          let InputBlock::Paragraph(old) = &target.block else {
            return Err(ProjectionApplyError::WrongBlockKind {
              block_id: *block_id,
              expected: "a paragraph",
            });
          };
          outline_dirty |= old.style != new.style;
          target.block = InputBlock::Paragraph(new.clone());
          paragraph_ix
        } else {
          let paragraph_ix = paragraph_ix_for_patch(document, *block_id, *paragraph_id, *row_hint)?;
          outline_dirty |= document.paragraphs[paragraph_ix].style != new.style;
          replace_paragraph_content(document, paragraph_ix, new);
          paragraph_ix
        };
        extend_invalidation(&mut paragraph_invalidation, paragraph_ix..paragraph_ix + 1);
      },
      ProjectionPatch::ParagraphStyle {
        block_id,
        paragraph_id,
        row_hint,
        style,
      } => {
        let paragraph_ix = if let Some(blocks) = structural_blocks.as_mut() {
          let row = structural_block_ix_for_patch(blocks, *block_id, *row_hint)?;
          let paragraph_ix = paragraph_ix_for_structural_row(blocks, row);
          let target = blocks
            .get_mut(row)
            .ok_or(ProjectionApplyError::MissingBlock {
              block_id: *block_id,
              row_hint: *row_hint,
            })?;
          if target.paragraph_id != Some(*paragraph_id) {
            return Err(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            });
          }
          let InputBlock::Paragraph(paragraph) = &mut target.block else {
            return Err(ProjectionApplyError::WrongBlockKind {
              block_id: *block_id,
              expected: "a paragraph",
            });
          };
          outline_dirty |= paragraph.style != *style;
          paragraph.style = *style;
          paragraph_ix
        } else {
          let paragraph_ix = paragraph_ix_for_patch(document, *block_id, *paragraph_id, *row_hint)?;
          let paragraph = paragraphs_mut(document)
            .get_mut(paragraph_ix)
            .ok_or(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            })?;
          outline_dirty |= paragraph.style != *style;
          paragraph.style = *style;
          bump_paragraph_version(paragraph);
          update_paragraph_block(document, paragraph_ix);
          paragraph_ix
        };
        extend_invalidation(&mut paragraph_invalidation, paragraph_ix..paragraph_ix + 1);
      },
      ProjectionPatch::ParagraphRuns {
        block_id,
        paragraph_id,
        row_hint,
        runs,
      } => {
        let paragraph_ix = if let Some(blocks) = structural_blocks.as_mut() {
          let row = structural_block_ix_for_patch(blocks, *block_id, *row_hint)?;
          let paragraph_ix = paragraph_ix_for_structural_row(blocks, row);
          let target = blocks
            .get_mut(row)
            .ok_or(ProjectionApplyError::MissingBlock {
              block_id: *block_id,
              row_hint: *row_hint,
            })?;
          if target.paragraph_id != Some(*paragraph_id) {
            return Err(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            });
          }
          let InputBlock::Paragraph(paragraph) = &mut target.block else {
            return Err(ProjectionApplyError::WrongBlockKind {
              block_id: *block_id,
              expected: "a paragraph",
            });
          };
          let text = input_paragraph_text(paragraph);
          paragraph.runs = input_runs_from_text_runs(&text, runs)?;
          paragraph_ix
        } else {
          let paragraph_ix = paragraph_ix_for_patch(document, *block_id, *paragraph_id, *row_hint)?;
          let text = paragraph_text(document, paragraph_ix);
          validate_text_runs(&text, runs)?;
          let paragraph = paragraphs_mut(document)
            .get_mut(paragraph_ix)
            .ok_or(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            })?;
          paragraph.runs.clone_from(runs);
          bump_paragraph_version(paragraph);
          update_paragraph_block(document, paragraph_ix);
          paragraph_ix
        };
        extend_invalidation(&mut paragraph_invalidation, paragraph_ix..paragraph_ix + 1);
      },
      ProjectionPatch::ReplaceObjectBlock {
        block_id,
        row_hint,
        block,
      } => {
        let blocks = structural_blocks
          .get_or_insert_with(|| projection_structural_blocks_from_document(document));
        let row = structural_block_ix_for_patch(blocks, *block_id, *row_hint)?;
        if blocks[row].paragraph_id.is_some() || block.paragraph_id.is_some() {
          return Err(ProjectionApplyError::WrongBlockKind {
            block_id: *block_id,
            expected: "an object block",
          });
        }
        if block.block_id != *block_id {
          return Err(ProjectionApplyError::InvalidStructuralPatch(
            "object replacement must preserve the target block id",
          ));
        }
        blocks[row] = block.clone();
        structural_changed = true;
      },
      ProjectionPatch::InsertBlocks {
        before,
        row_hint,
        blocks: inserted,
      } => {
        let blocks = structural_blocks
          .get_or_insert_with(|| projection_structural_blocks_from_document(document));
        let row = anchor_ix_for_insert_blocks(blocks, *before, *row_hint)?;
        blocks.splice(row..row, inserted.iter().cloned());
        structural_changed = true;
        outline_dirty = true;
      },
      ProjectionPatch::DeleteBlocks { block_ids, row_hint } => {
        let blocks = structural_blocks
          .get_or_insert_with(|| projection_structural_blocks_from_document(document));
        let mut rows = block_ids
          .iter()
          .map(|block_id| structural_block_ix_for_patch(blocks, *block_id, *row_hint))
          .collect::<Result<Vec<_>, _>>()?;
        rows.sort_unstable();
        for window in rows.windows(2) {
          if window[0] == window[1] {
            return Err(ProjectionApplyError::DuplicateBlockId(blocks[window[0]].block_id));
          }
        }
        for row in rows.into_iter().rev() {
          blocks.remove(row);
        }
        structural_changed = true;
        outline_dirty = true;
      },
      ProjectionPatch::MoveBlock {
        block_id,
        before,
        from_hint,
        to_hint,
      } => {
        let blocks = structural_blocks
          .get_or_insert_with(|| projection_structural_blocks_from_document(document));
        let from = structural_block_ix_for_patch(blocks, *block_id, *from_hint)?;
        let block = blocks.remove(from);
        let to = anchor_ix_for_insert_blocks(blocks, *before, *to_hint)?;
        blocks.insert(to, block);
        structural_changed = true;
        outline_dirty = true;
      },
      ProjectionPatch::AssetArrived { id, record } => {
        document.assets.assets.insert(*id, record.clone());
      },
    }
  }

  if let Some(blocks) = structural_blocks {
    validate_structural_blocks(&blocks)?;
    rebuild_document_from_projection_structural_blocks(document, blocks);
    if !outline_dirty {
      document.outline = original_outline;
    }
  } else if outline_dirty {
    rebuild_document_sections(document);
  }

  if let Some(invalidation) = invalidation {
    if structural_changed {
      extend_invalidation(invalidation, 0..document.paragraphs.len());
    } else if let Some(range) = paragraph_invalidation {
      extend_invalidation(invalidation, range);
    }
  }
  Ok(())
}

fn structural_block_ix_for_patch(
  blocks: &[ProjectionStructuralBlock],
  block_id: BlockId,
  row_hint: usize,
) -> Result<usize, ProjectionApplyError> {
  if blocks.get(row_hint).is_some_and(|block| block.block_id == block_id) {
    return Ok(row_hint);
  }
  blocks
    .iter()
    .position(|block| block.block_id == block_id)
    .ok_or(ProjectionApplyError::MissingBlock { block_id, row_hint })
}

fn paragraph_ix_for_structural_row(blocks: &[ProjectionStructuralBlock], row: usize) -> usize {
  blocks.iter().take(row).filter(|block| block.paragraph_id.is_some()).count()
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

fn paragraph_ix_for_patch(
  document: &DocumentProjection,
  block_id: BlockId,
  paragraph_id: ParagraphId,
  row_hint: usize,
) -> Result<usize, ProjectionApplyError> {
  let row = block_ix_for_patch(document, block_id, row_hint)?;
  let paragraph_ix = paragraph_ix_for_block_row(document, row).ok_or(ProjectionApplyError::WrongBlockKind {
    block_id,
    expected: "a paragraph",
  })?;
  if document.ids.paragraph_ids.get(paragraph_ix).copied() != Some(paragraph_id) {
    return Err(ProjectionApplyError::MissingParagraph { paragraph_id, block_id });
  }
  Ok(paragraph_ix)
}

fn block_ix_for_patch(document: &DocumentProjection, block_id: BlockId, row_hint: usize) -> Result<usize, ProjectionApplyError> {
  if document.ids.block_ids.get(row_hint).copied() == Some(block_id) {
    return Ok(row_hint);
  }
  document
    .ids
    .block_ids
    .iter()
    .position(|id| *id == block_id)
    .ok_or(ProjectionApplyError::MissingBlock { block_id, row_hint })
}

fn anchor_ix_for_insert_blocks(
  blocks: &[ProjectionStructuralBlock],
  before: Option<BlockId>,
  row_hint: usize,
) -> Result<usize, ProjectionApplyError> {
  let Some(before) = before else {
    return Ok(blocks.len());
  };
  if blocks.get(row_hint).is_some_and(|block| block.block_id == before) {
    return Ok(row_hint);
  }
  blocks
    .iter()
    .position(|block| block.block_id == before)
    .ok_or(ProjectionApplyError::InvalidAnchor(before))
}

fn validate_text_runs(text: &str, runs: &[TextRun]) -> Result<(), ProjectionApplyError> {
  let mut byte = 0usize;
  for run in runs {
    let end = byte
      .checked_add(run.len)
      .ok_or(ProjectionApplyError::InvalidStructuralPatch("paragraph run lengths overflow"))?;
    if end > text.len() || !text.is_char_boundary(byte) || !text.is_char_boundary(end) {
      return Err(ProjectionApplyError::InvalidStructuralPatch(
        "paragraph run lengths do not align to UTF-8 boundaries",
      ));
    }
    byte = end;
  }
  if byte != text.len() {
    return Err(ProjectionApplyError::InvalidStructuralPatch(
      "paragraph runs do not cover the complete paragraph text",
    ));
  }
  Ok(())
}

fn input_runs_from_text_runs(text: &str, runs: &[TextRun]) -> Result<Vec<InputRun>, ProjectionApplyError> {
  validate_text_runs(text, runs)?;
  let mut byte = 0usize;
  Ok(
    runs
      .iter()
      .map(|run| {
        let start = byte;
        byte += run.len;
        InputRun {
          text: text[start..byte].to_string(),
          styles: run.styles,
        }
      })
      .collect(),
  )
}

fn validate_structural_blocks(blocks: &[ProjectionStructuralBlock]) -> Result<(), ProjectionApplyError> {
  let mut block_ids = rustc_hash::FxHashSet::default();
  let mut paragraph_ids = rustc_hash::FxHashSet::default();
  for block in blocks {
    if !block_ids.insert(block.block_id) {
      return Err(ProjectionApplyError::DuplicateBlockId(block.block_id));
    }
    if let Some(paragraph_id) = block.paragraph_id
      && !paragraph_ids.insert(paragraph_id)
    {
      return Err(ProjectionApplyError::DuplicateParagraphId(paragraph_id));
    }
  }
  Ok(())
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
  let mut replacement = paragraph_from_input_paragraph(paragraph);
  replacement.version = document.paragraphs[paragraph_ix].version.wrapping_add(1);
  replacement.byte_range = byte_range.clone();
  paragraphs_mut(document)[paragraph_ix] = replacement;
  update_paragraph_offsets_after_len_change(document, paragraph_ix);
}

#[hotpath::measure]
fn projection_structural_blocks_from_document(document: &DocumentProjection) -> Vec<ProjectionStructuralBlock> {
  let mut paragraph_ix = 0;
  document
    .blocks
    .iter()
    .enumerate()
    .map(|(block_ix, block)| match block {
      Block::Paragraph(paragraph) => {
        let input = input_paragraph_from_document_paragraph(document, paragraph_ix, paragraph);
        let structural = ProjectionStructuralBlock {
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
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => ProjectionStructuralBlock {
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
fn rebuild_document_from_projection_structural_blocks(document: &mut DocumentProjection, blocks: Vec<ProjectionStructuralBlock>) {
  let assets = document.assets.clone();
  let theme = document.theme.clone();
  let frontier = document.frontier.clone();
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
  rebuilt.frontier = frontier;
  rebuilt.sections = document.sections.clone();
  rebuilt.ids.document_id = document_id;
  rebuilt.ids.block_ids = block_ids;
  rebuilt.ids.paragraph_ids = paragraph_ids;
  debug_assert_eq!(rebuilt.ids.block_ids.len(), rebuilt.blocks.len());
  debug_assert_eq!(rebuilt.ids.paragraph_ids.len(), rebuilt.paragraphs.len());
  rebuild_document_sections(&mut rebuilt);
  *document = rebuilt;
}


#[hotpath::measure]
fn clamp_selection_to_document(document: &DocumentProjection, selection: &mut EditorSelection) {
  selection.anchor = clamp_offset_to_document(document, selection.anchor);
  selection.head = clamp_offset_to_document(document, selection.head);
}

#[hotpath::measure]
fn clamp_offset_to_document(document: &DocumentProjection, offset: DocumentOffset) -> DocumentOffset {
  let paragraph = offset.paragraph.min(document.paragraphs.len().saturating_sub(1));
  let byte = clamp_paragraph_byte_to_char_boundary(document, paragraph, offset.byte);
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
