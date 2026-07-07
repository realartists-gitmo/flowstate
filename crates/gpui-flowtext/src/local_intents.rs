//! Typed local-write intents (spec §4, D2).
//!
//! An intent is the ONLY way editing enters the document. Each intent carries
//! stable identity (paragraph/block/row/column ids, optionally an encoded Loro
//! cursor) plus raw offsets strictly as *hints*. The write path resolves
//! identity against live Loro state inside the write gate before any mutation;
//! if identity cannot be resolved the intent is rejected before the doc is
//! touched (spec I-2, I-15). Raw projection offsets are never authority.
//!
//! One intent = one Loro commit = one undo-group member (spec §4). Intents are
//! deliberately plain data (no Loro types beyond encoded cursor bytes) so the
//! editor crate can construct them without depending on `loro`.

use std::ops::Range;

use crate::{
  BlockId, ColumnId, DocumentOffset, DocumentProjection, EditorSelection, InputBlock, InputBlockAlignment, InputImageSizing,
  InputParagraph, InputTableCell, InputTableColumnWidth, InputTableRow, ParagraphId, ParagraphStyle, ProjectionPatchBatch, RowId,
  RunStyles, SelectionAffinity, VisualGravity,
};

/// A position in body text addressed by stable identity.
///
/// Resolution law (spec §4): the encoded `cursor` (when present) is the
/// preferred basis and wins over `byte_hint`; otherwise the paragraph id
/// resolves via its durable paragraph record and `byte_hint` is clamped into
/// the resolved paragraph's current range. `byte_hint` is a UTF-8 byte offset
/// within the paragraph's text (projection space); it is never applied as a
/// raw document index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextAnchor {
  pub paragraph: ParagraphId,
  pub byte_hint: usize,
  /// Encoded `loro::cursor::Cursor` captured earlier (e.g. at composition
  /// start or selection time). Optional; identity resolution works without it.
  pub cursor: Option<Vec<u8>>,
}

impl TextAnchor {
  #[must_use]
  pub fn new(paragraph: ParagraphId, byte_hint: usize) -> Self {
    Self {
      paragraph,
      byte_hint,
      cursor: None,
    }
  }

  #[must_use]
  pub fn with_cursor(mut self, cursor: Vec<u8>) -> Self {
    self.cursor = Some(cursor);
    self
  }
}

/// Insert plain text at an anchored caret position.
///
/// Style inheritance is Loro's job (expand-`After` run marks, spec §9): a
/// plain insert emits NO style operations. `style_override` is present only
/// when the caret's style state differs from what inheritance would produce
/// (e.g. bold toggled off at the end of a bold run) and marks exactly the
/// inserted range.
#[derive(Clone, Debug)]
pub struct InsertTextIntent {
  pub at: TextAnchor,
  pub text: String,
  pub style_override: Option<RunStyles>,
}

/// Delete the anchored range. Marks collapse naturally; no style ops.
#[derive(Clone, Debug)]
pub struct DeleteRangeIntent {
  pub start: TextAnchor,
  pub end: TextAnchor,
}

/// Split the anchored paragraph at the anchor position. New paragraph/block
/// identities are minted by the write path (not the editor) and surface to the
/// UI through the returned projection patches.
///
/// Sentinel hygiene (spec §9): the split unmarks all run-style keys on the
/// inserted `\n` so expand-`After` marks never bleed across the paragraph
/// boundary; cross-split style continuation is the caret-style-override path.
#[derive(Clone, Debug)]
pub struct SplitParagraphIntent {
  pub at: TextAnchor,
  pub inherited_style: ParagraphStyle,
}

/// Join `second` into `first` (both by durable identity).
#[derive(Clone, Debug)]
pub struct JoinParagraphsIntent {
  pub first: ParagraphId,
  pub second: ParagraphId,
}

/// Apply run styles over an anchored range (minimal mark change over the
/// changed range only — never a run-list restatement, spec §9).
#[derive(Clone, Debug)]
pub struct SetMarksIntent {
  pub start: TextAnchor,
  pub end: TextAnchor,
  pub styles: RunStyles,
}

/// Set a paragraph's style mark (anchored on its boundary sentinel).
#[derive(Clone, Debug)]
pub struct SetParagraphStyleIntent {
  pub paragraph: ParagraphId,
  pub style: ParagraphStyle,
}

/// Insert an object block (image/equation/table/…) at an anchored position.
/// The block's durable identity is minted by the write path.
#[derive(Clone, Debug)]
pub struct InsertObjectIntent {
  pub at: TextAnchor,
  pub block: InputBlock,
}

/// Replace an object block's content in place (same identity).
#[derive(Clone, Debug)]
pub struct ReplaceObjectIntent {
  pub block: BlockId,
  pub after: InputBlock,
}

