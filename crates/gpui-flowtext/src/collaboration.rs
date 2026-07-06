use std::ops::Range;

use super::{
  AssetId, AssetRecord, BlockId, DocumentOffset, DocumentProjection, DocumentSpan, EditorSelection, InputBlock, InputBlockAlignment,
  InputImageSizing, InputParagraph, InputTableCell, InputTableColumnWidth, InputTableRow, ParagraphId, ParagraphStyle, RunStyles,
  SelectionAffinity, TextRun, VisualGravity, paragraph_text, paragraph_text_len,
};
use rustc_hash::FxHashMap;

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
    source_paragraph: ParagraphId,
    source_block: BlockId,
    new_paragraph: ParagraphId,
    new_block: BlockId,
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
pub struct SemanticCommandBatch {
  pub transaction_id: u128,
  pub selection_movement_epoch: u64,
  pub base_frontier: Vec<u8>,
  pub semantic_commands: Vec<SemanticEditCommand>,
  pub selection_after: Option<EditorSelection>,
  pub stable_selection_after: Option<StableEditorSelection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableSelectionEndpoint {
  paragraph_id: ParagraphId,
  paragraph_hint: usize,
  previous_paragraph: Option<(ParagraphId, usize)>,
  next_paragraph: Option<ParagraphId>,
  byte: usize,
  affinity: SelectionAffinity,
  gravity: VisualGravity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableEditorSelection {
  anchor: StableSelectionEndpoint,
  head: StableSelectionEndpoint,
}

impl StableSelectionEndpoint {
  #[must_use]
  pub fn capture(document: &DocumentProjection, offset: DocumentOffset, affinity: SelectionAffinity, gravity: VisualGravity) -> Option<Self> {
    let paragraph_id = document.ids.paragraph_ids.get(offset.paragraph).copied()?;
    let previous_paragraph = offset.paragraph.checked_sub(1).and_then(|paragraph| {
      Some((
        *document.ids.paragraph_ids.get(paragraph)?,
        paragraph_text_len(document.paragraphs.get(paragraph)?),
      ))
    });
    let next_paragraph = document.ids.paragraph_ids.get(offset.paragraph + 1).copied();
    Some(Self {
      paragraph_id,
      paragraph_hint: offset.paragraph,
      previous_paragraph,
      next_paragraph,
      byte: offset.byte,
      affinity,
      gravity,
    })
  }

  #[must_use]
  pub fn resolve(&self, document: &DocumentProjection) -> DocumentOffset {
    if let Some(paragraph) = document
      .ids
      .paragraph_ids
      .iter()
      .position(|candidate| *candidate == self.paragraph_id)
    {
      return DocumentOffset {
        paragraph,
        byte: clamp_paragraph_byte_to_char_boundary(document, paragraph, self.byte),
      };
    }

    // Fidelity: the captured paragraph id is gone from the projection, so the
    // caret must fall back to a neighbor/hint. This lossy path is a prime
    // suspect for caret drift after a reconcile; the event is strictly additive
    // and does not change the resolution below.
    flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Identity, "stable-selection-fallback", || {
      format!(
        "paragraph_id={:?} hint={} byte={} affinity={:?} gravity={:?} prev={:?} next={:?} doc_paras={}",
        self.paragraph_id,
        self.paragraph_hint,
        self.byte,
        self.affinity,
        self.gravity,
        self.previous_paragraph.map(|(id, _)| id),
        self.next_paragraph,
        document.paragraphs.len(),
      )
    });

    let prefer_next = matches!(self.affinity, SelectionAffinity::After) || matches!(self.gravity, VisualGravity::Downstream);
    let previous = self.previous_paragraph.and_then(|(id, old_len)| {
      let paragraph = document.ids.paragraph_ids.iter().position(|candidate| *candidate == id)?;
      Some(DocumentOffset {
        paragraph,
        byte: clamp_paragraph_byte_to_char_boundary(document, paragraph, old_len.saturating_add(self.byte)),
      })
    });
    let next = self.next_paragraph.and_then(|id| {
      let paragraph = document.ids.paragraph_ids.iter().position(|candidate| *candidate == id)?;
      Some(DocumentOffset { paragraph, byte: 0 })
    });
    if prefer_next {
      if let Some(offset) = next.or(previous) {
        return offset;
      }
    } else if let Some(offset) = previous.or(next) {
      return offset;
    }

    let paragraph = self.paragraph_hint.min(document.paragraphs.len().saturating_sub(1));
    DocumentOffset {
      paragraph,
      byte: clamp_paragraph_byte_to_char_boundary(document, paragraph, self.byte),
    }
  }
}

impl StableEditorSelection {
  #[must_use]
  pub fn capture(document: &DocumentProjection, selection: &EditorSelection) -> Option<Self> {
    Some(Self {
      anchor: StableSelectionEndpoint::capture(document, selection.anchor, selection.anchor_affinity, selection.anchor_gravity)?,
      head: StableSelectionEndpoint::capture(document, selection.head, selection.head_affinity, selection.head_gravity)?,
    })
  }

