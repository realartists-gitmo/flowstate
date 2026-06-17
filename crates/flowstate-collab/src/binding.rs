//! Local flowtext identity bindings for shared CRDT containers.

use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use gpui_flowtext::{Block, BlockId, Document, ParagraphId, paragraph_text_len};
use loro::{Container, ContainerID, ContainerTrait as _, LoroDoc, LoroMap, LoroMovableList, ValueOrContainer};

use crate::{
  body_index::{BodyParagraphIndex, paragraph_lens_from_body_text},
  schema::{BLOCKS, body_text},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
  Paragraph,
  Image,
  Equation,
  Table,
}

#[derive(Clone, Debug)]
pub struct BindingRow {
  pub map: LoroMap,
  pub kind: BlockKind,
  pub block_id: BlockId,
  pub paragraph_id: Option<ParagraphId>,
  pub version: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DocBinding {
  pub rows: Vec<BindingRow>,
  pub by_paragraph: HashMap<ParagraphId, usize>,
  pub by_block: HashMap<BlockId, usize>,
  pub by_container: HashMap<ContainerID, usize>,
  /// Paragraph ordinal -> body byte-range index, kept in sync with the body text
  /// so paragraph offset lookups never stringify the whole body.
  pub body_index: BodyParagraphIndex,
}

impl DocBinding {
  pub fn build(doc: &LoroDoc, document: &Document) -> Result<Self> {
    let blocks = doc.get_movable_list(BLOCKS);
    if blocks.len() != document.blocks.len() {
      bail!(
        "binding row count mismatch: loro has {} rows, document has {} blocks",
        blocks.len(),
        document.blocks.len()
      );
    }

    let mut binding = Self::default();
    let mut paragraph_ix = 0;
    for (row_ix, block) in document.blocks.iter().enumerate() {
      let map = map_at(&blocks, row_ix)?;
      let kind = BlockKind::from_block(block);
      let paragraph_id = if matches!(kind, BlockKind::Paragraph) {
        let id = document
          .ids
          .paragraph_ids
          .get(paragraph_ix)
          .copied()
          .context("document is missing a paragraph id for a paragraph block")?;
        paragraph_ix += 1;
        Some(id)
      } else {
        None
      };
      let block_id = document
        .ids
        .block_ids
        .get(row_ix)
        .copied()
        .context("document is missing a block id for a block row")?;
      binding.push_row(BindingRow {
        map,
        kind,
        block_id,
        paragraph_id,
        version: block_version(block),
      });
    }
    binding.refresh_body_index(document);
    binding.assert_consistent(doc, document);
    Ok(binding)
  }

  /// Rebuilds the paragraph byte-offset index from the document's paragraphs.
  /// Used on build and on the O(n) structural paths (block insert/delete/move)
  /// that already rewrite the whole body; the per-keystroke paths update the
  /// index incrementally instead.
  pub fn refresh_body_index(&mut self, document: &Document) {
    self.body_index = BodyParagraphIndex::from_lens(document.paragraphs.iter().map(paragraph_text_len).collect());
  }

  /// Rebuilds the paragraph byte-offset index from the live Loro body text.
  /// Used by the remote applier after a full body reprojection so the index
  /// keeps matching the freshly imported body (O(body length)).
  pub fn refresh_body_index_from_loro(&mut self, doc: &LoroDoc) {
    self.body_index = BodyParagraphIndex::from_lens(paragraph_lens_from_body_text(&body_text(doc)));
  }

