//! Translation from flowtext canonical operations into Loro operations.

use anyhow::{Context as _, Result, bail};
use std::ops::Range;

use gpui_flowtext::{Block, BlockId, CanonicalOperation, Document, DocumentSpan, ParagraphId, RunStyles, full_document_text};
use loro::{CommitOptions, LoroDoc, LoroMap, LoroMovableList, LoroValue, ValueOrContainer};

use crate::{
  binding::{BindingRow, BlockKind, DocBinding, block_version},
  projection::{insert_object_container, insert_paragraph_container, replace_blocks_from_document, replace_body_from_document},
  schema::{
    BLOCKS, BlockPayload, DATA, KIND, KIND_EQUATION, KIND_IMAGE, KIND_TABLE, REV, STYLE, body_text, decode_paragraph_style,
    encode_paragraph_style, payload_from_block, set_paragraph_style_utf8, set_run_styles_utf8, text_content,
  },
};

pub struct LocalApplier<'s> {
  pub doc: &'s LoroDoc,
  pub binding: &'s mut DocBinding,
}

impl LocalApplier<'_> {
  pub fn apply(&mut self, document: &Document, ops: &[CanonicalOperation]) -> Result<()> {
    for op in ops {
      self.apply_one(document, op)?;
    }
    if !ops.is_empty() {
      self
        .doc
        .commit_with(CommitOptions::new().origin("local-edit"));
    }
    self.binding.refresh_versions(document);
    self.binding.assert_consistent(self.doc, document);
    Ok(())
  }

  fn apply_one(&mut self, document: &Document, op: &CanonicalOperation) -> Result<()> {
    match op {
      CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => self.insert_text(document, *paragraph, *byte, text, *styles),
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => self.delete_range(document, *start_paragraph, *start_byte, *end_paragraph, *end_byte),
      CanonicalOperation::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      } => self.split_paragraph(document, *paragraph, *byte, *new_paragraph),
      CanonicalOperation::JoinParagraphs { first, second } => self.join_paragraphs(document, *first, *second),
      CanonicalOperation::SetParagraphStyle { paragraph, style } => {
        let row = self.row_for_paragraph(*paragraph)?;
        row.map.insert(STYLE, encode_paragraph_style(*style))?;
        let range = self.body_range_for_paragraph(*paragraph)?;
        set_paragraph_style_utf8(&body_text(self.doc), range, *style)?;
        Ok(())
      },
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => {
        let start = self.body_byte_for_paragraph_byte(*paragraph, range.start)?;
        let end = self.body_byte_for_paragraph_byte(*paragraph, range.end)?;
        set_run_styles_utf8(&body_text(self.doc), start..end, *styles)?;
        Ok(())
      },
      CanonicalOperation::InsertBlock { block, block_ix } => self.insert_block(document, *block, *block_ix),
      CanonicalOperation::DeleteBlock { block } => self.delete_block(document, *block),
      CanonicalOperation::MoveBlock { block, new_block_ix } => self.move_block(document, *block, *new_block_ix),
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => self.replace_paragraph_span(document, *start_paragraph, before, after),
      CanonicalOperation::ReplaceBlock { block } => self.replace_block(document, *block),
      CanonicalOperation::ReplaceDocument => self.replace_document(document),
    }
  }

  fn insert_text(&mut self, document: &Document, paragraph: ParagraphId, byte: usize, value: &str, styles: RunStyles) -> Result<()> {
    let byte = self.body_byte_for_paragraph_byte(paragraph, byte)?;
    let body = body_text(self.doc);
    body.insert_utf8(byte, value)?;
    set_run_styles_utf8(&body, byte..byte + value.len(), styles)?;
    self.debug_assert_body_matches_document(document);
    Ok(())
  }

  fn delete_range(
    &mut self,
    document: &Document,
    start_paragraph: ParagraphId,
    start: usize,
    end_paragraph: ParagraphId,
    end: usize,
  ) -> Result<()> {
    let start_ix = self.row_ix_for_paragraph(start_paragraph)?;
    let end_ix = self.row_ix_for_paragraph(end_paragraph)?;
    let start_byte = self.body_byte_for_paragraph_byte(start_paragraph, start)?;
    let end_byte = self.body_byte_for_paragraph_byte(end_paragraph, end)?;
    if end_byte > start_byte {
      body_text(self.doc).delete_utf8(start_byte, end_byte - start_byte)?;
    }
    if start_ix == end_ix {
      self.debug_assert_body_matches_document(document);
      return Ok(());
    }
    if start_ix > end_ix {
      bail!("cross-paragraph delete start row is after end row");
    }

    let start_paragraph_ix = self
      .paragraph_ordinal_for_row(start_ix)
      .context("delete start row is not a paragraph")?;
    let end_paragraph_ix = self
      .paragraph_ordinal_for_row(end_ix)
      .context("delete end row is not a paragraph")?;
    if end_paragraph_ix > start_paragraph_ix {
      let deleted_rows = self.paragraph_row_indices(start_paragraph_ix + 1, end_paragraph_ix - start_paragraph_ix)?;
      let blocks = self.blocks();
      for row_ix in deleted_rows.into_iter().rev() {
        blocks.delete(row_ix, 1)?;
        self
          .binding
          .remove_row(row_ix)
          .context("DocBinding paragraph row disappeared during cross-paragraph delete")?;
      }
    }
    self.debug_assert_body_matches_document(document);
    Ok(())
  }

  fn split_paragraph(&mut self, document: &Document, paragraph: ParagraphId, byte: usize, new_paragraph: ParagraphId) -> Result<()> {
    let row_ix = self.row_ix_for_paragraph(paragraph)?;
    let insert_ix = row_ix + 1;
    let block_id = document
      .ids
      .block_ids
      .get(insert_ix)
      .copied()
      .context("post-split document is missing the new block id")?;
    let version = document
      .blocks
      .get(insert_ix)
      .map(block_version)
      .context("post-split document is missing the new block")?;
    let body_byte = self.body_byte_for_paragraph_byte(paragraph, byte)?;
    body_text(self.doc).insert_utf8(body_byte, "\n")?;

    let blocks = self.blocks();
    let paragraph_ix = self
      .paragraph_ordinal_for_row(row_ix)
      .context("split row is not a paragraph")?
      .saturating_add(1);
    let style = document
      .paragraphs
      .get(paragraph_ix)
      .map_or_else(
        || {
          self
            .binding
            .rows
            .get(row_ix)
            .and_then(|row| map_i64(&row.map, STYLE).ok())
            .map(decode_paragraph_style)
            .unwrap_or(gpui_flowtext::ParagraphStyle::Normal)
        },
        |paragraph| paragraph.style,
      );
    let map = insert_paragraph_container(&blocks, insert_ix, style)?;
    self.binding.insert_row(
      insert_ix,
      BindingRow {
        map,
        kind: BlockKind::Paragraph,
        block_id,
        paragraph_id: Some(new_paragraph),
        version,
      },
    );
    self.debug_assert_body_matches_document(document);
    Ok(())
  }

  fn join_paragraphs(&mut self, document: &Document, first: ParagraphId, second: ParagraphId) -> Result<()> {
    let first_ix = self.row_ix_for_paragraph(first)?;
    let first_paragraph_ix = self
      .paragraph_ordinal_for_row(first_ix)
      .context("join first row is not a paragraph")?;
    let first_len = self
      .body_paragraph_range(first_paragraph_ix)?
      .len();
    self.delete_range(document, first, first_len, second, 0)
  }

  fn insert_block(&mut self, document: &Document, block_id: BlockId, block_ix: usize) -> Result<()> {
    let block = document
      .blocks
      .get(block_ix)
      .context("inserted block index is out of bounds in the post-edit document")?;
    match block {
      Block::Paragraph(paragraph) => {
        replace_body_from_document(self.doc, document)?;
        let paragraph_ix = document_paragraph_ix_for_block(document, block_ix)?;
        let paragraph_id = document
          .ids
          .paragraph_ids
          .get(paragraph_ix)
          .copied()
          .context("inserted paragraph block is missing a paragraph id")?;
        self.insert_paragraph_row(
          block_ix,
          block_id,
          paragraph_id,
          paragraph.style,
          block_version(block),
        )?;
        self.debug_assert_body_matches_document(document);
        Ok(())
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
        let blocks = self.blocks();
        let map = insert_object_container(&blocks, block_ix, block, document)?;
        self.binding.insert_row(
          block_ix,
          BindingRow {
            map,
            kind: BlockKind::from_block(block),
            block_id,
            paragraph_id: None,
            version: block_version(block),
          },
        );
        Ok(())
      },
    }
  }

  fn delete_block(&mut self, document: &Document, block: BlockId) -> Result<()> {
    let row_ix = self.row_ix_for_block(block)?;
    let was_paragraph = matches!(
      self.binding.rows.get(row_ix).map(|row| row.kind),
      Some(BlockKind::Paragraph)
    );
    self.blocks().delete(row_ix, 1)?;
    self
      .binding
      .remove_row(row_ix)
      .context("DocBinding block row disappeared during block delete")?;
    if was_paragraph {
      replace_body_from_document(self.doc, document)?;
      self.debug_assert_body_matches_document(document);
    }
    Ok(())
  }

  fn move_block(&mut self, document: &Document, block: BlockId, new_block_ix: usize) -> Result<()> {
    let old_ix = self.row_ix_for_block(block)?;
    let was_paragraph = matches!(
      self.binding.rows.get(old_ix).map(|row| row.kind),
      Some(BlockKind::Paragraph)
    );
    self.blocks().mov(old_ix, new_block_ix)?;
    self.binding.move_row(old_ix, new_block_ix);
    if was_paragraph {
      replace_body_from_document(self.doc, document)?;
      self.debug_assert_body_matches_document(document);
    }
    Ok(())
  }

  fn replace_paragraph_span(
    &mut self,
    document: &Document,
    start_paragraph: Option<ParagraphId>,
    before: &DocumentSpan,
    after: &DocumentSpan,
  ) -> Result<()> {
    let _ = (start_paragraph, before, after);
    self.replace_document(document)
  }

  fn replace_block(&mut self, document: &Document, block: Option<BlockId>) -> Result<()> {
    let row_ix = match block {
      Some(block) => self.binding.by_block.get(&block).copied(),
      None => self.single_changed_object_row(document),
    };
    if let Some(row_ix) = row_ix
      && self.replace_block_at(document, row_ix)?
    {
      return Ok(());
    }
    self.replace_document(document)
  }

  fn replace_document(&mut self, document: &Document) -> Result<()> {
    replace_blocks_from_document(self.doc, document)?;
    *self.binding = DocBinding::build(self.doc, document)?;
    Ok(())
  }

  fn replace_block_at(&mut self, document: &Document, row_ix: usize) -> Result<bool> {
    let Some(block) = document.blocks.get(row_ix) else {
      return Ok(false);
    };
    let Some(payload) = payload_from_block(block, &document.assets) else {
      return Ok(false);
    };
    let row = self
      .binding
      .rows
      .get_mut(row_ix)
      .context("replacement block row is out of bounds")?;
    row.map.insert(KIND, kind_for_payload(&payload))?;
    row
      .map
      .insert(DATA, LoroValue::Binary(postcard::to_stdvec(&payload)?.into()))?;
    row
      .map
      .insert(REV, map_i64_or(&row.map, REV, 0)?.saturating_add(1))?;
    row.kind = BlockKind::from_block(block);
    row.paragraph_id = None;
    row.version = block_version(block);
    self.binding.rebuild_indexes();
    Ok(true)
  }

  fn insert_paragraph_row(
    &mut self,
    row_ix: usize,
    block_id: BlockId,
    paragraph_id: ParagraphId,
    style: gpui_flowtext::ParagraphStyle,
    version: u64,
  ) -> Result<()> {
    let blocks = self.blocks();
    let map = insert_paragraph_container(&blocks, row_ix, style)?;
    self.binding.insert_row(
      row_ix,
      BindingRow {
        map,
        kind: BlockKind::Paragraph,
        block_id,
        paragraph_id: Some(paragraph_id),
        version,
      },
    );
    Ok(())
  }

  fn single_changed_object_row(&self, document: &Document) -> Option<usize> {
    let mut changed = None;
    for (row_ix, row) in self.binding.rows.iter().enumerate() {
      let block = document.blocks.get(row_ix)?;
      if matches!(block, Block::Paragraph(_)) || row.version == block_version(block) {
        continue;
      }
      if changed.replace(row_ix).is_some() {
        return None;
      }
    }
    changed
  }

  fn row_for_paragraph(&self, paragraph: ParagraphId) -> Result<&BindingRow> {
    let ix = self.row_ix_for_paragraph(paragraph)?;
    self
      .binding
      .rows
      .get(ix)
      .context("DocBinding paragraph index is out of bounds")
  }

  fn debug_assert_body_matches_document(&self, document: &Document) {
    debug_assert_eq!(text_content(&body_text(self.doc)), full_document_text(document));
  }

  fn row_ix_for_paragraph(&self, paragraph: ParagraphId) -> Result<usize> {
    self
      .binding
      .by_paragraph
      .get(&paragraph)
      .copied()
      .context("paragraph id is not present in DocBinding")
  }

  fn row_ix_for_block(&self, block: BlockId) -> Result<usize> {
    self
      .binding
      .by_block
      .get(&block)
      .copied()
      .context("block id is not present in DocBinding")
  }

  fn paragraph_ordinal_for_row(&self, target_row: usize) -> Option<usize> {
    let mut paragraph_ix = 0;
    for (row_ix, row) in self.binding.rows.iter().enumerate() {
      if row_ix == target_row {
        return row.paragraph_id.map(|_| paragraph_ix);
      }
      if row.paragraph_id.is_some() {
        paragraph_ix += 1;
      }
    }
    None
  }

  fn paragraph_row_indices(&self, start: usize, count: usize) -> Result<Vec<usize>> {
    let mut rows = Vec::with_capacity(count);
    let mut paragraph_ix = 0;
    for (row_ix, row) in self.binding.rows.iter().enumerate() {
      if row.paragraph_id.is_none() {
        continue;
      }
      if paragraph_ix >= start && rows.len() < count {
        rows.push(row_ix);
      }
      paragraph_ix += 1;
    }
    if rows.len() != count {
      bail!("paragraph span references rows outside DocBinding");
    }
    Ok(rows)
  }

  fn blocks(&self) -> LoroMovableList {
    self.doc.get_movable_list(BLOCKS)
  }

  fn body_byte_for_paragraph_byte(&self, paragraph: ParagraphId, byte: usize) -> Result<usize> {
    let row_ix = self.row_ix_for_paragraph(paragraph)?;
    let paragraph_ix = self
      .paragraph_ordinal_for_row(row_ix)
      .context("paragraph row has no paragraph ordinal")?;
    let range = self.body_paragraph_range(paragraph_ix)?;
    Ok(range.start + byte.min(range.len()))
  }

  fn body_range_for_paragraph(&self, paragraph: ParagraphId) -> Result<Range<usize>> {
    let row_ix = self.row_ix_for_paragraph(paragraph)?;
    let paragraph_ix = self
      .paragraph_ordinal_for_row(row_ix)
      .context("paragraph row has no paragraph ordinal")?;
    self.body_paragraph_range(paragraph_ix)
  }

  fn body_paragraph_range(&self, target_paragraph_ix: usize) -> Result<Range<usize>> {
    paragraph_range_in_body_text(&text_content(&body_text(self.doc)), target_paragraph_ix)
      .context("paragraph ordinal is outside the body text")
  }
}

