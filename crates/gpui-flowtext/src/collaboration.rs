use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::{
  AssetId, AssetRecord, Block, BlockId, Document, DocumentSpan, InputBlock, InputParagraph, ParagraphId, ParagraphStyle, RunStyles,
  new_block_id, new_paragraph_id,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TableCellId(pub u128);

#[derive(Clone, Debug, Default)]
pub struct DocumentIdentityMap {
  paragraph_ids: Vec<ParagraphId>,
  block_ids: Vec<BlockId>,
  table_cell_ids: Vec<Vec<Vec<TableCellId>>>,
}

#[hotpath::measure_all]
impl DocumentIdentityMap {
  #[must_use]
  pub fn new(document: &Document) -> Self {
    let mut this = Self::default();
    this.reconcile(document);
    this
  }

  pub fn reconcile(&mut self, document: &Document) {
    self.paragraph_ids.clone_from(&document.ids.paragraph_ids);
    self.block_ids.clone_from(&document.ids.block_ids);
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

  pub fn insert_split_paragraph(&mut self, paragraph_ix: usize, block_ix: usize) {
    self
      .paragraph_ids
      .insert((paragraph_ix + 1).min(self.paragraph_ids.len()), new_paragraph_id());
    let block_insert_ix = (block_ix + 1).min(self.block_ids.len());
    self.block_ids.insert(block_insert_ix, new_block_id());
    self.table_cell_ids.insert(block_insert_ix, Vec::new());
  }

  #[must_use]
  pub fn paragraph_id(&self, paragraph_ix: usize) -> Option<ParagraphId> {
    self.paragraph_ids.get(paragraph_ix).copied()
  }

  #[must_use]
  pub fn block_id(&self, block_ix: usize) -> Option<BlockId> {
    self.block_ids.get(block_ix).copied()
  }

  #[must_use]
  pub fn table_cell_id(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> Option<TableCellId> {
    self
      .table_cell_ids
      .get(block_ix)?
      .get(row_ix)?
      .get(cell_ix)
      .copied()
  }

  #[must_use]
  pub fn paragraph_index(&self, id: ParagraphId) -> Option<usize> {
    self
      .paragraph_ids
      .iter()
      .position(|candidate| *candidate == id)
  }
}

#[hotpath::measure]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
enum WireCanonicalOperation {
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
}

impl WireCanonicalOperation {
  fn from_canonical(operation: &CanonicalOperation) -> Option<Self> {
    match operation {
      CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => Some(Self::InsertText {
        paragraph: *paragraph,
        byte: *byte,
        text: text.clone(),
        styles: *styles,
      }),
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => Some(Self::DeleteRange {
        start_paragraph: *start_paragraph,
        start_byte: *start_byte,
        end_paragraph: *end_paragraph,
        end_byte: *end_byte,
      }),
      CanonicalOperation::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      } => Some(Self::SplitParagraph {
        paragraph: *paragraph,
        byte: *byte,
        new_paragraph: *new_paragraph,
      }),
      CanonicalOperation::JoinParagraphs { first, second } => Some(Self::JoinParagraphs {
        first: *first,
        second: *second,
      }),
      CanonicalOperation::SetParagraphStyle { paragraph, style } => Some(Self::SetParagraphStyle {
        paragraph: *paragraph,
        style: *style,
      }),
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => Some(Self::SetRunStyles {
        paragraph: *paragraph,
        range: range.clone(),
        styles: *styles,
      }),
      CanonicalOperation::InsertBlock { .. }
      | CanonicalOperation::DeleteBlock { .. }
      | CanonicalOperation::MoveBlock { .. }
      | CanonicalOperation::ReplaceParagraphSpan { .. }
      | CanonicalOperation::ReplaceBlock { .. }
      | CanonicalOperation::ReplaceDocument => None,
    }
  }

  fn into_canonical(self) -> CanonicalOperation {
    match self {
      Self::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      },
      Self::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      },
      Self::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      } => CanonicalOperation::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      },
      Self::JoinParagraphs { first, second } => CanonicalOperation::JoinParagraphs { first, second },
      Self::SetParagraphStyle { paragraph, style } => CanonicalOperation::SetParagraphStyle { paragraph, style },
      Self::SetRunStyles { paragraph, range, styles } => CanonicalOperation::SetRunStyles { paragraph, range, styles },
    }
  }
}

pub fn encode_canonical_operations(operations: &[CanonicalOperation]) -> Option<Vec<u8>> {
  let wire_operations: Vec<_> = operations
    .iter()
    .filter_map(WireCanonicalOperation::from_canonical)
    .collect();
  if wire_operations.is_empty() && !operations.is_empty() {
    return None;
  }
  postcard::to_stdvec(&wire_operations).ok()
}

pub fn decode_canonical_operations(bytes: &[u8]) -> Option<Vec<CanonicalOperation>> {
  postcard::from_bytes::<Vec<WireCanonicalOperation>>(bytes)
    .ok()
    .map(|operations| {
      operations
        .into_iter()
        .map(WireCanonicalOperation::into_canonical)
        .collect()
    })
}

#[derive(Clone, Debug, Default)]
pub struct CollaborationEdit {
  pub operations: Vec<CanonicalOperation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollabTextDelta {
  Retain(usize),
  Insert(usize),
  Delete(usize),
}

#[derive(Clone, Debug)]
pub struct CollabStructuralBlock {
  pub block_id: BlockId,
  pub paragraph_id: Option<ParagraphId>,
  pub block: InputBlock,
}

#[derive(Clone, Debug)]
pub enum CollabPatch {
  ParagraphText {
    row: usize,
    new: InputParagraph,
    delta_utf8: Vec<CollabTextDelta>,
  },
  ParagraphStyle {
    row: usize,
    style: ParagraphStyle,
  },
  ReplaceObjectBlock {
    row: usize,
    block: CollabStructuralBlock,
  },
  InsertBlocks {
    row: usize,
    blocks: Vec<CollabStructuralBlock>,
  },
  DeleteBlocks {
    row: usize,
    count: usize,
  },
  MoveBlock {
    from: usize,
    to: usize,
  },
  AssetArrived {
    id: AssetId,
    record: AssetRecord,
  },
}
