use std::ops::Range;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::{
  AssetId, AssetRecord, Block, BlockId, DocumentProjection, DocumentOffset, DocumentSpan, InputBlock, InputBlockAlignment, InputImageSizing,
  EditorSelection, InputParagraph, InputTableCell, InputTableColumnWidth, InputTableRow, ParagraphId, ParagraphStyle, RunStyles, TextRun,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TableCellId(pub u128);

#[derive(Clone, Debug, Default)]
pub struct DocumentIdentityMap {
  paragraph_ids: Vec<ParagraphId>,
  block_ids: Vec<BlockId>,
  table_cell_ids: Vec<Vec<Vec<TableCellId>>>,
  paragraph_index_by_id: FxHashMap<ParagraphId, usize>,
}

#[hotpath::measure_all]
impl DocumentIdentityMap {
  #[must_use]
  pub fn new(document: &DocumentProjection) -> Self {
    let mut this = Self::default();
    this.reconcile(document);
    this
  }

  pub fn reconcile(&mut self, document: &DocumentProjection) {
    self.paragraph_ids.clone_from(&document.ids.paragraph_ids);
    self.paragraph_index_by_id.clear();
    for (ix, id) in self.paragraph_ids.iter().enumerate() {
      self.paragraph_index_by_id.insert(*id, ix);
    }
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
    self.paragraph_index_by_id.get(&id).copied()
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
pub enum SemanticEditCommand {
  InsertText {
    at: DocumentOffset,
    text: String,
    styles: RunStyles,
  },
  DeleteRange {
    range: Range<DocumentOffset>,
  },
  SplitParagraph {
    at: DocumentOffset,
    inherited_style: ParagraphStyle,
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
    after: InputBlock,
  },
  DeleteBlock {
    block: BlockId,
  },
  MoveBlock {
    block: BlockId,
    new_block_ix: usize,
  },
  ReplaceParagraphSpan {
    start: Option<DocumentOffset>,
    before: DocumentSpan,
    after: DocumentSpan,
  },
  ReplaceBlock {
    block: Option<BlockId>,
    block_ix: usize,
    after: InputBlock,
  },
  InsertTableRow {
    table: BlockId,
    row_ix: usize,
    row: InputTableRow,
  },
  DeleteTableRow {
    table: BlockId,
    row_ix: usize,
  },
  MoveTableRow {
    table: BlockId,
    from_row_ix: usize,
    to_row_ix: usize,
  },
  InsertTableColumn {
    table: BlockId,
    column_ix: usize,
    width: InputTableColumnWidth,
    cells: Vec<InputTableCell>,
  },
  DeleteTableColumn {
    table: BlockId,
    column_ix: usize,
  },
  MoveTableColumn {
    table: BlockId,
    from_column_ix: usize,
    to_column_ix: usize,
  },
  ReplaceTableCell {
    table: BlockId,
    row_ix: usize,
    cell_ix: usize,
    cell: InputTableCell,
  },
  SetTableCellSpan {
    table: BlockId,
    row_ix: usize,
    cell_ix: usize,
    row_span: u16,
    column_span: u16,
  },
  ReplaceEquationSourceRange {
    equation: BlockId,
    range: Range<usize>,
    text: String,
  },
  ReplaceImageAltText {
    image: BlockId,
    text: String,
  },
  ReplaceImageCaption {
    image: BlockId,
    caption: Option<InputParagraph>,
  },
  SetImageLayout {
    image: BlockId,
    sizing: InputImageSizing,
    alignment: InputBlockAlignment,
  },
  SetTableColumnWidth {
    table: BlockId,
    column_ix: usize,
    width: InputTableColumnWidth,
  },
}

#[derive(Clone, Debug, Default)]
pub struct CollaborationEdit {
  pub semantic_commands: Vec<SemanticEditCommand>,
  pub selection_after: Option<EditorSelection>,
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
  ParagraphRuns {
    row: usize,
    runs: Vec<TextRun>,
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