fn document_paragraph_ix_for_block(document: &Document, target_block_ix: usize) -> Result<usize> {
  let mut paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(_) if block_ix == target_block_ix => return Ok(paragraph_ix),
      Block::Paragraph(_) => paragraph_ix += 1,
      Block::Image(_) | Block::Equation(_) | Block::Table(_) if block_ix == target_block_ix => {
        bail!("target block is not a paragraph")
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {},
    }
  }
  bail!("target block index is out of bounds")
}

fn kind_for_payload(payload: &BlockPayload) -> &'static str {
  match payload {
    BlockPayload::Image { .. } => KIND_IMAGE,
    BlockPayload::Equation { .. } => KIND_EQUATION,
    BlockPayload::Table(_) => KIND_TABLE,
  }
}

fn paragraph_range_in_body_text(text: &str, target_paragraph_ix: usize) -> Option<Range<usize>> {
  let mut paragraph_ix = 0usize;
  let mut start = 0usize;
  for (byte, ch) in text.char_indices() {
    if ch != '\n' {
      continue;
    }
    if paragraph_ix == target_paragraph_ix {
      return Some(start..byte);
    }
    paragraph_ix += 1;
    start = byte + ch.len_utf8();
  }
  (paragraph_ix == target_paragraph_ix).then_some(start..text.len())
}

fn map_i64(map: &LoroMap, key: &str) -> Result<i64> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::I64(value))) => Ok(value),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => {
      bail!("collaboration map key {key} is not an i64")
    },
  }
}

fn map_i64_or(map: &LoroMap, key: &str, default: i64) -> Result<i64> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::I64(value))) => Ok(value),
    None => Ok(default),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) => {
      bail!("collaboration map key {key} is not an i64")
    },
  }
}