/// Delete object blocks by identity.
#[derive(Clone, Debug)]
pub struct DeleteBlocksIntent {
  pub blocks: Vec<BlockId>,
}

/// Move an object block so it sits immediately before `before` (or at the end
/// of the document when `None`). Identity-anchored — never a raw index.
#[derive(Clone, Debug)]
pub struct MoveBlockIntent {
  pub block: BlockId,
  pub before: Option<BlockId>,
}

/// One block of a rich pasted fragment.
#[derive(Clone, Debug)]
pub enum FragmentBlock {
  Paragraph(InputParagraph),
  Object(InputBlock),
}

/// Insert a rich multi-block fragment (paste) at an anchored position. This is
/// a compound intent: it still executes as one gate hold and one Loro commit,
/// and a mid-apply failure is compensated per spec I-10.
#[derive(Clone, Debug)]
pub struct InsertRichFragmentIntent {
  pub at: TextAnchor,
  pub blocks: Vec<FragmentBlock>,
}

/// Equation source edit (identity + intra-source byte range).
#[derive(Clone, Debug)]
pub struct ReplaceEquationSourceRangeIntent {
  pub equation: BlockId,
  pub range: Range<usize>,
  pub text: String,
}

/// Image metadata/layout intents (identity-based).
#[derive(Clone, Debug)]
pub struct ReplaceImageAltTextIntent {
  pub image: BlockId,
  pub text: String,
}

#[derive(Clone, Debug)]
pub struct ReplaceImageCaptionIntent {
  pub image: BlockId,
  pub caption: Option<InputParagraph>,
}

#[derive(Clone, Debug)]
pub struct SetImageLayoutIntent {
  pub image: BlockId,
  pub sizing: InputImageSizing,
  pub alignment: InputBlockAlignment,
}

/// Table intents — durable row/column/cell identities throughout (§P2b).
/// `InsertTableRow`/`InsertTableColumn` identities are minted by the write
/// path; `after_*` anchors falling back deterministically to the tail when the
/// anchor was concurrently deleted (existing convergent behavior).
#[derive(Clone, Debug)]
pub enum TableIntent {
  InsertRow {
    table: BlockId,
    after_row: Option<RowId>,
    row: InputTableRow,
  },
  DeleteRow {
    table: BlockId,
    row: RowId,
  },
  MoveRow {
    table: BlockId,
    row: RowId,
    after_row: Option<RowId>,
  },
  InsertColumn {
    table: BlockId,
    after_column: Option<ColumnId>,
    width: InputTableColumnWidth,
  },
  DeleteColumn {
    table: BlockId,
    column: ColumnId,
  },
  MoveColumn {
    table: BlockId,
    column: ColumnId,
    after_column: Option<ColumnId>,
  },
  ReplaceCell {
    table: BlockId,
    row: RowId,
    column: ColumnId,
    cell: InputTableCell,
  },
  SetCellSpan {
    table: BlockId,
    row: RowId,
    column: ColumnId,
    row_span: u16,
    column_span: u16,
  },
  SetColumnWidth {
    table: BlockId,
    column: ColumnId,
    width: InputTableColumnWidth,
  },
}

/// The complete intent vocabulary. Single dispatch point for the write path,
/// fuzzing, and counters; the per-intent structs above are the public API
/// surface behind [`LocalWriteAuthority`].
#[derive(Clone, Debug)]
pub enum LocalIntent {
  InsertText(InsertTextIntent),
  DeleteRange(DeleteRangeIntent),
  SplitParagraph(SplitParagraphIntent),
  JoinParagraphs(JoinParagraphsIntent),
  SetMarks(SetMarksIntent),
  SetParagraphStyle(SetParagraphStyleIntent),
  InsertObject(InsertObjectIntent),
  ReplaceObject(ReplaceObjectIntent),
  DeleteBlocks(DeleteBlocksIntent),
  MoveBlock(MoveBlockIntent),
  InsertRichFragment(InsertRichFragmentIntent),
  ReplaceEquationSourceRange(ReplaceEquationSourceRangeIntent),
  ReplaceImageAltText(ReplaceImageAltTextIntent),
  ReplaceImageCaption(ReplaceImageCaptionIntent),
  SetImageLayout(SetImageLayoutIntent),
  Table(TableIntent),
}

