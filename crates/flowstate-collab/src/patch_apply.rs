//! Headless application of collaboration patches to flowtext documents.

use anyhow::{Context as _, Result};
use gpui_flowtext::{
  Block, CollabPatch, CollabStructuralBlock, Document, InputBlock, InputParagraph, InputRun, document_from_input_blocks,
  input_block_from_block, paragraph_text, paragraphs_mut, rebuild_document_sections, update_paragraph_block,
};
use loro::LoroDoc;

use crate::binding::DocBinding;

pub fn apply_patches(document: &mut Document, binding: &mut DocBinding, doc: &LoroDoc, patches: &[CollabPatch]) -> Result<()> {
  for patch in patches {
    apply_one(document, patch)?;
  }
  binding.refresh_versions(document);
  binding.assert_consistent(doc, document);
  Ok(())
}

fn apply_one(document: &mut Document, patch: &CollabPatch) -> Result<()> {
  match patch {
    CollabPatch::ParagraphText { row, new, .. } => {
      replace_paragraph_block(document, *row, new)?;
    },
    CollabPatch::ParagraphStyle { row, style } => {
      if let Some(paragraph_ix) = paragraph_ix_for_block(document, *row)
        && let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix)
      {
        paragraph.style = *style;
        gpui_flowtext::bump_paragraph_version(paragraph);
        update_paragraph_block(document, paragraph_ix);
        rebuild_document_sections(document);
      }
    },
    CollabPatch::ReplaceObjectBlock { row, block } => {
      let mut blocks = structural_blocks_from_document(document)?;
      if *row < blocks.len() {
        blocks[*row] = block.clone();
        rebuild_document_from_structural_blocks(document, blocks)?;
      }
    },
    CollabPatch::InsertBlocks { row, blocks: inserted } => {
      let mut blocks = structural_blocks_from_document(document)?;
      let row = (*row).min(blocks.len());
      blocks.splice(row..row, inserted.iter().cloned());
      rebuild_document_from_structural_blocks(document, blocks)?;
    },
    CollabPatch::DeleteBlocks { row, count } => {
      let mut blocks = structural_blocks_from_document(document)?;
      let start = (*row).min(blocks.len());
      let end = start.saturating_add(*count).min(blocks.len());
      blocks.drain(start..end);
      rebuild_document_from_structural_blocks(document, blocks)?;
    },
    CollabPatch::MoveBlock { from, to } => {
      let mut blocks = structural_blocks_from_document(document)?;
      if *from < blocks.len() {
        let block = blocks.remove(*from);
        blocks.insert((*to).min(blocks.len()), block);
        rebuild_document_from_structural_blocks(document, blocks)?;
      }
    },
    CollabPatch::AssetArrived { .. } => {},
  }
  Ok(())
}

fn replace_paragraph_block(document: &mut Document, row: usize, paragraph: &InputParagraph) -> Result<()> {
  let mut blocks = structural_blocks_from_document(document)?;
  let Some(block) = blocks.get_mut(row) else {
    return Ok(());
  };
  if !matches!(block.block, InputBlock::Paragraph(_)) {
    return Ok(());
  }
  block.block = InputBlock::Paragraph(paragraph.clone());
  rebuild_document_from_structural_blocks(document, blocks)
}

fn structural_blocks_from_document(document: &Document) -> Result<Vec<CollabStructuralBlock>> {
  let mut paragraph_ix = 0;
  document
    .blocks
    .iter()
    .enumerate()
    .map(|(block_ix, block)| {
      let block_id = document
        .ids
        .block_ids
        .get(block_ix)
        .copied()
        .context("document block is missing its collaboration id")?;
      match block {
        Block::Paragraph(paragraph) => {
          let paragraph_id = document
            .ids
            .paragraph_ids
            .get(paragraph_ix)
            .copied()
            .context("document paragraph is missing its collaboration id")?;
          let input = input_paragraph_from_document_paragraph(document, paragraph_ix, paragraph);
          paragraph_ix += 1;
          Ok(CollabStructuralBlock {
            block_id,
            paragraph_id: Some(paragraph_id),
            block: InputBlock::Paragraph(input),
          })
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => Ok(CollabStructuralBlock {
          block_id,
          paragraph_id: None,
          block: input_block_from_block(block),
        }),
      }
    })
    .collect()
}

fn input_paragraph_from_document_paragraph(document: &Document, paragraph_ix: usize, paragraph: &gpui_flowtext::Paragraph) -> InputParagraph {
  let text = paragraph_text(document, paragraph_ix);
  let mut byte = 0usize;
  InputParagraph {
    style: paragraph.style,
    runs: paragraph
      .runs
      .iter()
      .map(|run| {
        let start = byte;
        let end = start.saturating_add(run.len).min(text.len());
        byte = end;
        InputRun {
          text: text.get(start..end).unwrap_or_default().to_string(),
          styles: run.styles,
        }
      })
      .collect(),
  }
}

fn rebuild_document_from_structural_blocks(document: &mut Document, blocks: Vec<CollabStructuralBlock>) -> Result<()> {
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
      InputBlock::Paragraph(_) => Some(block.paragraph_id.context("structural paragraph patch is missing a paragraph id")),
      InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
    })
    .collect::<Result<Vec<_>>>()?;

  let mut rebuilt = document_from_input_blocks(theme, input_blocks);
  rebuilt.assets = assets;
  rebuilt.ids.document_id = document_id;
  rebuilt.ids.block_ids = block_ids;
  rebuilt.ids.paragraph_ids = paragraph_ids;
  rebuild_document_sections(&mut rebuilt);
  debug_assert_eq!(rebuilt.ids.block_ids.len(), rebuilt.blocks.len());
  debug_assert_eq!(rebuilt.ids.paragraph_ids.len(), rebuilt.paragraphs.len());
  *document = rebuilt;
  Ok(())
}

fn paragraph_ix_for_block(document: &Document, target_row: usize) -> Option<usize> {
  let mut paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    if !matches!(block, Block::Paragraph(_)) {
      continue;
    }
    if block_ix == target_row {
      return Some(paragraph_ix);
    }
    paragraph_ix += 1;
  }
  None
}
