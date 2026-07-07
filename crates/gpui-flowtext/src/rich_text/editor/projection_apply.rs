#[hotpath::measure_all]
impl RichTextEditor {
  // Asset bytes are transferred out-of-band; remote document content is applied
  // by replacing the Loro projection snapshot, not by feeding document patches.
  pub fn apply_synced_asset_records(&mut self, asset_records: &[(AssetId, AssetRecord)], cx: &mut Context<Self>) {
    if asset_records.is_empty() {
      return;
    }
    for (id, record) in asset_records {
      self.document.assets.assets.insert(*id, record.clone());
    }
    // Asset availability is out-of-band cache state: repaint, but never dirty
    // the document or advance the edit generation.
    self.after_formatting_mutation(cx);
  }

  /// Apply a REMOTE projection patch batch to THE document (Loro-first spec
  /// §6): single projection, no committed/visible split, no pending-edit
  /// replay, no stable-selection resolve pass — the caret clamps and the next
  /// local intent re-resolves by identity.
  pub fn apply_remote_patch_batch(&mut self, batch: &ProjectionPatchBatch, cx: &mut Context<Self>) -> Result<(), ProjectionApplyError> {
    let mut document = self.document.clone();
    apply_projection_patch_batch(&mut document, batch)?;
    let theme = self.document.theme.clone();
    self.document = projection_with_local_theme(document, &theme);
    self.identity_map.reconcile(&self.document);
    let head = self.clamp_offset_to_document(self.selection.head);
    let anchor = self.clamp_offset_to_document(self.selection.anchor);
    if head != self.selection.head || anchor != self.selection.anchor {
      self.selection = EditorSelection::range(anchor, head);
      self.emit_selection_changed(cx);
    }
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed(generation, false, cx);
    // Remote patches update the editor in place but never scroll the viewport
    // as if the local user typed.
    self.pending_scroll_head_after_layout = false;
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

#[hotpath::measure]
pub fn apply_projection_patch_batch(document: &mut DocumentProjection, batch: &ProjectionPatchBatch) -> Result<(), ProjectionApplyError> {
  if document.frontier != batch.base_frontier {
    return Err(ProjectionApplyError::StaleFrontier {
      expected: batch.base_frontier.clone(),
      actual: document.frontier.clone(),
    });
  }
  let mut candidate = hotpath::measure_block!("editor_patch_candidate_clone", document.clone());
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
  let mut structural_blocks: Option<Vec<ProjectionPatchBlock>> = None;
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
          let target = blocks.get_mut(row).ok_or(ProjectionApplyError::MissingBlock {
            block_id: *block_id,
            row_hint: *row_hint,
          })?;
          if target.paragraph_id != Some(*paragraph_id) {
            return Err(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            });
          }
          outline_dirty |= projection_patch_block_paragraph_style(document, target, *block_id)? != new.style;
          target.payload = ProjectionPatchBlockPayload::Paragraph(new.clone());
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
          let target = blocks.get_mut(row).ok_or(ProjectionApplyError::MissingBlock {
            block_id: *block_id,
            row_hint: *row_hint,
          })?;
          if target.paragraph_id != Some(*paragraph_id) {
            return Err(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            });
          }
          let paragraph = materialize_projection_patch_paragraph(document, target, *block_id)?;
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
          let target = blocks.get_mut(row).ok_or(ProjectionApplyError::MissingBlock {
            block_id: *block_id,
            row_hint: *row_hint,
          })?;
          if target.paragraph_id != Some(*paragraph_id) {
            return Err(ProjectionApplyError::MissingParagraph {
              paragraph_id: *paragraph_id,
              block_id: *block_id,
            });
          }
          let paragraph = materialize_projection_patch_paragraph(document, target, *block_id)?;
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
        if let Some(blocks) = structural_blocks.as_mut() {
          let row = structural_block_ix_for_patch(blocks, *block_id, *row_hint)?;
          if blocks[row].paragraph_id.is_some()
            || block.paragraph_id.is_some()
            || matches!(&block.block, InputBlock::Paragraph(_))
          {
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
          blocks[row] = ProjectionPatchBlock::from_structural_block(block)?;
        } else {
          replace_object_projection_patch(document, *block_id, *row_hint, block)?;
        }
        structural_changed = true;
      },
      ProjectionPatch::InsertBlocks {
        before,
        row_hint,
        blocks: inserted,
      } => {
        if structural_blocks.is_none() && inserted.iter().all(projection_structural_block_is_object) {
          let row = anchor_ix_for_document_insert(document, *before, *row_hint)?;
          for (offset, block) in inserted.iter().enumerate() {
            insert_projection_structural_block(document, row + offset, (*block).clone())?;
          }
          structural_changed = true;
        } else {
          let blocks = structural_blocks
            .get_or_insert_with(|| projection_patch_blocks_from_document(document));
          let row = anchor_ix_for_insert_blocks(blocks, *before, *row_hint)?;
          let inserted = inserted
            .iter()
            .map(ProjectionPatchBlock::from_structural_block)
            .collect::<Result<Vec<_>, _>>()?;
          blocks.splice(row..row, inserted);
          structural_changed = true;
          outline_dirty = true;
        }
      },
      ProjectionPatch::DeleteBlocks { block_ids, row_hint } => {
        if structural_blocks.is_none() && block_ids.iter().all(|block_id| document_block_is_object(document, *block_id, *row_hint)) {
          let mut rows = block_ids
            .iter()
            .map(|block_id| block_ix_for_patch(document, *block_id, *row_hint))
            .collect::<Result<Vec<_>, _>>()?;
          rows.sort_unstable();
          for window in rows.windows(2) {
            if window[0] == window[1] {
              return Err(ProjectionApplyError::DuplicateBlockId(document.ids.block_ids[window[0]]));
            }
          }
          for row in rows.into_iter().rev() {
            delete_projection_block_at(document, row)?;
          }
          structural_changed = true;
        } else {
          let blocks = structural_blocks
            .get_or_insert_with(|| projection_patch_blocks_from_document(document));
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
        }
      },
      ProjectionPatch::MoveBlock {
        block_id,
        before,
        from_hint,
        to_hint,
      } => {
        if structural_blocks.is_none() && document_block_is_object(document, *block_id, *from_hint) {
          let from = block_ix_for_patch(document, *block_id, *from_hint)?;
          let to = anchor_ix_for_document_move(document, from, *before, *to_hint)?;
          move_projection_block(document, from, to)?;
          structural_changed = true;
        } else {
          let blocks = structural_blocks
            .get_or_insert_with(|| projection_patch_blocks_from_document(document));
          let from = structural_block_ix_for_patch(blocks, *block_id, *from_hint)?;
          let block = blocks.remove(from);
          let to = anchor_ix_for_insert_blocks(blocks, *before, *to_hint)?;
          blocks.insert(to, block);
          structural_changed = true;
          outline_dirty = true;
        }
      },
      ProjectionPatch::AssetArrived { id, record } => {
        document.assets.assets.insert(*id, record.clone());
      },
    }
  }

  let rebuilt_structural = structural_blocks.is_some();
  if let Some(blocks) = structural_blocks {
    validate_projection_patch_blocks(&blocks)?;
    rebuild_document_from_projection_patch_blocks(document, blocks)?;
  } else if outline_dirty {
    rebuild_document_sections(document);
  }
  if rebuilt_structural && outline_dirty {
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

#[derive(Clone)]
struct ProjectionPatchBlock {
  block_id: BlockId,
  paragraph_id: Option<ParagraphId>,
  payload: ProjectionPatchBlockPayload,
}

#[derive(Clone)]
enum ProjectionPatchBlockPayload {
  ExistingParagraph { paragraph_ix: usize },
  Paragraph(InputParagraph),
  ExistingObject { block_ix: usize },
  Object(InputBlock),
}

impl ProjectionPatchBlock {
  fn from_structural_block(block: &ProjectionStructuralBlock) -> Result<Self, ProjectionApplyError> {
    match &block.block {
      InputBlock::Paragraph(paragraph) => {
        let Some(paragraph_id) = block.paragraph_id else {
          return Err(ProjectionApplyError::InvalidStructuralPatch(
            "paragraph structural block must carry a paragraph id",
          ));
        };
        Ok(Self {
          block_id: block.block_id,
          paragraph_id: Some(paragraph_id),
          payload: ProjectionPatchBlockPayload::Paragraph(paragraph.clone()),
        })
      },
      InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => {
        if block.paragraph_id.is_some() {
          return Err(ProjectionApplyError::InvalidStructuralPatch(
            "object structural block must not carry a paragraph id",
          ));
        }
        Ok(Self {
          block_id: block.block_id,
          paragraph_id: None,
          payload: ProjectionPatchBlockPayload::Object(block.block.clone()),
        })
      },
    }
  }
}

fn projection_patch_blocks_from_document(document: &DocumentProjection) -> Vec<ProjectionPatchBlock> {
  let mut paragraph_ix = 0;
  document
    .blocks
    .iter()
    .enumerate()
    .map(|(block_ix, block)| {
      let block_id = document.ids.block_ids.get(block_ix).copied().unwrap_or_else(new_block_id);
      match block {
        Block::Paragraph(_) => {
          let structural = ProjectionPatchBlock {
            block_id,
            paragraph_id: Some(
              document
                .ids
                .paragraph_ids
                .get(paragraph_ix)
                .copied()
                .unwrap_or_else(new_paragraph_id),
            ),
            payload: ProjectionPatchBlockPayload::ExistingParagraph { paragraph_ix },
          };
          paragraph_ix += 1;
          structural
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => ProjectionPatchBlock {
          block_id,
          paragraph_id: None,
          payload: ProjectionPatchBlockPayload::ExistingObject { block_ix },
        },
      }
    })
    .collect()
}

fn structural_block_ix_for_patch(
  blocks: &[ProjectionPatchBlock],
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

fn paragraph_ix_for_structural_row(blocks: &[ProjectionPatchBlock], row: usize) -> usize {
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
  blocks: &[ProjectionPatchBlock],
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

fn anchor_ix_for_document_insert(
  document: &DocumentProjection,
  before: Option<BlockId>,
  row_hint: usize,
) -> Result<usize, ProjectionApplyError> {
  let Some(before) = before else {
    return Ok(document.blocks.len());
  };
  if document.ids.block_ids.get(row_hint).copied() == Some(before) {
    return Ok(row_hint);
  }
  document
    .ids
    .block_ids
    .iter()
    .position(|block_id| *block_id == before)
    .ok_or(ProjectionApplyError::InvalidAnchor(before))
}

fn anchor_ix_for_document_move(
  document: &DocumentProjection,
  from: usize,
  before: Option<BlockId>,
  row_hint: usize,
) -> Result<usize, ProjectionApplyError> {
  let Some(before) = before else {
    return Ok(document.blocks.len().saturating_sub(1));
  };
  let adjusted_hint = if row_hint > from { row_hint.saturating_sub(1) } else { row_hint };
  if document
    .ids
    .block_ids
    .get(row_hint)
    .copied()
    .or_else(|| document.ids.block_ids.get(adjusted_hint).copied())
    == Some(before)
  {
    return Ok(adjusted_hint);
  }
  document
    .ids
    .block_ids
    .iter()
    .enumerate()
    .filter(|(ix, _)| *ix != from)
    .map(|(_, id)| *id)
    .position(|block_id| block_id == before)
    .ok_or(ProjectionApplyError::InvalidAnchor(before))
}

fn projection_structural_block_is_object(block: &ProjectionStructuralBlock) -> bool {
  block.paragraph_id.is_none() && matches!(&block.block, InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_))
}

fn document_block_is_object(document: &DocumentProjection, block_id: BlockId, row_hint: usize) -> bool {
  block_ix_for_patch(document, block_id, row_hint)
    .ok()
    .and_then(|row| document.blocks.get(row))
    .is_some_and(|block| matches!(block, Block::Image(_) | Block::Equation(_) | Block::Table(_)))
}

fn replace_object_projection_patch(
  document: &mut DocumentProjection,
  block_id: BlockId,
  row_hint: usize,
  block: &ProjectionStructuralBlock,
) -> Result<(), ProjectionApplyError> {
  if block.block_id != block_id {
    return Err(ProjectionApplyError::InvalidStructuralPatch(
      "object replacement must preserve the target block id",
    ));
  }
  if !projection_structural_block_is_object(block) {
    return Err(ProjectionApplyError::WrongBlockKind {
      block_id,
      expected: "an object block",
    });
  }
  let row = block_ix_for_patch(document, block_id, row_hint)?;
  if !matches!(document.blocks.get(row), Some(Block::Image(_) | Block::Equation(_) | Block::Table(_))) {
    return Err(ProjectionApplyError::WrongBlockKind {
      block_id,
      expected: "an object block",
    });
  }
  replace_projection_block(document, row, block_id, block.block.clone())
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

fn validate_projection_patch_blocks(blocks: &[ProjectionPatchBlock]) -> Result<(), ProjectionApplyError> {
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

fn projection_patch_block_paragraph_style(
  document: &DocumentProjection,
  block: &ProjectionPatchBlock,
  block_id: BlockId,
) -> Result<ParagraphStyle, ProjectionApplyError> {
  match &block.payload {
    ProjectionPatchBlockPayload::ExistingParagraph { paragraph_ix } => document
      .paragraphs
      .get(*paragraph_ix)
      .map(|paragraph| paragraph.style)
      .ok_or(ProjectionApplyError::MissingBlock {
        block_id,
        row_hint: *paragraph_ix,
      }),
    ProjectionPatchBlockPayload::Paragraph(paragraph) => Ok(paragraph.style),
    ProjectionPatchBlockPayload::ExistingObject { .. } | ProjectionPatchBlockPayload::Object(_) => {
      Err(ProjectionApplyError::WrongBlockKind {
        block_id,
        expected: "a paragraph",
      })
    },
  }
}

fn materialize_projection_patch_paragraph<'a>(
  document: &DocumentProjection,
  block: &'a mut ProjectionPatchBlock,
  block_id: BlockId,
) -> Result<&'a mut InputParagraph, ProjectionApplyError> {
  match &block.payload {
    ProjectionPatchBlockPayload::ExistingParagraph { paragraph_ix } => {
      let Some(paragraph) = document.paragraphs.get(*paragraph_ix) else {
        return Err(ProjectionApplyError::MissingBlock {
          block_id,
          row_hint: *paragraph_ix,
        });
      };
      block.payload = ProjectionPatchBlockPayload::Paragraph(input_paragraph_from_document_paragraph(
        document,
        *paragraph_ix,
        paragraph,
      ));
    },
    ProjectionPatchBlockPayload::Paragraph(_) => {},
    ProjectionPatchBlockPayload::ExistingObject { .. } | ProjectionPatchBlockPayload::Object(_) => {
      return Err(ProjectionApplyError::WrongBlockKind {
        block_id,
        expected: "a paragraph",
      });
    },
  }
  match &mut block.payload {
    ProjectionPatchBlockPayload::Paragraph(paragraph) => Ok(paragraph),
    ProjectionPatchBlockPayload::ExistingParagraph { .. }
    | ProjectionPatchBlockPayload::ExistingObject { .. }
    | ProjectionPatchBlockPayload::Object(_) => unreachable!("paragraph payload was just materialized"),
  }
}

#[hotpath::measure]
fn rebuild_document_from_projection_patch_blocks(
  document: &mut DocumentProjection,
  patch_blocks: Vec<ProjectionPatchBlock>,
) -> Result<(), ProjectionApplyError> {
  let mut text = String::new();
  let mut paragraphs = Vec::new();
  let mut blocks = Vec::with_capacity(patch_blocks.len().max(1));
  let mut block_ids = Vec::with_capacity(patch_blocks.len().max(1));
  let mut paragraph_ids = Vec::new();

  for patch_block in patch_blocks {
    block_ids.push(patch_block.block_id);
    match patch_block.payload {
      ProjectionPatchBlockPayload::ExistingParagraph { paragraph_ix } => {
        let Some(paragraph) = document.paragraphs.get(paragraph_ix).cloned() else {
          return Err(ProjectionApplyError::MissingBlock {
            block_id: patch_block.block_id,
            row_hint: paragraph_ix,
          });
        };
        let Some(paragraph_id) = patch_block.paragraph_id else {
          return Err(ProjectionApplyError::InvalidStructuralPatch(
            "paragraph structural block must carry a paragraph id",
          ));
        };
        let paragraph_text = paragraph_text(document, paragraph_ix);
        push_rebuilt_paragraph(
          &mut text,
          &mut paragraphs,
          &mut blocks,
          &mut paragraph_ids,
          paragraph,
          paragraph_id,
          &paragraph_text,
        );
      },
      ProjectionPatchBlockPayload::Paragraph(input) => {
        let Some(paragraph_id) = patch_block.paragraph_id else {
          return Err(ProjectionApplyError::InvalidStructuralPatch(
            "paragraph structural block must carry a paragraph id",
          ));
        };
        let paragraph_text = input_paragraph_text(&input);
        let paragraph = paragraph_from_input_paragraph(&input);
        push_rebuilt_paragraph(
          &mut text,
          &mut paragraphs,
          &mut blocks,
          &mut paragraph_ids,
          paragraph,
          paragraph_id,
          &paragraph_text,
        );
      },
      ProjectionPatchBlockPayload::ExistingObject { block_ix } => {
        let Some(block) = document.blocks.get(block_ix).cloned() else {
          return Err(ProjectionApplyError::MissingBlock {
            block_id: patch_block.block_id,
            row_hint: block_ix,
          });
        };
        if matches!(block, Block::Paragraph(_)) {
          return Err(ProjectionApplyError::WrongBlockKind {
            block_id: patch_block.block_id,
            expected: "an object block",
          });
        }
        blocks.push(block);
      },
      ProjectionPatchBlockPayload::Object(input) => {
        if matches!(&input, InputBlock::Paragraph(_)) {
          return Err(ProjectionApplyError::WrongBlockKind {
            block_id: patch_block.block_id,
            expected: "an object block",
          });
        }
        blocks.push(block_from_input_block(&input));
      },
    }
  }

  if paragraphs.is_empty() {
    let paragraph = Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    };
    let paragraph_id = new_paragraph_id();
    let block_id = new_block_id();
    block_ids.push(block_id);
    push_rebuilt_paragraph(
      &mut text,
      &mut paragraphs,
      &mut blocks,
      &mut paragraph_ids,
      paragraph,
      paragraph_id,
      "",
    );
  }

  let offset_index = ParagraphOffsetIndex::new(&paragraphs);
  document.text = Rope::from(text);
  document.paragraphs = Arc::new(paragraphs);
  document.blocks = Arc::new(blocks);
  document.ids.block_ids = block_ids;
  document.ids.paragraph_ids = paragraph_ids;
  document.offset_index = offset_index;
  reconcile_document_ids(document);
  Ok(())
}

#[allow(clippy::too_many_arguments, reason = "rebuilding projection shape threads parallel output vectors")]
fn push_rebuilt_paragraph(
  text: &mut String,
  paragraphs: &mut Vec<Paragraph>,
  blocks: &mut Vec<Block>,
  paragraph_ids: &mut Vec<ParagraphId>,
  mut paragraph: Paragraph,
  paragraph_id: ParagraphId,
  paragraph_text: &str,
) {
  if !paragraphs.is_empty() {
    text.push('\n');
  }
  let start = text.len();
  text.push_str(paragraph_text);
  paragraph.byte_range = start..text.len();
  paragraph_ids.push(paragraph_id);
  paragraphs.push(paragraph.clone());
  blocks.push(Block::Paragraph(paragraph));
}

fn insert_projection_structural_block(
  document: &mut DocumentProjection,
  row: usize,
  block: ProjectionStructuralBlock,
) -> Result<(), ProjectionApplyError> {
  if matches!(&block.block, InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_)) {
    if block.paragraph_id.is_some() {
      return Err(ProjectionApplyError::InvalidStructuralPatch(
        "object structural block must not carry a paragraph id",
      ));
    }
    if document.ids.block_ids.contains(&block.block_id) {
      return Err(ProjectionApplyError::DuplicateBlockId(block.block_id));
    }
    let row = row.min(document.blocks.len());
    Arc::make_mut(&mut document.blocks).insert(row, block_from_input_block(&block.block));
    document
      .ids
      .block_ids
      .insert(row.min(document.ids.block_ids.len()), block.block_id);
    reconcile_document_ids(document);
    return Ok(());
  }

  let mut blocks = projection_patch_blocks_from_document(document);
  let row = row.min(blocks.len());
  blocks.insert(row, ProjectionPatchBlock::from_structural_block(&block)?);
  validate_projection_patch_blocks(&blocks)?;
  rebuild_document_from_projection_patch_blocks(document, blocks)?;
  rebuild_document_sections(document);
  Ok(())
}

fn delete_projection_block_at(document: &mut DocumentProjection, row: usize) -> Result<(), ProjectionApplyError> {
  let missing_block_id = document.ids.block_ids.get(row).copied().unwrap_or(BlockId(0));
  let Some(block) = document.blocks.get(row) else {
    return Err(ProjectionApplyError::MissingBlock {
      block_id: missing_block_id,
      row_hint: row,
    });
  };
  if matches!(block, Block::Image(_) | Block::Equation(_) | Block::Table(_)) {
    Arc::make_mut(&mut document.blocks).remove(row);
    remove_block_ids(document, row..row + 1);
    reconcile_document_ids(document);
    return Ok(());
  }

  let mut blocks = projection_patch_blocks_from_document(document);
  if row >= blocks.len() {
    return Err(ProjectionApplyError::MissingBlock {
      block_id: missing_block_id,
      row_hint: row,
    });
  }
  blocks.remove(row);
  validate_projection_patch_blocks(&blocks)?;
  rebuild_document_from_projection_patch_blocks(document, blocks)?;
  rebuild_document_sections(document);
  Ok(())
}

fn move_projection_block(document: &mut DocumentProjection, from: usize, to: usize) -> Result<(), ProjectionApplyError> {
  let missing_block_id = document.ids.block_ids.get(from).copied().unwrap_or(BlockId(0));
  let Some(block) = document.blocks.get(from) else {
    return Err(ProjectionApplyError::MissingBlock {
      block_id: missing_block_id,
      row_hint: from,
    });
  };
  if matches!(block, Block::Image(_) | Block::Equation(_) | Block::Table(_)) {
    let blocks = Arc::make_mut(&mut document.blocks);
    let block = blocks.remove(from);
    let to = to.min(blocks.len());
    blocks.insert(to, block);
    if from < document.ids.block_ids.len() {
      let block_id = document.ids.block_ids.remove(from);
      document
        .ids
        .block_ids
        .insert(to.min(document.ids.block_ids.len()), block_id);
    }
    reconcile_document_ids(document);
    return Ok(());
  }

  let mut blocks = projection_patch_blocks_from_document(document);
  if from >= blocks.len() {
    return Err(ProjectionApplyError::MissingBlock {
      block_id: missing_block_id,
      row_hint: from,
    });
  }
  let block = blocks.remove(from);
  blocks.insert(to.min(blocks.len()), block);
  validate_projection_patch_blocks(&blocks)?;
  rebuild_document_from_projection_patch_blocks(document, blocks)?;
  rebuild_document_sections(document);
  Ok(())
}

fn replace_projection_block(
  document: &mut DocumentProjection,
  row: usize,
  block_id: BlockId,
  after: InputBlock,
) -> Result<(), ProjectionApplyError> {
  let paragraph_id = paragraph_ix_for_block_row(document, row).and_then(|paragraph_ix| document.ids.paragraph_ids.get(paragraph_ix).copied());
  if matches!(document.blocks.get(row), Some(Block::Image(_) | Block::Equation(_) | Block::Table(_)))
    && matches!(&after, InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_))
  {
    if document
      .ids
      .block_ids
      .iter()
      .enumerate()
      .any(|(ix, id)| ix != row && *id == block_id)
    {
      return Err(ProjectionApplyError::DuplicateBlockId(block_id));
    }
    if let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(row) {
      *block = block_from_input_block(&after);
    }
    if row < document.ids.block_ids.len() {
      document.ids.block_ids[row] = block_id;
    }
    reconcile_document_ids(document);
    return Ok(());
  }

  let mut blocks = projection_patch_blocks_from_document(document);
  if row >= blocks.len() {
    return Err(ProjectionApplyError::MissingBlock { block_id, row_hint: row });
  }
  blocks[row] = ProjectionPatchBlock::from_structural_block(&structural_block_for_input(block_id, paragraph_id, after))?;
  validate_projection_patch_blocks(&blocks)?;
  rebuild_document_from_projection_patch_blocks(document, blocks)?;
  rebuild_document_sections(document);
  Ok(())
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