impl LocalIntent {
  /// Intent class label, stamped onto the Loro commit message for field
  /// forensics (spec §16, pre-commit attribution) and used by counters.
  #[must_use]
  pub fn class(&self) -> &'static str {
    match self {
      Self::InsertText(_) => "insert-text",
      Self::DeleteRange(_) => "delete-range",
      Self::SplitParagraph(_) => "split-paragraph",
      Self::JoinParagraphs(_) => "join-paragraphs",
      Self::SetMarks(_) => "set-marks",
      Self::SetParagraphStyle(_) => "set-paragraph-style",
      Self::InsertObject(_) => "insert-object",
      Self::ReplaceObject(_) => "replace-object",
      Self::DeleteBlocks(_) => "delete-blocks",
      Self::MoveBlock(_) => "move-block",
      Self::InsertRichFragment(_) => "insert-rich-fragment",
      Self::ReplaceEquationSourceRange(_) => "replace-equation-source",
      Self::ReplaceImageAltText(_) => "replace-image-alt-text",
      Self::ReplaceImageCaption(_) => "replace-image-caption",
      Self::SetImageLayout(_) => "set-image-layout",
      Self::Table(_) => "table-op",
    }
  }

  /// Compound intents may touch several containers; a mid-apply failure in one
  /// triggers the I-10 compensation path. Single-mutation intents cannot fail
  /// mid-apply after resolution succeeds.
  #[must_use]
  pub fn is_compound(&self) -> bool {
    matches!(self, Self::InsertRichFragment(_)) || matches!(self, Self::Table(TableIntent::InsertColumn { .. }))
  }
}

/// A selection endpoint in authoritative (cursor-backed) form plus its
/// render-space offset at capture time (spec §8, D5).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorEndpoint {
  /// Encoded `loro::cursor::Cursor` anchored per the §8 anchor-character rule.
  pub cursor: Vec<u8>,
  /// App-side interpretation delta: caret index = resolved anchor index +
  /// `delta` (0 = right-sticky "at anchor", 1 = left-sticky "after anchor").
  pub delta: u8,
  pub affinity: SelectionAffinity,
  pub gravity: VisualGravity,
  /// Render-space offset valid for the projection this endpoint was produced
  /// against. Never authority; recomputed on resolution.
  pub offset: DocumentOffset,
}

/// Authoritative selection: a cursor pair (spec I-12).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionSnapshot {
  pub anchor: CursorEndpoint,
  pub head: CursorEndpoint,
}

/// Why a local intent was rejected before mutation (spec I-15). The editor
/// restores selection from the nearest valid cursor, surfaces a recovery
/// notice, and never retries unchanged.
#[derive(Clone, Debug)]
pub enum WriteRejected {
  /// The paragraph identity no longer exists in canonical state.
  UnresolvedParagraph(ParagraphId),
  /// The block identity no longer exists in canonical state.
  UnresolvedBlock(BlockId),
  /// A table row/column/cell identity no longer exists.
  UnresolvedTableEntity { table: BlockId, detail: String },
  /// The supplied cursor could not be decoded or resolved.
  UnresolvedCursor,
  /// The intent is a no-op (empty text, empty range, empty fragment).
  EmptyIntent,
  /// The intent would violate document structure (e.g. delete the boundary-0
  /// sentinel, join across a non-adjacent pair).
  StructureViolation(&'static str),
  /// The write gate is poisoned: a panic occurred while the doc was held.
  /// Reload from persisted state (spec I-10d).
  GatePoisoned,
  /// A mid-apply failure occurred and was compensated via `revert_to`
  /// (spec I-10). The document converged back to its pre-intent state; the
  /// diagnostic carries the underlying error chain.
  CompensatedFailure { class: &'static str, diagnostic: String },
  /// Compensation itself failed. The core is wedged and must be reloaded; the
  /// caller must treat the document as read-only until then.
  CompensationFailed { class: &'static str, diagnostic: String },
}

impl std::fmt::Display for WriteRejected {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::UnresolvedParagraph(id) => write!(f, "paragraph identity {} did not resolve against canonical state", id.0),
      Self::UnresolvedBlock(id) => write!(f, "block identity {} did not resolve against canonical state", id.0),
      Self::UnresolvedTableEntity { table, detail } => {
        write!(f, "table {} entity did not resolve against canonical state: {detail}", table.0)
      },
      Self::UnresolvedCursor => f.write_str("supplied cursor could not be decoded/resolved against canonical state"),
      Self::EmptyIntent => f.write_str("intent is a no-op"),
      Self::StructureViolation(reason) => write!(f, "intent violates document structure: {reason}"),
      Self::GatePoisoned => f.write_str("write gate poisoned; reload from persisted state"),
      Self::CompensatedFailure { class, diagnostic } => {
        write!(f, "intent '{class}' failed mid-apply and was compensated back to pre-intent state: {diagnostic}")
      },
      Self::CompensationFailed { class, diagnostic } => {
        write!(f, "intent '{class}' failed mid-apply AND compensation failed — core must be reloaded: {diagnostic}")
      },
    }
  }
}

impl std::error::Error for WriteRejected {}

/// Always-on work counters for one committed intent (spec §11 — the
/// complexity-shape contract). Counts, not clocks: these assert O(touched)
/// behavior independent of hardware.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IntentCounters {
  /// Loro ops in the committed transaction (`ChangeMeta.len` via the
  /// pre-commit subscription; the source of truth, immune to drift).
  pub loro_ops: u64,
  /// Distinct Loro containers mutated by the intent (counted at the mutation
  /// call sites).
  pub containers_touched: u32,
  /// Style mark/unmark operations emitted.
  pub marks_emitted: u32,
  /// Projection patches synthesized.
  pub patch_count: u32,
  /// True when this intent fell back to a full projection rebuild — must be
  /// rare and loud (spec I-14); test failure unless the op class requires it.
  pub full_rebuild: bool,
  /// Gate hold time (microseconds) for the whole intent.
  pub gate_hold_micros: u64,
}

