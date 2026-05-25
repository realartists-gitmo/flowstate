use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ParagraphId(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct BlockId(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TableCellId(pub u128);

#[derive(Clone, Debug, Default)]
pub(super) struct DocumentIdentityMap {
  paragraph_ids: Vec<ParagraphId>,
  block_ids: Vec<BlockId>,
  table_cell_ids: Vec<Vec<Vec<TableCellId>>>,
}

impl DocumentIdentityMap {
  pub(super) fn new(document: &Document) -> Self {
    let mut this = Self::default();
    this.reconcile(document);
    this
  }

  pub(super) fn reconcile(&mut self, document: &Document) {
    resize_ids(&mut self.paragraph_ids, document.paragraphs.len(), ParagraphId);
    resize_ids(&mut self.block_ids, document.blocks.len(), BlockId);
    self
      .table_cell_ids
      .resize_with(document.blocks.len(), Vec::new);
    self.table_cell_ids.truncate(document.blocks.len());
    for (block_ix, block) in document.blocks.iter().enumerate() {
      let Block::Table(table) = block else {
        self.table_cell_ids[block_ix].clear();
        continue;
      };
      let rows = &mut self.table_cell_ids[block_ix];
      rows.resize_with(table.rows.len(), Vec::new);
      rows.truncate(table.rows.len());
      for (row_ix, row) in table.rows.iter().enumerate() {
        resize_ids(&mut rows[row_ix], row.cells.len(), TableCellId);
      }
    }
  }

  pub(super) fn insert_split_paragraph(&mut self, paragraph_ix: usize, block_ix: usize) {
    self
      .paragraph_ids
      .insert((paragraph_ix + 1).min(self.paragraph_ids.len()), ParagraphId(uuid::Uuid::new_v4().as_u128()));
    let block_insert_ix = (block_ix + 1).min(self.block_ids.len());
    self
      .block_ids
      .insert(block_insert_ix, BlockId(uuid::Uuid::new_v4().as_u128()));
    self.table_cell_ids.insert(block_insert_ix, Vec::new());
  }

  pub(super) fn paragraph_id(&self, paragraph_ix: usize) -> Option<ParagraphId> {
    self.paragraph_ids.get(paragraph_ix).copied()
  }

  pub(super) fn block_id(&self, block_ix: usize) -> Option<BlockId> {
    self.block_ids.get(block_ix).copied()
  }

  pub(super) fn table_cell_id(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> Option<TableCellId> {
    self
      .table_cell_ids
      .get(block_ix)?
      .get(row_ix)?
      .get(cell_ix)
      .copied()
  }

  pub(super) fn paragraph_index(&self, id: ParagraphId) -> Option<usize> {
    self
      .paragraph_ids
      .iter()
      .position(|candidate| *candidate == id)
  }
}

fn resize_ids<T>(ids: &mut Vec<T>, len: usize, wrap: impl Fn(u128) -> T)
where
  T: std::marker::Copy,
{
  while ids.len() < len {
    ids.push(wrap(uuid::Uuid::new_v4().as_u128()));
  }
  ids.truncate(len);
}

#[derive(Clone, Debug)]
pub enum CanonicalOperation {
  InsertText {
    paragraph: ParagraphId,
    byte: usize,
    text: String,
    styles: RunStyles,
  },
  DeleteRange {
    start_paragraph: ParagraphId,
    start_byte: usize,
    end_paragraph: ParagraphId,
    end_byte: usize,
  },
  SplitParagraph {
    paragraph: ParagraphId,
    byte: usize,
    new_paragraph: ParagraphId,
  },
  JoinParagraphs {
    first: ParagraphId,
    second: ParagraphId,
  },
  SetParagraphStyle {
    paragraph: ParagraphId,
    style: ParagraphStyle,
  },
  SetRunStyles {
    paragraph: ParagraphId,
    range: Range<usize>,
    styles: RunStyles,
  },
  InsertBlock {
    block: BlockId,
    block_ix: usize,
  },
  DeleteBlock {
    block: BlockId,
  },
  MoveBlock {
    block: BlockId,
    new_block_ix: usize,
  },
  ReplaceParagraphSpan {
    start_paragraph: Option<ParagraphId>,
    before: DocumentSpan,
    after: DocumentSpan,
  },
  ReplaceBlock {
    block: Option<BlockId>,
  },
  ReplaceDocument,
}

#[derive(Clone, Debug, Default)]
pub struct CollaborationEdit {
  pub operations: Vec<CanonicalOperation>,
}