  pub fn assert_consistent(&self, doc: &LoroDoc, document: &Document) {
    let blocks = doc.get_movable_list(BLOCKS);
    debug_assert_eq!(self.rows.len(), blocks.len());
    debug_assert_eq!(self.rows.len(), document.blocks.len());

    let mut paragraph_ix = 0;
    for (row_ix, row) in self.rows.iter().enumerate() {
      let Some(block) = document.blocks.get(row_ix) else {
        debug_assert!(false, "binding row has no matching document block");
        continue;
      };
      debug_assert_eq!(
        row.kind,
        BlockKind::from_block(block),
        "binding row {row_ix} kind does not match document block"
      );
      debug_assert_eq!(
        document.ids.block_ids.get(row_ix).copied(),
        Some(row.block_id),
        "binding row {row_ix} block id does not match document block id"
      );
      debug_assert_eq!(row.version, block_version(block));
      if matches!(row.kind, BlockKind::Paragraph) {
        debug_assert_eq!(document.ids.paragraph_ids.get(paragraph_ix).copied(), row.paragraph_id);
        paragraph_ix += 1;
      } else {
        debug_assert!(row.paragraph_id.is_none());
      }
    }

    debug_assert_eq!(
      self.body_index.len(),
      paragraph_ix,
      "body index paragraph count does not match paragraph rows"
    );
    #[cfg(debug_assertions)]
    {
      let body_lens = paragraph_lens_from_body_text(&body_text(doc));
      debug_assert_eq!(
        self.body_index.len(),
        body_lens.len(),
        "body index paragraph count does not match the body text"
      );
      for (ordinal, expected) in body_lens.iter().enumerate() {
        debug_assert_eq!(
          self.body_index.paragraph_len(ordinal),
          *expected,
          "body index byte length mismatch vs body at paragraph {ordinal}"
        );
        debug_assert_eq!(
          self.body_index.paragraph_len(ordinal),
          document
            .paragraphs
            .get(ordinal)
            .map_or(0, paragraph_text_len),
          "body index byte length mismatch vs document at paragraph {ordinal}"
        );
      }
    }
  }

  pub fn push_row(&mut self, row: BindingRow) {
    let ix = self.rows.len();
    self.index_row(ix, &row);
    self.rows.push(row);
  }

  pub fn insert_row(&mut self, ix: usize, row: BindingRow) {
    let ix = ix.min(self.rows.len());
    self.rows.insert(ix, row);
    self.rebuild_indexes();
  }

  pub fn remove_row(&mut self, ix: usize) -> Option<BindingRow> {
    if ix >= self.rows.len() {
      return None;
    }
    let row = self.rows.remove(ix);
    self.rebuild_indexes();
    Some(row)
  }

  pub fn move_row(&mut self, from: usize, to: usize) {
    if from >= self.rows.len() || from == to {
      return;
    }
    let row = self.rows.remove(from);
    self.rows.insert(to.min(self.rows.len()), row);
    self.rebuild_indexes();
  }

  pub fn refresh_versions(&mut self, document: &Document) {
    for (row, block) in self.rows.iter_mut().zip(document.blocks.iter()) {
      row.version = block_version(block);
    }
  }

  pub fn rebuild_indexes(&mut self) {
    let mut by_paragraph = HashMap::new();
    let mut by_block = HashMap::new();
    let mut by_container = HashMap::new();
    for (ix, row) in self.rows.iter().enumerate() {
      by_block.insert(row.block_id, ix);
      by_container.insert(row.map.id(), ix);
      if let Some(paragraph_id) = row.paragraph_id {
        by_paragraph.insert(paragraph_id, ix);
      }
    }
    self.by_paragraph = by_paragraph;
    self.by_block = by_block;
    self.by_container = by_container;
  }

  fn index_row(&mut self, ix: usize, row: &BindingRow) {
    self.by_block.insert(row.block_id, ix);
    self.by_container.insert(row.map.id(), ix);
    if let Some(paragraph_id) = row.paragraph_id {
      self.by_paragraph.insert(paragraph_id, ix);
    }
  }
}

#[must_use]
pub const fn block_version(block: &Block) -> u64 {
  match block {
    Block::Paragraph(paragraph) => paragraph.version,
    Block::Image(image) => image.version,
    Block::Equation(equation) => equation.version,
    Block::Table(table) => table.version,
  }
}

impl BlockKind {
  #[must_use]
  pub const fn from_block(block: &Block) -> Self {
    match block {
      Block::Paragraph(_) => Self::Paragraph,
      Block::Image(_) => Self::Image,
      Block::Equation(_) => Self::Equation,
      Block::Table(_) => Self::Table,
    }
  }
}

fn map_at(blocks: &LoroMovableList, ix: usize) -> Result<LoroMap> {
  match blocks.get(ix) {
    Some(ValueOrContainer::Container(Container::Map(map))) => Ok(map),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => {
      bail!("binding row {ix} is not a map container")
    },
  }
}
