use std::ops::Range;

use super::{
  AssetId, AssetRecord, BlockId, DocumentOffset, DocumentProjection, DocumentSpan, EditorSelection, InputBlock, InputBlockAlignment,
  InputImageSizing, InputParagraph, InputTableCell, InputTableColumnWidth, InputTableRow, ParagraphId, ParagraphStyle, RunStyles, TextRun,
};
use rustc_hash::FxHashMap;

const OBJECT_REPLACEMENT: char = '\u{FFFC}';

#[derive(Clone, Debug, Default)]
pub struct DocumentIdentityMap {
  paragraph_ids: Vec<ParagraphId>,
  block_ids: Vec<BlockId>,
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
  pub fn paragraph_index(&self, id: ParagraphId) -> Option<usize> {
    self.paragraph_index_by_id.get(&id).copied()
  }
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

impl SemanticEditCommand {
  /// Whether the editor's optimistic projection is already the exact visible
  /// result of this command and can be acknowledged without replaying the
  /// runtime's projection echo.
  #[must_use]
  pub fn can_acknowledge_without_projection_replay(&self) -> bool {
    match self {
      Self::InsertText { text, .. } => !text.contains('\n') && !text.contains(OBJECT_REPLACEMENT),
      Self::DeleteRange { range } => range.start.paragraph == range.end.paragraph,
      Self::SetParagraphStyle { .. } | Self::SetRunStyles { .. } => true,
      _ => false,
    }
  }
}

#[derive(Clone, Debug, Default)]
pub struct SemanticCommandBatch {
  pub base_frontier: Vec<u8>,
  pub semantic_commands: Vec<SemanticEditCommand>,
  pub selection_after: Option<EditorSelection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionTextDelta {
  Retain(usize),
  Insert(usize),
  Delete(usize),
}

#[derive(Clone, Debug)]
pub struct ProjectionStructuralBlock {
  pub block_id: BlockId,
  pub paragraph_id: Option<ParagraphId>,
  pub block: InputBlock,
}

#[derive(Clone, Debug)]
pub enum ProjectionPatch {
  ParagraphText {
    row: usize,
    new: InputParagraph,
    delta_utf8: Vec<ProjectionTextDelta>,
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
    block: ProjectionStructuralBlock,
  },
  InsertBlocks {
    row: usize,
    blocks: Vec<ProjectionStructuralBlock>,
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