/// The synchronous result of a committed local intent (spec §4).
#[derive(Clone, Debug)]
pub struct LocalCommit {
  /// Exact patches for the UI read model, corresponding to this commit.
  pub patches: ProjectionPatchBatch,
  /// Encoded doc frontier after the commit.
  pub frontier: Vec<u8>,
  /// Encoded version vector after the commit.
  pub version_vector: Vec<u8>,
  /// Cursor-backed selection after the intent (caret placement authority).
  pub selection_after: Option<SelectionSnapshot>,
  pub counters: IntentCounters,
}

/// A full projection replacement produced when an intent legitimately requires
/// a rebuild (rare, loud) or by undo/redo/revision operations.
#[derive(Clone, Debug)]
pub struct ProjectionReplace {
  pub document: DocumentProjection,
  pub frontier: Vec<u8>,
  pub version_vector: Vec<u8>,
}

/// Outcome of an intent that may either patch or (rarely) replace the
/// projection.
#[derive(Clone, Debug)]
pub enum LocalWriteOutcome {
  Committed(LocalCommit),
  /// Committed, but the projection change could not be expressed as patches;
  /// the full document replaces the UI copy. Counted via
  /// `IntentCounters::full_rebuild` and the `full-rebuild-after-local-write`
  /// metric.
  CommittedWithRebuild {
    commit: LocalCommit,
    replace: Box<ProjectionReplace>,
  },
}

impl LocalWriteOutcome {
  #[must_use]
  pub fn commit(&self) -> &LocalCommit {
    match self {
      Self::Committed(commit) => commit,
      Self::CommittedWithRebuild { commit, .. } => commit,
    }
  }
}

/// Outcome of an undo/redo executed through the collaboration runtime's
/// Loro `UndoManager` (spec §10).
#[derive(Debug)]
pub struct UndoOutcome {
  /// `None` when the undo stack was empty (nothing applied).
  pub replace: Option<ProjectionReplace>,
  /// Cursor-restored selection recorded in the undo item's metadata.
  pub selection: Option<EditorSelection>,
}

/// One item of the ORDERED projection stream (field fix 2026-07-07): every
/// editor-bound projection change — local intent patches, remote import
/// patches, full replaces from undo/rebuild — is queued on the document core
/// IN COMMIT ORDER under the write gate, and the editor drains it as its sole
/// projection input. Splitting delivery across a synchronous local channel and
/// an asynchronous session channel is what produced the base-frontier
/// mismatch cascade in the field logs; a single ordered stream makes it
/// structurally impossible.
#[derive(Clone, Debug)]
pub enum ProjectionStreamItem {
  Patches(ProjectionPatchBatch),
  Replace(Box<DocumentProjection>),
}

/// The ONE local write path (Loro-first spec §4, invariant 5), as seen from
/// the editor. The application injects an implementation backed by the
/// gate-protected document core (`LocalDocHandle`); solo and collaborative
/// documents receive the identical authority. Every call is synchronous: the
/// returned commit IS the committed state, and the patches are exact.
pub trait LocalWriteAuthority: Send + Sync {
  fn apply(&self, intent: LocalIntent) -> Result<LocalWriteOutcome, WriteRejected>;
  fn undo(&self) -> Result<UndoOutcome, WriteRejected>;
  fn redo(&self) -> Result<UndoOutcome, WriteRejected>;
  /// Begin an undo group at an input-semantic boundary. Fallible by design:
  /// a non-disjoint remote import closes the active group underneath the
  /// editor, and the editor re-arms at the next boundary.
  fn undo_group_start(&self) -> Result<bool, WriteRejected>;
  fn undo_group_end(&self) -> Result<(), WriteRejected>;
  /// Drain the ordered projection stream (everything queued since the last
  /// drain, in commit order). The editor is the single consumer.
  fn drain_projection_stream(&self) -> Result<Vec<ProjectionStreamItem>, WriteRejected>;
  /// Clone the canonical projection (attach + self-heal fallback).
  fn canonical_projection(&self) -> Result<DocumentProjection, WriteRejected>;
}
