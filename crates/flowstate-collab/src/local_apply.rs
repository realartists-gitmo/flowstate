//! Translation from flowtext canonical operations into Loro operations.

use anyhow::{Context as _, Result, bail};
use gpui_flowtext::{
  Block, BlockId, CanonicalOperation, Document, DocumentSpan, Paragraph, ParagraphId, RunStyles, TextRun, paragraph_text, paragraph_text_len,
};
use loro::{CommitOptions, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, TextDelta, UpdateOptions, ValueOrContainer, cursor::PosType};

use crate::{
  binding::{BindingRow, BlockKind, DocBinding, block_version},
  projection::{insert_object_container, insert_paragraph_container, replace_blocks_from_document},
  schema::{
    BLOCKS, BlockPayload, DATA, KIND, KIND_EQUATION, KIND_IMAGE, KIND_TABLE, MARK_HIGHLIGHT, MARK_SEMANTIC, MARK_STRIKE, MARK_UNDERLINE, REV,
    STYLE, apply_mark_intervals, debug_assert_paragraph_text_len, decode_paragraph_style, encode_paragraph_style, mark_intervals_from_runs,
    payload_from_block, run_styles_from_attrs, set_run_styles_utf8, unmark_utf8,
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
        Ok(())
      },
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => {
        let row = self.row_for_paragraph(*paragraph)?;
        let text = row
          .text
          .as_ref()
          .context("paragraph row is missing its LoroText")?;
        set_run_styles_utf8(text, range.clone(), *styles)?;
        self.debug_assert_row_text_len(document, self.row_ix_for_paragraph(*paragraph)?);
        Ok(())
      },
      CanonicalOperation::InsertBlock { block, block_ix } => self.insert_block(document, *block, *block_ix),
      CanonicalOperation::DeleteBlock { block } => self.delete_block(*block),
      CanonicalOperation::MoveBlock { block, new_block_ix } => self.move_block(*block, *new_block_ix),
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
    let row_ix = self.row_ix_for_paragraph(paragraph)?;
    let row = self
      .binding
      .rows
      .get(row_ix)
      .context("DocBinding paragraph index is out of bounds")?;
    let text = row
      .text
      .as_ref()
      .context("paragraph row is missing its LoroText")?;
    text.insert_utf8(byte, value)?;
    set_run_styles_utf8(text, byte..byte + value.len(), styles)?;
    self.debug_assert_row_text_len(document, row_ix);
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
    if start_ix == end_ix {
      self.delete_text_at_row(start_ix, start, end)?;
      self.debug_assert_row_text_len(document, start_ix);
      return Ok(());
    }
    if start_ix > end_ix {
      bail!("cross-paragraph delete start row is after end row");
    }

    let start_text = self.paragraph_text_at_row(start_ix)?;
    let end_text = self.paragraph_text_at_row(end_ix)?;
    let tail = end_text.slice_delta(end, end_text.len_utf8(), PosType::Bytes)?;
    let start_len = start_text.len_utf8();
    if start < start_len {
      start_text.delete_utf8(start, start_len - start)?;
    }

    let blocks = self.blocks();
    for row_ix in ((start_ix + 1)..end_ix).rev() {
      blocks.delete(row_ix, 1)?;
      self
        .binding
        .remove_row(row_ix)
        .context("DocBinding row disappeared during cross-paragraph delete")?;
    }

    insert_delta_at(&start_text, start, &tail)?;
    blocks.delete(start_ix + 1, 1)?;
    self
      .binding
      .remove_row(start_ix + 1)
      .context("DocBinding end row disappeared during cross-paragraph delete")?;
    self.debug_assert_row_text_len(document, start_ix);
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
    let row = self
      .binding
      .rows
      .get(row_ix)
      .context("DocBinding paragraph index is out of bounds")?;
    let style = decode_paragraph_style(map_i64(&row.map, STYLE)?);
    let text = row
      .text
      .as_ref()
      .context("paragraph row is missing its LoroText")?
      .clone();
    let tail = text.slice_delta(byte, text.len_utf8(), PosType::Bytes)?;
    if byte < text.len_utf8() {
      text.delete_utf8(byte, text.len_utf8() - byte)?;
    }
    self.debug_assert_row_text_len(document, row_ix);

    let blocks = self.blocks();
    let (map, new_text) = insert_paragraph_container(&blocks, insert_ix, style, &[], "")?;
    insert_delta_at(&new_text, 0, &tail)?;
    self.binding.insert_row(
      insert_ix,
      BindingRow {
        map,
        text: Some(new_text),
        kind: BlockKind::Paragraph,
        block_id,
        paragraph_id: Some(new_paragraph),
        version,
      },
    );
    self.debug_assert_row_text_len(document, insert_ix);
    Ok(())
  }

  fn join_paragraphs(&mut self, document: &Document, first: ParagraphId, second: ParagraphId) -> Result<()> {
    let first_ix = self.row_ix_for_paragraph(first)?;
    let first_len = self.paragraph_text_at_row(first_ix)?.len_utf8();
    self.delete_range(document, first, first_len, second, 0)
  }

  fn insert_block(&mut self, document: &Document, block_id: BlockId, block_ix: usize) -> Result<()> {
    let block = document
      .blocks
      .get(block_ix)
      .context("inserted block index is out of bounds in the post-edit document")?;
    match block {
      Block::Paragraph(paragraph) => {
        let paragraph_ix = document_paragraph_ix_for_block(document, block_ix)?;
        let paragraph_id = document
          .ids
          .paragraph_ids
          .get(paragraph_ix)
          .copied()
          .context("inserted paragraph block is missing a paragraph id")?;
        let text = paragraph_text(document, paragraph_ix);
        self.insert_paragraph_row(
          block_ix,
          block_id,
          paragraph_id,
          paragraph.style,
          &paragraph.runs,
          &text,
          block_version(block),
        )?;
        self.debug_assert_row_text_len(document, block_ix);
        Ok(())
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
        let blocks = self.blocks();
        let map = insert_object_container(&blocks, block_ix, block, document)?;
        self.binding.insert_row(
          block_ix,
          BindingRow {
            map,
            text: None,
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

  fn delete_block(&mut self, block: BlockId) -> Result<()> {
    let row_ix = self.row_ix_for_block(block)?;
    self.blocks().delete(row_ix, 1)?;
    self
      .binding
      .remove_row(row_ix)
      .context("DocBinding block row disappeared during block delete")?;
    Ok(())
  }

  fn move_block(&mut self, block: BlockId, new_block_ix: usize) -> Result<()> {
    let old_ix = self.row_ix_for_block(block)?;
    self.blocks().mov(old_ix, new_block_ix)?;
    self.binding.move_row(old_ix, new_block_ix);
    Ok(())
  }

  fn replace_paragraph_span(
    &mut self,
    document: &Document,
    start_paragraph: Option<ParagraphId>,
    before: &DocumentSpan,
    after: &DocumentSpan,
  ) -> Result<()> {
    let start = self.replace_span_start_ordinal(start_paragraph, before);
    let old_rows = self.paragraph_row_indices(start, before.paragraphs.len())?;
    let before_texts = span_paragraph_texts(before)?;
    let after_texts = span_paragraph_texts(after)?;
    let matched = before.paragraphs.len().min(after.paragraphs.len());

    for ix in 0..matched {
      self.update_matched_paragraph(
        document,
        old_rows[ix],
        &before.paragraphs[ix],
        &before_texts[ix],
        &after.paragraphs[ix],
        &after_texts[ix],
      )?;
    }

    if after.paragraphs.len() > matched {
      let base_insert_ix = if matched > 0 {
        old_rows[matched - 1] + 1
      } else {
        old_rows
          .first()
          .copied()
          .unwrap_or_else(|| self.row_ix_for_paragraph_insert_ordinal(start))
      };
      for (ix, (paragraph, text)) in after
        .paragraphs
        .iter()
        .zip(after_texts.iter())
        .enumerate()
        .skip(matched)
      {
        let insert_ix = base_insert_ix + ix - matched;
        let paragraph_ix = start + ix;
        let block_ix = document_block_ix_for_paragraph(document, paragraph_ix)?;
        let block_id = document
          .ids
          .block_ids
          .get(block_ix)
          .copied()
          .context("inserted paragraph span block is missing a block id")?;
        let paragraph_id = document
          .ids
          .paragraph_ids
          .get(paragraph_ix)
          .copied()
          .context("inserted paragraph span block is missing a paragraph id")?;
        self.insert_paragraph_row(
          insert_ix,
          block_id,
          paragraph_id,
          paragraph.style,
          &paragraph.runs,
          text,
          block_version(&document.blocks[block_ix]),
        )?;
        self.debug_assert_row_text_len(document, insert_ix);
      }
    }

    if before.paragraphs.len() > matched {
      let blocks = self.blocks();
      for row_ix in old_rows[matched..].iter().rev().copied() {
        blocks.delete(row_ix, 1)?;
        self
          .binding
          .remove_row(row_ix)
          .context("DocBinding paragraph span row disappeared during delete")?;
      }
    }

    self.binding.refresh_versions(document);
    self.binding.assert_consistent(self.doc, document);
    Ok(())
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

  fn update_matched_paragraph(
    &self,
    document: &Document,
    row_ix: usize,
    before: &Paragraph,
    before_text: &str,
    after: &Paragraph,
    after_text: &str,
  ) -> Result<()> {
    let row = self
      .binding
      .rows
      .get(row_ix)
      .context("matched paragraph row is out of bounds")?;
    let text = row
      .text
      .as_ref()
      .context("matched paragraph row is missing its LoroText")?;
    if before_text != after_text {
      text.update(after_text, UpdateOptions::default())?;
    }
    if before.runs != after.runs {
      refresh_marks(text, &after.runs)?;
    }
    if before.style != after.style {
      row.map.insert(STYLE, encode_paragraph_style(after.style))?;
    }
    self.debug_assert_row_text_len(document, row_ix);
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
    row.text = None;
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
    runs: &[TextRun],
    text: &str,
    version: u64,
  ) -> Result<()> {
    let blocks = self.blocks();
    let (map, loro_text) = insert_paragraph_container(&blocks, row_ix, style, runs, text)?;
    self.binding.insert_row(
      row_ix,
      BindingRow {
        map,
        text: Some(loro_text),
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

  fn delete_text_at_row(&self, row_ix: usize, start: usize, end: usize) -> Result<()> {
    let text = self.paragraph_text_at_row(row_ix)?;
    if end > start {
      text.delete_utf8(start, end - start)?;
    }
    Ok(())
  }

  fn paragraph_text_at_row(&self, row_ix: usize) -> Result<LoroText> {
    self
      .binding
      .rows
      .get(row_ix)
      .and_then(|row| row.text.clone())
      .context("paragraph row is missing its LoroText")
  }

  fn row_for_paragraph(&self, paragraph: ParagraphId) -> Result<&BindingRow> {
    let ix = self.row_ix_for_paragraph(paragraph)?;
    self
      .binding
      .rows
      .get(ix)
      .context("DocBinding paragraph index is out of bounds")
  }

  fn debug_assert_row_text_len(&self, document: &Document, row_ix: usize) {
    let Some(row) = self.binding.rows.get(row_ix) else {
      return;
    };
    let Some(text) = row.text.as_ref() else {
      return;
    };
    if let Some(paragraph_ix) = self.paragraph_ordinal_for_row(row_ix) {
      debug_assert_paragraph_text_len(text, document, paragraph_ix);
    }
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

  fn replace_span_start_ordinal(&self, start_paragraph: Option<ParagraphId>, before: &DocumentSpan) -> usize {
    start_paragraph
      .and_then(|paragraph| self.binding.by_paragraph.get(&paragraph).copied())
      .and_then(|row_ix| self.paragraph_ordinal_for_row(row_ix))
      .unwrap_or(before.start_paragraph)
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

  fn row_ix_for_paragraph_insert_ordinal(&self, target: usize) -> usize {
    let mut paragraph_ix = 0;
    for (row_ix, row) in self.binding.rows.iter().enumerate() {
      if row.paragraph_id.is_none() {
        continue;
      }
      if paragraph_ix == target {
        return row_ix;
      }
      paragraph_ix += 1;
    }
    self.binding.rows.len()
  }

  fn blocks(&self) -> LoroMovableList {
    self.doc.get_movable_list(BLOCKS)
  }
}

fn insert_delta_at(text: &LoroText, byte: usize, delta: &[TextDelta]) -> Result<()> {
  let mut cursor = byte;
  for item in delta {
    let TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    if insert.is_empty() {
      continue;
    }
    text.insert_utf8(cursor, insert)?;
    let styles = run_styles_from_attrs(attributes.as_ref());
    set_run_styles_utf8(text, cursor..cursor + insert.len(), styles)?;
    cursor += insert.len();
  }
  Ok(())
}

fn refresh_marks(text: &LoroText, runs: &[TextRun]) -> Result<()> {
  let len = text.len_utf8();
  if len == 0 {
    return Ok(());
  }
  for key in [MARK_SEMANTIC, MARK_UNDERLINE, MARK_STRIKE, MARK_HIGHLIGHT] {
    unmark_utf8(text, 0..len, key)?;
  }
  apply_mark_intervals(text, &mark_intervals_from_runs(runs))
}

fn span_paragraph_texts(span: &DocumentSpan) -> Result<Vec<String>> {
  let mut texts = Vec::with_capacity(span.paragraphs.len());
  let mut byte = 0;
  for (ix, paragraph) in span.paragraphs.iter().enumerate() {
    let end = byte + paragraph_text_len(paragraph);
    texts.push(
      span
        .text
        .get(byte..end)
        .context("DocumentSpan text is shorter than its paragraph runs")?
        .to_string(),
    );
    byte = end + usize::from(ix + 1 < span.paragraphs.len());
  }
  Ok(texts)
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

fn document_block_ix_for_paragraph(document: &Document, target_paragraph_ix: usize) -> Result<usize> {
  let mut paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(_) if paragraph_ix == target_paragraph_ix => return Ok(block_ix),
      Block::Paragraph(_) => paragraph_ix += 1,
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {},
    }
  }
  bail!("target paragraph index is out of bounds")
}

fn kind_for_payload(payload: &BlockPayload) -> &'static str {
  match payload {
    BlockPayload::Image { .. } => KIND_IMAGE,
    BlockPayload::Equation { .. } => KIND_EQUATION,
    BlockPayload::Table(_) => KIND_TABLE,
  }
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