  #[must_use]
  pub fn resolve(&self, document: &DocumentProjection) -> EditorSelection {
    EditorSelection {
      anchor: self.anchor.resolve(document),
      head: self.head.resolve(document),
      anchor_affinity: self.anchor.affinity,
      head_affinity: self.head.affinity,
      anchor_gravity: self.anchor.gravity,
      head_gravity: self.head.gravity,
    }
  }
}

fn clamp_paragraph_byte_to_char_boundary(document: &DocumentProjection, paragraph: usize, byte: usize) -> usize {
  let Some(text) = document.paragraphs.get(paragraph).map(|_| paragraph_text(document, paragraph)) else {
    return 0;
  };
  let mut byte = byte.min(text.len());
  while byte > 0 && !text.is_char_boundary(byte) {
    byte -= 1;
  }
  byte
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
pub struct ProjectionPatchBatch {
  /// Unique id for diagnostics and duplicate-delivery detection. It is not part
  /// of document semantics; the frontier pair is the authoritative ordering.
  pub transaction_id: u128,
  /// Frontier of the materialized document this delta must be applied to.
  pub base_frontier: Vec<u8>,
  /// Frontier represented after every patch has been applied successfully.
  pub new_frontier: Vec<u8>,
  pub patches: Vec<ProjectionPatch>,
}

impl ProjectionPatchBatch {
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.patches.is_empty()
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionApplyError {
  StaleFrontier { expected: Vec<u8>, actual: Vec<u8> },
  MissingBlock { block_id: BlockId, row_hint: usize },
  WrongBlockKind { block_id: BlockId, expected: &'static str },
  MissingParagraph { paragraph_id: ParagraphId, block_id: BlockId },
  DuplicateBlockId(BlockId),
  DuplicateParagraphId(ParagraphId),
  InvalidAnchor(BlockId),
  InvalidStructuralPatch(&'static str),
  UnexpectedTransaction { expected: Option<u128>, actual: u128 },
}

impl std::fmt::Display for ProjectionApplyError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::StaleFrontier { expected, actual } => write!(
        f,
        "materialized document frontier mismatch (expected {} bytes, actual {} bytes)",
        expected.len(),
        actual.len()
      ),
      Self::MissingBlock { block_id, row_hint } => write!(f, "projection patch target block {} is missing (row hint {row_hint})", block_id.0),
      Self::WrongBlockKind { block_id, expected } => write!(f, "projection patch target block {} is not {expected}", block_id.0),
      Self::MissingParagraph { paragraph_id, block_id } => {
        write!(f, "projection paragraph {} is not attached to block {}", paragraph_id.0, block_id.0)
      },
      Self::DuplicateBlockId(id) => write!(f, "projection patch would create duplicate block id {}", id.0),
      Self::DuplicateParagraphId(id) => write!(f, "projection patch would create duplicate paragraph id {}", id.0),
      Self::InvalidAnchor(id) => write!(f, "projection insertion anchor block {} is missing", id.0),
      Self::InvalidStructuralPatch(reason) => f.write_str(reason),
      Self::UnexpectedTransaction { expected, actual } => {
        write!(f, "runtime transaction acknowledgement mismatch (expected {expected:?}, actual {actual})")
      },
    }
  }
}

impl std::error::Error for ProjectionApplyError {}

#[derive(Clone, Debug)]
pub enum ProjectionPatch {
  ParagraphText {
    block_id: BlockId,
    paragraph_id: ParagraphId,
    row_hint: usize,
    new: InputParagraph,
    delta_utf8: Vec<ProjectionTextDelta>,
  },
  ParagraphStyle {
    block_id: BlockId,
    paragraph_id: ParagraphId,
    row_hint: usize,
    style: ParagraphStyle,
  },
  ParagraphRuns {
    block_id: BlockId,
    paragraph_id: ParagraphId,
    row_hint: usize,
    runs: Vec<TextRun>,
  },
  ReplaceObjectBlock {
    block_id: BlockId,
    row_hint: usize,
    block: ProjectionStructuralBlock,
  },
  InsertBlocks {
    /// Insert immediately before this stable block id, or append when `None`.
    before: Option<BlockId>,
    row_hint: usize,
    blocks: Vec<ProjectionStructuralBlock>,
  },
  DeleteBlocks {
    block_ids: Vec<BlockId>,
    row_hint: usize,
  },
  MoveBlock {
    block_id: BlockId,
    /// Place the moved block immediately before this id, or at the end when
    /// `None`. The anchor is interpreted after removing `block_id`.
    before: Option<BlockId>,
    from_hint: usize,
    to_hint: usize,
  },
  AssetArrived {
    id: AssetId,
    record: AssetRecord,
  },
}
