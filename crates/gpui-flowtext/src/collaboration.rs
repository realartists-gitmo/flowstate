use super::{
  AssetId, AssetRecord, BlockId, DocumentProjection, InputBlock, InputParagraph, ParagraphId, ParagraphStyle, TextRun,
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

// Loro-first (spec invariant 6): the raw projection-space command surface is
// DELETED — not deprecated, not quarantined. Local editing enters exclusively
// as typed intents (`crate::local_intents`) resolved against live Loro state;
// wrong-position operations are unrepresentable. The CI guard
// (tools/check_raw_authority.sh) rejects any reintroduction by name.

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
