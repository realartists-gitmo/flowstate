use std::{
  collections::BTreeMap,
  io,
  path::{Path, PathBuf},
  sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering as AtomicOrdering},
  },
};

use anyhow::{Context as _, Result};
use flowstate_document::{
  AssetId, AssetRecord, BLOCKS_BY_ID, Block, BlockId, CellId, ColumnId, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD, DocumentPackage,
  DocumentProjection, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID, ImportedLoroDocument, InputBlock,
  InputBlockAlignment, InputEquationDisplay, InputImageSizing, InputParagraph, InputTableBlock, InputTableCell, InputTableCellBlock,
  InputTableColumn, InputTableColumnWidth, MAIN_BODY_BLOCK_ID, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE,
  MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, Paragraph, ParagraphId,
  ParagraphStyle, ProjectionDefect, ProjectionPatch, ProjectionStructuralBlock, ROOT, ROOT_BODY_FLOW_ID, ROOT_FIRST_PARAGRAPH_ID, RowId,
  RunSemanticStyle, RunStyles, SENTINEL_NEWLINE, SectionId, TableBlock, cell_loro_id, cell_loro_id_for, column_loro_id, document_from_loro,
  document_from_loro_with_defects, import_document_projection, loro_import::assets_from_document, loro_schema::body_text, new_loro_document,
  row_loro_id,
};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::{
  Container, ContainerID, ExportMode, Frontiers, ID, ImportStatus, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, Subscription,
  UndoItemMeta, UndoManager, ValueOrContainer, VersionRange, VersionVector,
  cursor::{Cursor, Side},
  event::{Diff, DiffEvent},
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

/// §P2a: commit origin stamped on canonical projection-repair batches. Excluded
/// from the local undo stack and used by peers/telemetry to identify repairs.
const REPAIR_ORIGIN: &str = "repair";

/// §P2a: maximum canonical-repair attempts per defect `stable_key`. A defect that
/// survives this many repair passes is quarantined (left as the deterministic
/// projection) instead of looping forever.
const PROJECTION_REPAIR_ATTEMPT_CAP: u64 = 3;

#[path = "crdt_runtime/projection_patch.rs"]
mod projection_patch;
#[path = "crdt_runtime/projection_repair.rs"]
mod projection_repair;
#[path = "crdt_runtime/table_ops.rs"]
mod table_ops;
#[path = "crdt_runtime/types.rs"]
mod types;
use crate::presence::{PresenceSelection, SelectionAffinity, SelectionDirection, SelectionEndpoint, VisualGravity};
use gpui_flowtext::{
  DocumentOffset, EditorSelection, ExternalCaret, ProjectionPatchBatch, apply_projection_patch_batch, replay_semantic_command_on_projection,
};
use loro::{ContainerTrait as _, cursor::PosType};
use projection_patch::{
  body_input_paragraph, projection_patches_between, remote_body_projection_patches, remote_nonstructural_projection_patches,
};
use types::UndoSelectionState;
pub use types::{
  EditorCommitResult, ProjectionFallbackStats, ProjectionInvalidation, ProjectionTextRange, RuntimeAssetMetadata, RuntimeEvent,
  RuntimePresenceCaretRequest, RuntimePresenceCarets, RuntimeRevisionInfo, SemanticCommand, StaleProjectionError, UndoSelectionAffinity,
  UndoSelectionDirection, UndoSelectionSnapshot,
};

#[derive(Debug)]
pub struct CrdtRuntime {
  doc: LoroDoc,
  projection: DocumentProjection,
  projection_index: ProjectionRuntimeIndex,
  undo: UndoManager,
  defer_undo_checkpoints: bool,
  undo_checkpoint_pending: bool,
  // §15: optional durable author identity. When set, revisions record this user
  // as their author; when `None`, authorship stays unset (behavior unchanged).
  author_user_id: Option<u128>,
  package: Option<DocumentPackage>,
  package_path: Option<PathBuf>,
  package_journal_prepared: bool,
  last_persisted_frontier: Frontiers,
  last_persisted_vv: VersionVector,
  undo_selection: Arc<Mutex<UndoSelectionState>>,
  subscription_events: Arc<Mutex<Vec<SubscriptionEventSummary>>>,
  // §23: monotonic runtime epoch. Bumped on every full projection rebuild/reset so
  // the permanent subscription can discard buffered summaries stamped before the
  // reset instead of relying on synchronous drain timing during import/checkout.
  runtime_epoch: Arc<AtomicU64>,
  local_subscription_updates: Arc<Mutex<Vec<Vec<u8>>>>,
  projection_fallback_counts: Mutex<BTreeMap<String, u64>>,
  // §P2a: per-`stable_key` projection-defect repair attempt counter (mirrors
  // `projection_fallback_counts`). A defect that survives `PROJECTION_REPAIR_ATTEMPT_CAP`
  // repair passes is quarantined instead of spinning.
  projection_repair_counts: Mutex<BTreeMap<String, u64>>,
  // §P2a re-entrancy guard: `schedule_projection_repairs` re-projects via
  // `refresh_projection`, which itself collects defects — the guard stops that
  // inner refresh from scheduling another repair pass (no infinite recursion).
  repairing_projection_defects: bool,
  _root_subscription: Subscription,
  _local_update_subscription: Subscription,
}

/// §24 style interval index entry: one run's byte span within a paragraph plus
/// its styles. `start`/`len` are byte offsets into the paragraph text.
#[derive(Clone, Copy, Debug)]
struct StyleInterval {
  start: usize,
  len: usize,
  styles: RunStyles,
}

/// §24/§P2b table row/column/cell index entry for one table block.
///
/// The projection's [`TableBlock`] now carries durable [`RowId`]/[`ColumnId`]/
/// [`CellId`] identifiers, so the index is keyed by those durable ids rather than
/// positional indices: `cells` maps each `(RowId, ColumnId)` coordinate to the
/// cell's deterministic [`CellId`]. This keeps the diagnostic invalidation index
/// stable under concurrent structural table edits (insert/remove/reorder).
#[derive(Clone, Debug, Default)]
struct TableIndexEntry {
  row_ids: Vec<RowId>,
  column_ids: Vec<ColumnId>,
  cells: FxHashMap<(RowId, ColumnId), CellId>,
}

/// §24 search unit span: a lightweight body-unicode range for one search unit.
/// `paragraph` is `Some(ix)` for a paragraph unit and `None` for an object
/// placeholder unit. Used to map changed body ranges onto affected search units.
#[derive(Clone, Copy, Debug)]
struct SearchUnitSpan {
  paragraph: Option<usize>,
  unicode_start: usize,
  unicode_len: usize,
}

#[derive(Debug, Default)]
struct ProjectionRuntimeIndex {
  // Original three indexes (§ earlier work): behavior and population unchanged.
  paragraph_body_unicode_starts: Vec<usize>,
  paragraph_boundary_positions: Vec<usize>,
  object_placeholder_positions: Vec<usize>,
  // §24 additive projection indexes, all derived from the projection in
  // `from_projection` and rebuilt whenever the original three are rebuilt.
  /// Paragraph metadata index: paragraph id → paragraph index.
  paragraph_metadata_by_id: FxHashMap<ParagraphId, usize>,
  /// Block anchor index: block id → block index in `projection.blocks`.
  block_anchor_by_id: FxHashMap<BlockId, usize>,
  /// Table row/column/cell index: table block id → its positional cell layout.
  table_cells_by_block: FxHashMap<BlockId, TableIndexEntry>,
  /// Style interval index: paragraph index → its run style intervals.
  style_runs_by_paragraph: FxHashMap<usize, Vec<StyleInterval>>,
  /// Section anchor index: section id → its start paragraph id.
  section_anchor_by_id: FxHashMap<SectionId, ParagraphId>,
  /// Asset reference index: asset id → blocks (image blocks) referencing it.
  asset_refs_by_id: FxHashMap<AssetId, Vec<BlockId>>,
  /// Search unit index: per-paragraph and per-object body-unicode spans.
  search_unit_spans: Vec<SearchUnitSpan>,
  /// Cursor resolution cache: encoded Loro cursor bytes → resolved document
  /// offset. Memoizes `resolve_undo_cursor`. Positions shift on any edit, so the
  /// cache is emptied on every rebuild (a fresh `from_projection`) and cleared in
  /// `update_for_patches` for incremental updates.
  cursor_resolution_cache: FxHashMap<Vec<u8>, DocumentOffset>,
}

impl ProjectionRuntimeIndex {
  fn from_projection(projection: &DocumentProjection) -> Self {
    let mut index = Self::default();
    let mut body_unicode = 1usize;
    let mut paragraph_ix = 0usize;
    let mut has_body_content = false;

    for (block_ix, block) in projection.blocks.iter().enumerate() {
      // §24 block anchor index: block id → block index. `block_ids` is parallel
      // to `blocks`, so the first occurrence matches the prior `.position` scans.
      if let Some(block_id) = projection.ids.block_ids.get(block_ix) {
        index
          .block_anchor_by_id
          .entry(*block_id)
          .or_insert(block_ix);
      }
      match block {
        Block::Paragraph(paragraph) => {
          if has_body_content {
            index.paragraph_boundary_positions.push(body_unicode);
            body_unicode = body_unicode.saturating_add(1);
          } else {
            index.paragraph_boundary_positions.push(0);
          }
          let paragraph_start = body_unicode;
          index.paragraph_body_unicode_starts.push(paragraph_start);
          let char_count = flowstate_document::paragraph_text(projection, paragraph_ix)
            .chars()
            .count();
          body_unicode = body_unicode.saturating_add(char_count);
          // §24 paragraph metadata index: paragraph id → paragraph index.
          if let Some(paragraph_id) = projection.ids.paragraph_ids.get(paragraph_ix) {
            index
              .paragraph_metadata_by_id
              .entry(*paragraph_id)
              .or_insert(paragraph_ix);
          }
          // §24 style interval index: per-paragraph run intervals.
          index
            .style_runs_by_paragraph
            .insert(paragraph_ix, style_intervals_for_paragraph(paragraph));
          // §24 search unit index: one body-text span per paragraph.
          index.search_unit_spans.push(SearchUnitSpan {
            paragraph: Some(paragraph_ix),
            unicode_start: paragraph_start,
            unicode_len: char_count,
          });
          paragraph_ix = paragraph_ix.saturating_add(1);
          has_body_content = true;
        },
        Block::Image(image) => {
          index.object_placeholder_positions.push(body_unicode);
          // §24 asset reference index: asset id → referencing image block ids.
          if let Some(block_id) = projection.ids.block_ids.get(block_ix) {
            index
              .asset_refs_by_id
              .entry(image.asset_id)
              .or_default()
              .push(*block_id);
          }
          index.search_unit_spans.push(SearchUnitSpan {
            paragraph: None,
            unicode_start: body_unicode,
            unicode_len: 1,
          });
          body_unicode = body_unicode.saturating_add(1);
          has_body_content = true;
        },
        Block::Equation(_) => {
          index.object_placeholder_positions.push(body_unicode);
          index.search_unit_spans.push(SearchUnitSpan {
            paragraph: None,
            unicode_start: body_unicode,
            unicode_len: 1,
          });
          body_unicode = body_unicode.saturating_add(1);
          has_body_content = true;
        },
        Block::Table(table) => {
          index.object_placeholder_positions.push(body_unicode);
          // §24 table row/column/cell index.
          if let Some(block_id) = projection.ids.block_ids.get(block_ix) {
            index
              .table_cells_by_block
              .insert(*block_id, table_index_entry(table));
          }
          index.search_unit_spans.push(SearchUnitSpan {
            paragraph: None,
            unicode_start: body_unicode,
            unicode_len: 1,
          });
          body_unicode = body_unicode.saturating_add(1);
          has_body_content = true;
        },
      }
    }

    // §24 section anchor index: section id → start paragraph id.
    for section in projection.sections.iter() {
      index
        .section_anchor_by_id
        .entry(section.id)
        .or_insert(section.start_paragraph);
    }

    // FS-170: surface any object placeholder whose two caret sides collapse to the
    // same document offset. Gated so a disabled build pays only one atomic load.
    if fidelity::enabled() {
      index.fidelity_check_object_sides(projection);
    }

    index
  }

  fn body_unicode_for_offset(&self, projection: &DocumentProjection, offset: DocumentOffset) -> Option<usize> {
    let paragraph = projection.paragraphs.get(offset.paragraph)?;
    let paragraph_text = flowstate_document::paragraph_text(projection, offset.paragraph);
    let byte = offset
      .byte
      .min(flowstate_document::paragraph_text_len(paragraph));
    if !paragraph_text.is_char_boundary(byte) {
      return None;
    }
    let unicode = *self.paragraph_body_unicode_starts.get(offset.paragraph)? + paragraph_text[..byte].chars().count();
    fidelity::event(FidelityClass::Caret, "offset->unicode", || format!("offset={offset:?} byte={byte} -> body_unicode={unicode}"));
    Some(unicode)
  }

  /// Like [`body_unicode_for_offset`], but returns a position in the ACTUAL live Loro
  /// body flow rather than the projection's coordinate space. Use this whenever the
  /// result feeds a Loro body mutation or a Loro cursor: `push_flow_blocks` can
  /// coalesce an object-adjacent empty paragraph out of the projection while its
  /// boundary newline stays PHYSICALLY in the body, so the projection-derived index
  /// runs short of the live body by one unicode per coalesced empty. Resolving the
  /// paragraph's start from its durable boundary cursor keeps the position in Loro
  /// space, so an edit lands where the projection intends instead of the phantom slot
  /// (which would materialize the coalesced paragraph and diverge the incremental
  /// projection from the full rebuild). Falls back to the projection-space start when
  /// the durable record can't be resolved (e.g. the boundary-0 sentinel).
  fn body_unicode_for_offset_in_loro(&self, doc: &LoroDoc, projection: &DocumentProjection, offset: DocumentOffset) -> Option<usize> {
    let paragraph = projection.paragraphs.get(offset.paragraph)?;
    let paragraph_text = flowstate_document::paragraph_text(projection, offset.paragraph);
    let byte = offset
      .byte
      .min(flowstate_document::paragraph_text_len(paragraph));
    if !paragraph_text.is_char_boundary(byte) {
      return None;
    }
    let paragraph_start = projection
      .ids
      .paragraph_ids
      .get(offset.paragraph)
      .and_then(|paragraph_id| paragraph_body_start_in_loro(doc, *paragraph_id))
      .or_else(|| self.paragraph_body_unicode_starts.get(offset.paragraph).copied())?;
    let unicode = paragraph_start + paragraph_text[..byte].chars().count();
    fidelity::event(FidelityClass::Caret, "offset->unicode-loro", || format!("offset={offset:?} byte={byte} -> body_unicode={unicode}"));
    Some(unicode)
  }

  fn offset_for_body_unicode(&self, projection: &DocumentProjection, unicode: usize) -> Option<DocumentOffset> {
    // FS-170: the body-unicode slot immediately AFTER a block-object placeholder
    // is the interstitial paragraph boundary. A caret resting there is "after the
    // object" and belongs to the FOLLOWING paragraph's start — not the preceding
    // paragraph's end, which the object's own slot (`position`) already resolves
    // to. Without this redirect both sides of the object collapse onto the same
    // offset (the `object-side-collapse` fidelity violation). Only fires when a
    // paragraph actually starts just past the boundary (an object between two
    // paragraphs); a trailing object falls through to the normal clamp.
    if unicode > 0 && self.object_placeholder_positions.contains(&(unicode - 1)) {
      let following_start = unicode + 1;
      if let Ok(start_ix) = self.paragraph_body_unicode_starts.binary_search(&following_start) {
        let paragraph_ix = start_ix.min(projection.paragraphs.len().saturating_sub(1));
        let offset = DocumentOffset { paragraph: paragraph_ix, byte: 0 };
        fidelity::event(FidelityClass::Caret, "unicode->offset-after-object", || {
          format!("body_unicode={unicode} -> offset={offset:?}")
        });
        return Some(offset);
      }
    }
    let paragraph_ix = self.paragraph_at_body_unicode(unicode, projection.paragraphs.len())?;
    let paragraph_start = self
      .paragraph_body_unicode_starts
      .get(paragraph_ix)
      .copied()
      .unwrap_or_default();
    let local_unicode = unicode.saturating_sub(paragraph_start);
    let paragraph_text = flowstate_document::paragraph_text(projection, paragraph_ix);
    let byte = paragraph_text
      .char_indices()
      .nth(local_unicode)
      .map_or(paragraph_text.len(), |(byte, _)| byte);
    let offset = DocumentOffset {
      paragraph: paragraph_ix,
      byte,
    };
    fidelity::event(FidelityClass::Caret, "unicode->offset", || format!("body_unicode={unicode} -> offset={offset:?}"));
    Some(offset)
  }

  /// FS-170 diagnostic: a U+FFFC object placeholder occupies a single body-unicode
  /// slot, but a caret can rest on either side of it. The slot before the object
  /// (its own position) and the slot after it (`position + 1`) must resolve to
  /// distinct document offsets; a collapse means carets on the two sides of the
  /// object are indistinguishable. Emits the resolved offsets and, when both
  /// resolve, checks they differ (kind `object-side-collapse`). Runs only when
  /// fidelity tracing is enabled; read-only.
  fn fidelity_check_object_sides(&self, projection: &DocumentProjection) {
    for &position in &self.object_placeholder_positions {
      let left = self.offset_for_body_unicode(projection, position);
      let right = self.offset_for_body_unicode(projection, position.saturating_add(1));
      fidelity::event(FidelityClass::Caret, "object-side-offsets", || {
        format!("placeholder_body_unicode={position} left_offset={left:?} right_offset={right:?}")
      });
      if let (Some(left), Some(right)) = (left, right) {
        fidelity::check(
          left != right,
          FidelityClass::Caret,
          "object-side-collapse",
          || format!("U+FFFC object at body-unicode {position} collapses caret sides: both map to {left:?}"),
        );
      }
    }
  }

  fn paragraphs_for_changed_ranges(&self, ranges: &[ProjectionTextRange], paragraph_count: usize, live_starts: &[usize]) -> Vec<usize> {
    let mut touched = std::collections::BTreeSet::new();
    for range in ranges
      .iter()
      .filter(|range| range.flow_id == ROOT_BODY_FLOW_ID)
    {
      let start = self.paragraph_at_body_unicode_with(live_starts, range.unicode_start, paragraph_count);
      let end = self.paragraph_at_body_unicode_with(live_starts, range.unicode_start.saturating_add(range.unicode_len), paragraph_count);
      if let Some(start) = start {
        touched.insert(start);
      }
      if let Some(end) = end {
        touched.insert(end);
      }
      if let (Some(start), Some(end)) = (start, end) {
        touched.extend(start.min(end)..=start.max(end));
      }
    }
    touched.into_iter().collect()
  }

  fn paragraph_at_body_unicode_with(&self, live_starts: &[usize], unicode: usize, paragraph_count: usize) -> Option<usize> {
    // The changed-range unicode positions are in POST-import (new-body) coordinates,
    // so map them against paragraph starts derived from the CURRENT Loro body, not the
    // stale pre-import `paragraph_body_unicode_starts`. On the incremental path any
    // boundary (`\n`) insert/delete forces a full rebuild, so the paragraph count is
    // stable and `live_starts[i]` aligns with projection paragraph `i`. Falls back to
    // the prebuilt index only when live starts are unavailable/mismatched.
    let starts = if live_starts.len() == paragraph_count && !live_starts.is_empty() {
      live_starts
    } else {
      self.paragraph_body_unicode_starts.as_slice()
    };
    if paragraph_count == 0 || starts.is_empty() {
      return None;
    }
    match starts.binary_search(&unicode) {
      Ok(ix) => Some(ix.min(paragraph_count - 1)),
      Err(0) => Some(0),
      Err(ix) => Some((ix - 1).min(paragraph_count - 1)),
    }
  }

  fn paragraph_at_body_unicode(&self, unicode: usize, paragraph_count: usize) -> Option<usize> {
    if paragraph_count == 0 || self.paragraph_body_unicode_starts.is_empty() {
      return None;
    }
    match self.paragraph_body_unicode_starts.binary_search(&unicode) {
      Ok(ix) => Some(ix.min(paragraph_count - 1)),
      Err(0) => Some(0),
      Err(ix) => Some((ix - 1).min(paragraph_count - 1)),
    }
  }

  fn deleted_range_contains_structure(&self, start: usize, len: usize) -> bool {
    if len == 0 {
      return false;
    }
    let end = start.saturating_add(len);
    self
      .paragraph_boundary_positions
      .iter()
      .chain(&self.object_placeholder_positions)
      .any(|position| (start..end).contains(position))
  }

  /// §24 paragraph metadata index lookup: paragraph id → paragraph index.
  ///
  /// O(1) through `paragraph_metadata_by_id`, falling back to the prior linear
  /// `position` scan when the id is absent so the semantics (first match, or
  /// `None`) are identical to the scans this replaces.
  fn paragraph_index_for_id(&self, projection: &DocumentProjection, id: ParagraphId) -> Option<usize> {
    if let Some(&paragraph_ix) = self.paragraph_metadata_by_id.get(&id) {
      return Some(paragraph_ix);
    }
    projection
      .ids
      .paragraph_ids
      .iter()
      .position(|candidate| *candidate == id)
  }

  /// §24 block anchor index lookup: block id → block index in `projection.blocks`.
  ///
  /// O(1) through `block_anchor_by_id`, with the same linear fallback so callers
  /// keep identical semantics to the `projection.ids.block_ids` scans.
  fn block_index_for_id(&self, projection: &DocumentProjection, id: BlockId) -> Option<usize> {
    if let Some(&block_ix) = self.block_anchor_by_id.get(&id) {
      return Some(block_ix);
    }
    projection
      .ids
      .block_ids
      .iter()
      .position(|candidate| *candidate == id)
  }

  /// §24 style interval index lookup: run styles covering `byte` in a paragraph.
  fn run_styles_at(&self, paragraph_ix: usize, byte: usize) -> Option<RunStyles> {
    self
      .style_runs_by_paragraph
      .get(&paragraph_ix)?
      .iter()
      .find(|interval| byte >= interval.start && byte < interval.start.saturating_add(interval.len))
      .map(|interval| interval.styles)
  }

  /// §24 search unit index lookup: indices of search-unit spans overlapping any
  /// body-text range in `ranges`. Mirrors `paragraphs_for_changed_ranges` but
  /// resolves search units rather than paragraphs.
  fn search_units_for_changed_ranges(&self, ranges: &[ProjectionTextRange]) -> Vec<usize> {
    self
      .search_unit_spans
      .iter()
      .enumerate()
      .filter_map(|(unit_ix, span)| {
        let unit_end = span.unicode_start.saturating_add(span.unicode_len.max(1));
        let overlaps = ranges
          .iter()
          .filter(|range| range.flow_id == ROOT_BODY_FLOW_ID)
          .any(|range| {
            let range_end = range.unicode_start.saturating_add(range.unicode_len.max(1));
            span.unicode_start < range_end && range.unicode_start < unit_end
          });
        overlaps.then_some(unit_ix)
      })
      .collect()
  }

  fn update_for_patches(&mut self, projection: &DocumentProjection, patches: &[ProjectionPatch]) -> bool {
    // §24: resolved cursor offsets shift whenever the projection's positions
    // change, so invalidate the memoized cursor cache on every incremental
    // update. (Full rebuilds construct a fresh index, which starts empty.)
    self.cursor_resolution_cache.clear();
    let mut text_deltas = Vec::new();
    let mut rebuild = false;
    for patch in patches {
      match patch {
        ProjectionPatch::ParagraphText {
          block_id,
          paragraph_id,
          row_hint,
          new,
          ..
        } => {
          let Some(paragraph_ix) = paragraph_index_for_patch(projection, *block_id, *paragraph_id, *row_hint) else {
            rebuild = true;
            break;
          };
          let old_len = flowstate_document::paragraph_text(projection, paragraph_ix)
            .chars()
            .count();
          let new_len = new
            .runs
            .iter()
            .map(|run| run.text.chars().count())
            .sum::<usize>();
          text_deltas.push((paragraph_ix, new_len as isize - old_len as isize));
        },
        ProjectionPatch::InsertBlocks { .. } | ProjectionPatch::DeleteBlocks { .. } | ProjectionPatch::MoveBlock { .. } => {
          rebuild = true;
          break;
        },
        ProjectionPatch::ParagraphStyle { .. }
        | ProjectionPatch::ParagraphRuns { .. }
        | ProjectionPatch::ReplaceObjectBlock { .. }
        | ProjectionPatch::AssetArrived { .. } => {},
      }
    }
    if rebuild {
      return true;
    }
    for (paragraph_ix, delta) in text_deltas {
      if delta == 0 {
        continue;
      }
      for start in self
        .paragraph_body_unicode_starts
        .iter_mut()
        .skip(paragraph_ix.saturating_add(1))
      {
        *start = start.saturating_add_signed(delta);
      }
      for boundary in self
        .paragraph_boundary_positions
        .iter_mut()
        .skip(paragraph_ix.saturating_add(1))
      {
        *boundary = boundary.saturating_add_signed(delta);
      }
      let threshold = self
        .paragraph_body_unicode_starts
        .get(paragraph_ix)
        .copied()
        .unwrap_or_default();
      for placeholder in self
        .object_placeholder_positions
        .iter_mut()
        .filter(|position| **position > threshold)
      {
        *placeholder = placeholder.saturating_add_signed(delta);
      }
      for span in &mut self.search_unit_spans {
        if span.paragraph == Some(paragraph_ix) {
          span.unicode_len = span.unicode_len.saturating_add_signed(delta);
        } else if span.unicode_start > threshold {
          span.unicode_start = span.unicode_start.saturating_add_signed(delta);
        }
      }
    }
    false
  }
}

fn paragraph_index_for_block_row(projection: &DocumentProjection, row: usize) -> Option<usize> {
  matches!(projection.blocks.get(row), Some(Block::Paragraph(_))).then(|| {
    projection
      .blocks
      .iter()
      .take(row)
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count()
  })
}

fn paragraph_index_for_patch(
  projection: &DocumentProjection,
  block_id: flowstate_document::BlockId,
  paragraph_id: flowstate_document::ParagraphId,
  row_hint: usize,
) -> Option<usize> {
  let row = if projection.ids.block_ids.get(row_hint).copied() == Some(block_id) {
    row_hint
  } else {
    projection
      .ids
      .block_ids
      .iter()
      .position(|id| *id == block_id)?
  };
  let paragraph_ix = paragraph_index_for_block_row(projection, row)?;
  (projection.ids.paragraph_ids.get(paragraph_ix).copied() == Some(paragraph_id)).then_some(paragraph_ix)
}

/// §24 style interval index builder: maps a paragraph's runs to byte-offset
/// `{start, len, styles}` intervals. Run lengths are byte lengths, matching the
/// projection's `TextRun::len`.
fn style_intervals_for_paragraph(paragraph: &Paragraph) -> Vec<StyleInterval> {
  let mut intervals = Vec::with_capacity(paragraph.runs.len());
  let mut start = 0usize;
  for run in &paragraph.runs {
    intervals.push(StyleInterval {
      start,
      len: run.len,
      styles: run.styles,
    });
    start = start.saturating_add(run.len);
  }
  intervals
}

/// §24/§P2b table row/column/cell index builder, keyed by the model's durable
/// [`RowId`]/[`ColumnId`]/[`CellId`] identifiers so the index survives concurrent
/// structural table edits.
fn table_index_entry(table: &TableBlock) -> TableIndexEntry {
  let mut cells = FxHashMap::default();
  for row in &table.rows {
    for cell in &row.cells {
      cells.insert((cell.row_id, cell.column_id), cell.id);
    }
  }
  TableIndexEntry {
    row_ids: table.rows.iter().map(|row| row.id).collect(),
    column_ids: table.columns.iter().map(|column| column.id).collect(),
    cells,
  }
}

impl CrdtRuntime {
  pub fn new_empty(title: &str) -> Result<Self> {
    let doc = new_loro_document(title).context("initializing Loro document")?;
    Self::from_doc(doc, None, None)
  }

  pub fn open_package(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref();
    let package = DocumentPackage::read(path).with_context(|| format!("reading Flowstate package {}", path.display()))?;
    let projection = package
      .current_projection_document()
      .context("reading frontier-matched package projection cache")?;
    let doc = package
      .load_loro_doc()
      .context("loading Loro document from package")?;
    let mut runtime = Self::from_doc_with_projection(doc, Some(package), Some(path.to_path_buf()), projection)?;
    runtime.package_journal_prepared = true;
    Ok(runtime)
  }

  pub fn from_package(package: DocumentPackage, package_path: Option<PathBuf>) -> Result<Self> {
    let projection = package
      .current_projection_document()
      .context("reading frontier-matched package projection cache")?;
    let doc = package
      .load_loro_doc()
      .context("loading Loro document from package")?;
    Self::from_doc_with_projection(doc, Some(package), package_path, projection)
  }

  pub fn from_document_projection(document: &DocumentProjection, title: &str) -> Result<Self> {
    let imported = import_document_projection(document.clone(), title).context("importing projected document into canonical Loro runtime")?;
    Self::from_imported_document(imported)
  }

  pub fn from_imported_document(imported: ImportedLoroDocument) -> Result<Self> {
    let ImportedLoroDocument { doc, projection } = imported;
    Self::from_doc_with_projection_options(doc, None, None, Some(projection), false)
  }

  pub fn from_doc(doc: LoroDoc, package: Option<DocumentPackage>, package_path: Option<PathBuf>) -> Result<Self> {
    Self::from_doc_with_projection(doc, package, package_path, None)
  }

  fn from_doc_with_projection(
    doc: LoroDoc,
    package: Option<DocumentPackage>,
    package_path: Option<PathBuf>,
    projection: Option<DocumentProjection>,
  ) -> Result<Self> {
    Self::from_doc_with_projection_options(doc, package, package_path, projection, true)
  }

  fn from_doc_with_projection_options(
    doc: LoroDoc,
    mut package: Option<DocumentPackage>,
    package_path: Option<PathBuf>,
    projection: Option<DocumentProjection>,
    repair_paragraph_style_marks: bool,
  ) -> Result<Self> {
    // Every `CrdtRuntime` marks paragraph/run styles on its canonical body text,
    // and `text.mark(..)` errors ("Style configuration missing") unless the doc
    // has been style-configured. `new_loro_document` and the package loaders
    // configure the docs they build, but a runtime can also be constructed from a
    // bare `LoroDoc` received over the network or restored elsewhere; guarantee
    // the invariant here so no construction path can leave marking broken. The
    // call is idempotent, so re-configuring an already-configured doc is a no-op.
    flowstate_document::loro_schema::configure_text_styles(&doc);
    let frontier_before_startup_metadata = doc.state_frontiers().encode();
    let projection_content_repaired = if repair_paragraph_style_marks {
      persist_body_paragraph_style_mark_repair(&doc, package.as_mut(), package_path.as_deref())?
    } else {
      // Trusted import builders apply every paragraph boundary mark while constructing
      // the body. Avoid materializing a full rich-text delta only to verify it again.
      flowstate_document::register_replica(&doc, None)?;
      false
    };
    let current_frontier = doc.state_frontiers().encode();
    // §P2a: projecting from canonical Loro reports defects; the two trusted-input
    // arms below skip projection, so they contribute none.
    let mut startup_defects: Vec<ProjectionDefect> = Vec::new();
    let mut projection = match projection {
      Some(projection) if projection.frontier == current_frontier => projection,
      Some(mut projection) if !projection_content_repaired && projection.frontier == frontier_before_startup_metadata => {
        projection.frontier.clone_from(&current_frontier);
        projection
      },
      None => {
        let (projection, defects) = document_from_loro_with_defects(&doc).context("building initial projection from canonical Loro state")?;
        startup_defects = defects;
        projection
      },
      Some(_) => {
        let (projection, defects) = document_from_loro_with_defects(&doc).context("rebuilding stale initial projection")?;
        startup_defects = defects;
        projection
      },
    };
    if let Some(package) = &package {
      attach_package_assets(&mut projection, package);
    }
    let last_persisted_frontier = doc.state_frontiers();
    let last_persisted_vv = doc.state_vv();
    let subscription_events = Arc::new(Mutex::new(Vec::new()));
    let subscription_events_for_callback = Arc::clone(&subscription_events);
    let runtime_epoch = Arc::new(AtomicU64::new(0));
    let runtime_epoch_for_callback = Arc::clone(&runtime_epoch);
    // §23: the callback owns a reference clone of the document (a shared handle, not a
    // deep fork) so it can stamp the live state frontier on every summary. This is
    // deadlock-safe: Loro releases the doc state lock before dispatching observers
    // (see `emit_events`: "we should not hold the lock when emitting events"), and
    // `state_frontiers()` only takes that same lock briefly to clone the frontier.
    let doc_for_callback = doc.clone();
    let root_subscription = doc.subscribe_root(Arc::new(move |event: DiffEvent<'_>| {
      let mut summary = summarize_subscription_event(&event);
      // §23: stamp the runtime epoch and the emit-time frontier. Summaries stamped
      // before a full rebuild (different epoch) or ahead of the drain target (later
      // frontier) are filtered in `merge_subscription_invalidation`.
      summary.epoch = runtime_epoch_for_callback.load(AtomicOrdering::SeqCst);
      summary.frontier = doc_for_callback.state_frontiers().encode();
      tracing::trace!(
        origin = %summary.origin,
        trigger = %summary.triggered_by,
        epoch = summary.epoch,
        frontier_len = summary.frontier.len(),
        changes = summary.changes.len(),
        "Flowstate Loro root event",
      );
      if let Ok(mut events) = subscription_events_for_callback.lock() {
        events.push(summary);
      }
    }));
    let local_subscription_updates = Arc::new(Mutex::new(Vec::new()));
    let local_updates_for_callback = Arc::clone(&local_subscription_updates);
    let local_update_subscription = doc.subscribe_local_update(Box::new(move |bytes| {
      tracing::trace!(bytes = bytes.len(), "Flowstate Loro local update");
      if let Ok(mut updates) = local_updates_for_callback.lock() {
        updates.push(bytes.clone());
      }
      true
    }));
    let mut undo = UndoManager::new(&doc);
    undo.set_merge_interval(600);
    undo.set_max_undo_steps(300);
    undo.add_exclude_origin_prefix("remote");
    // §P2a: canonical repair commits carry the `repair` origin and must never
    // enter the local undo stack (they are convergent housekeeping, not edits).
    undo.add_exclude_origin_prefix("repair");
    let undo_selection = Arc::new(Mutex::new(UndoSelectionState::default()));
    install_undo_selection_callbacks(&mut undo, &undo_selection);
    let projection_index = ProjectionRuntimeIndex::from_projection(&projection);
    let mut runtime = Self {
      doc,
      projection,
      projection_index,
      undo,
      defer_undo_checkpoints: false,
      undo_checkpoint_pending: false,
      author_user_id: None,
      package,
      package_path,
      package_journal_prepared: false,
      last_persisted_frontier,
      last_persisted_vv,
      undo_selection,
      subscription_events,
      runtime_epoch,
      local_subscription_updates,
      projection_fallback_counts: Mutex::new(BTreeMap::new()),
      projection_repair_counts: Mutex::new(BTreeMap::new()),
      repairing_projection_defects: false,
      _root_subscription: root_subscription,
      _local_update_subscription: local_update_subscription,
    };
    // §P2a: repair any malformed canonical state the initial projection reported.
    // At construction there is no peer channel to emit onto, but the repair is
    // committed + persisted, so peers receive it via the update segment / next
    // anti-entropy pull. Errors are logged, never fatal to opening the document.
    if !startup_defects.is_empty()
      && let Err(error) = runtime.schedule_projection_repairs(startup_defects)
    {
      tracing::error!(%error, "scheduling projection repairs during runtime construction failed");
    }
    Ok(runtime)
  }

  pub(crate) fn doc(&self) -> &LoroDoc {
    &self.doc
  }

  /// §15: bind a stable durable author identity to this live replica.
  ///
  /// Registers (or refreshes) the user record in `users_by_id`, links the current
  /// Loro replica to that user, and stores the id so later revisions record it as
  /// their author. Until this is called, `author_user_id` stays `None` and
  /// authorship is left unset (behavior unchanged).
  pub fn set_author_identity(&mut self, user_id: u128, display_name: Option<String>) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    flowstate_document::register_user(&self.doc, user_id, display_name.as_deref()).context("registering durable author identity")?;
    self.doc.commit();
    self.author_user_id = Some(user_id);

    let update = match self.local_update_bytes(&from_vv) {
      Ok(update) => update,
      Err(error) => {
        tracing::error!(%error, "exporting committed author-identity update failed; later synchronization must recover it");
        return Ok(Vec::new());
      },
    };
    if update.is_empty() {
      return Ok(Vec::new());
    }
    if let Err(error) = self.persist_update_segment(from_frontier, from_vv, update.clone()) {
      tracing::error!(%error, "persisting committed author-identity update failed");
    }
    Ok(vec![RuntimeEvent::LocalUpdate {
      bytes: update,
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    }])
  }

  pub fn set_pending_undo_selection(&mut self, selection: Option<UndoSelectionSnapshot>) -> Result<()> {
    let pending_selection = selection
      .map(|selection| postcard::to_stdvec(&selection).context("encoding undo selection snapshot failed"))
      .transpose()?;
    if let Ok(mut state) = self.undo_selection.lock() {
      state.pending_selection = pending_selection;
    }
    Ok(())
  }

  pub fn take_restored_undo_selection(&mut self) -> Option<UndoSelectionSnapshot> {
    self
      .undo_selection
      .lock()
      .ok()
      .and_then(|mut state| state.restored_selection.take())
  }

  fn record_undo_checkpoint(&mut self) -> Result<()> {
    if self.defer_undo_checkpoints {
      self.undo_checkpoint_pending = true;
      return Ok(());
    }
    self
      .undo
      .record_new_checkpoint()
      .context("recording Loro undo checkpoint")?;
    Ok(())
  }

  fn undo_selection_for_projection(&self, projection: &DocumentProjection, selection: &EditorSelection) -> Option<UndoSelectionSnapshot> {
    let direction = selection_direction(selection.anchor, selection.head);
    let anchor_affinity = SelectionAffinity::from(selection.anchor_affinity);
    let head_affinity = SelectionAffinity::from(selection.head_affinity);
    let body = body_text(&self.doc);
    let anchor = clamp_projection_offset(projection, selection.anchor);
    let head = clamp_projection_offset(projection, selection.head);
    let projection_index = ProjectionRuntimeIndex::from_projection(projection);
    // Intentionally projection-space (NOT the `_in_loro` variant): a selection is
    // encoded to a Loro cursor here and decoded back via `offset_for_body_unicode`,
    // which is also projection-space. Both directions must use the SAME space or the
    // round-trip drifts on object-adjacent-empty docs (guarded by
    // `caret_offset_round_trips_through_projection_space`). Only direct body mutations
    // — which don't round-trip — use the Loro-space resolver (FS-170 fix 5d50099).
    let anchor_pos = projection_index.body_unicode_for_offset(projection, anchor)?;
    let head_pos = projection_index.body_unicode_for_offset(projection, head)?;
    let anchor_cursor = cursor_for_boundary(&body, anchor_pos, anchor_affinity)?.encode();
    let head_cursor = cursor_for_boundary(&body, head_pos, head_affinity)?.encode();
    Some(UndoSelectionSnapshot {
      anchor_cursor,
      head_cursor,
      anchor_affinity: undo_affinity(anchor_affinity),
      head_affinity: undo_affinity(head_affinity),
      direction: match direction {
        SelectionDirection::Forward => UndoSelectionDirection::Forward,
        SelectionDirection::Backward => UndoSelectionDirection::Backward,
        SelectionDirection::None => UndoSelectionDirection::None,
      },
    })
  }

  pub fn apply_editor_semantic_command(
    &mut self,
    projection: &DocumentProjection,
    command: &EditorSemanticCommand,
  ) -> Result<Vec<RuntimeEvent>> {
    self.apply_editor_semantic_command_with_projection(projection, command, true)
  }

  pub fn apply_editor_semantic_command_without_projection(
    &mut self,
    projection: &DocumentProjection,
    command: &EditorSemanticCommand,
  ) -> Result<Vec<RuntimeEvent>> {
    self.apply_editor_semantic_command_with_projection(projection, command, false)
  }

  pub fn try_apply_editor_semantic_command_without_projection(&mut self, command: &EditorSemanticCommand) -> Result<Option<Vec<RuntimeEvent>>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    if apply_editor_semantic_command_body_fast_path(&self.doc, &self.projection, &self.projection_index, command)? {
      self.doc.commit();
      self.record_undo_checkpoint()?;
      let mut invalidation = ProjectionInvalidation::body_text(
        from_frontier.encode(),
        self.doc.state_frontiers().encode(),
        0,
        body_text(&self.doc).len_unicode(),
      );
      self.merge_subscription_invalidation(&mut invalidation);
      let mut events = self.events_after_local_change(from_frontier, from_vv, invalidation.clone(), false)?;
      if let Some(patches) = incremental_projection_patches_for_command(&self.projection, &self.projection_index, &self.doc, command) {
        self.apply_projection_patch_set(&patches);
        self.projection.frontier = self.doc.state_frontiers().encode();
        events.push(self.projection_patched_event(patches, invalidation));
      } else {
        let before_projection = self.projection.clone();
        self.refresh_projection()?;
        events.push(self.projection_change_event(&before_projection, invalidation)?);
      }
      return Ok(Some(events));
    }
    Ok(None)
  }

  fn apply_editor_semantic_command_with_projection(
    &mut self,
    projection: &DocumentProjection,
    command: &EditorSemanticCommand,
    emit_projection: bool,
  ) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    if apply_editor_semantic_command(&self.doc, projection, command)? {
      self.doc.commit();
      self.record_undo_checkpoint()?;
      let mut invalidation = editor_command_invalidation(projection, command, from_frontier.encode(), self.doc.state_frontiers().encode());
      self.merge_subscription_invalidation(&mut invalidation);
      let mut events = self.events_after_local_change(from_frontier, from_vv, invalidation.clone(), false)?;
      if emit_projection {
        if let Some(patches) = incremental_projection_patches_for_command(&self.projection, &self.projection_index, &self.doc, command) {
          self.apply_projection_patch_set(&patches);
          self.projection.frontier = self.doc.state_frontiers().encode();
          events.push(self.projection_patched_event(patches, invalidation));
        } else {
          let before_projection = self.projection.clone();
          self.refresh_projection()?;
          events.push(self.projection_change_event(&before_projection, invalidation)?);
        }
      } else {
        self.refresh_projection()?;
      }
      Ok(events)
    } else {
      Ok(Vec::new())
    }
  }

  pub fn projection_snapshot(&self) -> Result<DocumentProjection> {
    Ok(self.projection.clone())
  }

  pub fn asset_metadata(&self) -> Result<Vec<RuntimeAssetMetadata>> {
    let root = self.doc.get_map(ROOT);
    let Some(ValueOrContainer::Container(Container::Map(assets_by_id))) = root.get(flowstate_document::loro_schema::ASSETS_BY_ID) else {
      return Ok(Vec::new());
    };
    let mut assets = Vec::new();
    for key in assets_by_id.keys() {
      let Some(ValueOrContainer::Container(Container::Map(map))) = assets_by_id.get(&key) else {
        continue;
      };
      let Some(asset_id) = map_string_opt(&map, "asset_id").and_then(|value| value.parse::<u128>().ok()) else {
        continue;
      };
      let byte_length = map_i64_opt(&map, "byte_length").unwrap_or_default().max(0) as u64;
      let Some(content_hash) = map_string_opt(&map, "content_hash").and_then(|hash| parse_blake3_hex(&hash)) else {
        tracing::warn!(asset_id, "ignoring asset metadata with an invalid BLAKE3 digest");
        continue;
      };
      if byte_length == 0 {
        continue;
      }
      assets.push(RuntimeAssetMetadata {
        asset_id,
        content_hash,
        mime_type: map_string_opt(&map, "mime_type").unwrap_or_else(|| "application/octet-stream".to_string()),
        original_name: map_string_opt(&map, "original_name"),
        byte_length,
      });
    }
    Ok(assets)
  }

  pub fn revisions(&self) -> Vec<RuntimeRevisionInfo> {
    self
      .package
      .as_ref()
      .map(|package| {
        package
          .revisions
          .iter()
          .rev()
          .map(|revision| RuntimeRevisionInfo {
            revision_id: revision.revision_id,
            title: revision.title.clone(),
            summary: revision.summary.clone(),
            created_at_unix_secs: revision.created_at_unix_secs,
          })
          .collect()
      })
      .unwrap_or_default()
  }

  pub fn presence_selection(&self, selection: &EditorSelection) -> Option<PresenceSelection> {
    let direction = selection_direction(selection.anchor, selection.head);
    // §16: read the genuine, stored affinity/gravity off each endpoint instead
    // of guessing a side from the selection's direction.
    let anchor_affinity = SelectionAffinity::from(selection.anchor_affinity);
    let head_affinity = SelectionAffinity::from(selection.head_affinity);
    let anchor_gravity = VisualGravity::from(selection.anchor_gravity);
    let head_gravity = VisualGravity::from(selection.head_gravity);
    Some(PresenceSelection {
      anchor: self.presence_endpoint(selection.anchor, anchor_affinity, anchor_gravity)?,
      head: self.presence_endpoint(selection.head, head_affinity, head_gravity)?,
      direction,
    })
  }

  pub fn resolve_presence_carets(&self, requests: Vec<RuntimePresenceCaretRequest>) -> RuntimePresenceCarets {
    let text = body_text(&self.doc);
    let carets = requests
      .into_iter()
      .filter_map(|request| {
        let cursor = Cursor::decode(&request.selection.head.cursor).ok()?;
        if cursor.container != text.id() {
          return None;
        }
        let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
        let unicode = resolved_cursor_boundary_unicode(&text, &resolved)?;
        Some(ExternalCaret {
          offset: self
            .projection_index
            .offset_for_body_unicode(&self.projection, unicode)?,
          visual_gravity: gpui_gravity_from_presence(request.selection.head.visual_gravity),
          color_rgb: request.color_rgb,
        })
      })
      .collect();
    RuntimePresenceCarets { carets }
  }

  fn presence_endpoint(&self, offset: DocumentOffset, affinity: SelectionAffinity, visual_gravity: VisualGravity) -> Option<SelectionEndpoint> {
    let text = body_text(&self.doc);
    let offset = clamp_projection_offset(&self.projection, offset);
    // Intentionally projection-space: the endpoint encodes to a Loro cursor and is
    // decoded back via projection-space `offset_for_body_unicode`; both sides must
    // share a space or the presence caret drifts on object-adjacent-empty docs. Only
    // direct (non-round-tripping) body mutations use the Loro-space resolver (5d50099).
    let pos = self
      .projection_index
      .body_unicode_for_offset(&self.projection, offset)?;
    cursor_for_boundary(&text, pos, affinity).map(|cursor| SelectionEndpoint {
      cursor: cursor.encode(),
      affinity,
      visual_gravity,
    })
  }

  pub fn merge_asset_records(&mut self, records: Vec<AssetRecord>) -> Result<Vec<RuntimeEvent>> {
    let base_frontier = self.projection.frontier.clone();
    Ok(
      self
        .apply_editor_transaction(Uuid::new_v4().as_u128(), &base_frontier, &[], &records, None)?
        .events,
    )
  }

  pub fn apply_editor_commands(
    &mut self,
    transaction_id: u128,
    base_frontier: &[u8],
    commands: &[EditorSemanticCommand],
    selection_after: Option<&EditorSelection>,
  ) -> Result<EditorCommitResult> {
    self.apply_editor_transaction(transaction_id, base_frontier, commands, &[], selection_after)
  }

  pub fn apply_editor_transaction(
    &mut self,
    transaction_id: u128,
    base_frontier: &[u8],
    commands: &[EditorSemanticCommand],
    asset_records: &[AssetRecord],
    selection_after: Option<&EditorSelection>,
  ) -> Result<EditorCommitResult> {
    let current_projection_frontier = self.projection.frontier.clone();
    if !base_frontier.is_empty() && base_frontier != current_projection_frontier.as_slice() {
      return Err(
        StaleProjectionError {
          expected_frontier_len: base_frontier.len(),
          current_frontier_len: current_projection_frontier.len(),
        }
        .into(),
      );
    }

    let mut predicted_projection = self.projection.clone();
    for (command_ix, command) in commands.iter().enumerate() {
      if editor_semantic_command_is_noop(command) {
        continue;
      }
      if !replay_semantic_command_on_projection(&mut predicted_projection, command) {
        anyhow::bail!("editor command {command_ix} failed stable-identity preflight: {command:?}");
      }
    }
    let asset_merge = merge_asset_records_into_projection(&mut predicted_projection, asset_records);
    let commands_change_document = commands
      .iter()
      .any(|command| !editor_semantic_command_is_noop(command));
    if !commands_change_document && !asset_merge.any_changed {
      return Ok(EditorCommitResult {
        transaction_id,
        base_frontier: current_projection_frontier.clone(),
        new_frontier: current_projection_frontier,
        events: Vec::new(),
      });
    }

    if commands
      .iter()
      .any(editor_semantic_command_requires_staging_validation)
      || asset_merge.metadata_changed
    {
      let staging_doc = self.doc.fork();
      let mut staging_projection = self.projection.clone();
      for (command_ix, command) in commands.iter().enumerate() {
        if editor_semantic_command_is_noop(command) {
          continue;
        }
        let applied = apply_editor_semantic_command(&staging_doc, &staging_projection, command)
          .with_context(|| format!("validating editor command {command_ix} against staged canonical state"))?;
        if !applied {
          anyhow::bail!("editor command {command_ix} was rejected by staged canonical validation: {command:?}");
        }
        let replayed = replay_semantic_command_on_projection(&mut staging_projection, command);
        debug_assert!(replayed, "preflighted editor command must replay on staged projection");
      }
      staging_projection.assets = predicted_projection.assets.clone();
      if asset_merge.metadata_changed {
        flowstate_document::loro_import::import_assets(&staging_doc, &staging_projection)
          .context("validating canonical asset metadata in staged editor transaction")?;
        refresh_image_asset_metadata(&staging_doc).context("validating image asset metadata in staged editor transaction")?;
      }
    }

    let mut staged_package = self.package.clone();
    if asset_merge.any_changed
      && let Some(package) = staged_package.as_mut()
    {
      package.replace_assets_from_document(&predicted_projection)?;
      if let Some(path) = &self.package_path {
        package.append_assets_to_path(path)?;
      }
    }

    if !commands_change_document && !asset_merge.metadata_changed {
      self.projection.assets = predicted_projection.assets;
      if asset_merge.any_changed {
        self.package = staged_package;
      }
      return Ok(EditorCommitResult {
        transaction_id,
        base_frontier: current_projection_frontier.clone(),
        new_frontier: current_projection_frontier,
        events: Vec::new(),
      });
    }

    let mut batch_projection_before = self.projection.clone();
    let batch_frontier_before = self.doc.state_frontiers();
    let batch_vv_before = self.doc.state_vv();
    flowstate_document::touch_document_metadata(&self.doc).context("updating canonical document metadata for grouped editor transaction")?;
    let mut working_projection = self.projection.clone();

    for (command_ix, command) in commands.iter().enumerate() {
      if editor_semantic_command_is_noop(command) {
        continue;
      }
      let applied = apply_editor_semantic_command(&self.doc, &working_projection, command)
        .with_context(|| format!("applying editor command {command_ix} inside grouped Loro transaction"))?;
      if !applied {
        anyhow::bail!("editor command {command_ix} was rejected after successful preflight: {command:?}");
      }
      let replayed = replay_semantic_command_on_projection(&mut working_projection, command);
      debug_assert!(replayed, "preflighted editor command must replay on the working projection");
    }
    working_projection.assets = predicted_projection.assets.clone();
    if asset_merge.metadata_changed {
      flowstate_document::loro_import::import_assets(&self.doc, &working_projection)
        .context("recording asset metadata in grouped editor transaction")?;
      refresh_image_asset_metadata(&self.doc).context("refreshing image asset metadata in grouped editor transaction")?;
    }
    let undo_selection = selection_after.and_then(|selection| self.undo_selection_for_projection(&working_projection, selection));
    self.set_pending_undo_selection(undo_selection)?;
    self.doc.commit();
    self.fidelity_frontier_transition("editor-transaction", &batch_frontier_before, &batch_vv_before);

    let mut invalidation = ProjectionInvalidation::full_rebuild(
      batch_projection_before.frontier.clone(),
      self.doc.state_frontiers().encode(),
      "editor_command_batch_projection",
    );
    invalidation.changed_assets = asset_merge.changed_asset_ids;
    self.merge_subscription_invalidation(&mut invalidation);

    // §P2a: collect projection defects so the transaction's repair pass can fold
    // its canonical fix into the same LocalUpdate stream the peers already receive.
    let mut transaction_defects: Vec<ProjectionDefect> = Vec::new();
    let mut authoritative_projection = match document_from_loro_with_defects(&self.doc) {
      Ok((projection, defects)) => {
        transaction_defects = defects;
        projection
      },
      Err(error) => {
        tracing::error!(%error, "canonical projection materialization failed after committed editor transaction; using the prevalidated projection");
        let mut projection = predicted_projection.clone();
        projection.frontier = self.doc.state_frontiers().encode();
        projection
      },
    };
    authoritative_projection.assets = predicted_projection.assets;
    authoritative_projection.theme = self.projection.theme.clone();
    self.projection = authoritative_projection;
    self.projection_index = ProjectionRuntimeIndex::from_projection(&self.projection);
    self.bump_runtime_epoch();
    if asset_merge.any_changed {
      self.package = staged_package;
    }
    if let Err(error) = self.record_undo_checkpoint() {
      tracing::error!(%error, "recording undo checkpoint failed after committed editor transaction");
    }

    debug_assert_eq!(
      self.projection.ids.paragraph_ids, predicted_projection.ids.paragraph_ids,
      "canonical paragraph identities diverged from preflighted editor transaction {transaction_id}: {commands:?}",
    );
    debug_assert_eq!(
      self.projection.ids.block_ids, predicted_projection.ids.block_ids,
      "canonical block identities diverged from preflighted editor transaction {transaction_id}: {commands:?}",
    );

    let mut events = Vec::new();
    match self.local_update_bytes(&batch_vv_before) {
      Ok(update) if !update.is_empty() => {
        if let Err(error) = self.persist_update_segment(batch_frontier_before, batch_vv_before, update.clone()) {
          tracing::error!(%error, "persisting committed editor transaction update segment failed");
        }
        events.push(RuntimeEvent::LocalUpdate {
          bytes: update,
          frontier: self.doc.state_frontiers().encode(),
          version_vector: self.doc.state_vv().encode(),
        });
      },
      Ok(_) => {},
      Err(error) => {
        tracing::error!(%error, "exporting committed editor transaction update failed; later synchronization must recover it");
      },
    }

    invalidation
      .frontier_after
      .clone_from(&self.projection.frontier);
    // An incremental patch is only ever an optimization over shipping the full
    // projection, so it MUST reproduce the authoritative projection exactly. A
    // lossy/incomplete diff (e.g. a split-then-insert batch whose diff drops the
    // inserted text) would silently lose content on the editor side. Verify the
    // patch reproduces `self.projection` before trusting it; otherwise fall back
    // to a full snapshot. This makes patch-path data loss structurally impossible
    // regardless of any gap in `projection_patches_between`.
    let batch_before_frontier = batch_projection_before.frontier.clone();
    let verified_patch = match projection_patches_between(&batch_projection_before, &self.projection) {
      Some(patches) => {
        let batch = ProjectionPatchBatch {
          transaction_id,
          base_frontier: batch_before_frontier.clone(),
          new_frontier: self.projection.frontier.clone(),
          patches,
        };
        // Verify by replaying the patch onto the pre-batch projection IN PLACE.
        // Its content is not needed afterward (only its frontier, saved above),
        // so this avoids a second full-projection clone per transaction.
        match apply_projection_patch_batch(&mut batch_projection_before, &batch) {
          Ok(()) if projections_semantically_equal(&batch_projection_before, &self.projection) => Some(batch),
          outcome => {
            fidelity::event(FidelityClass::Projection, "patch-verify-fallback", || {
              format!("editor-transaction {transaction_id}: incremental patch did not reproduce the authoritative projection ({outcome:?}); emitting full projection")
            });
            None
          },
        }
      },
      None => None,
    };
    if let Some(batch) = verified_patch {
      events.push(RuntimeEvent::ProjectionPatched {
        batch,
        invalidation,
        version_vector: self.doc.state_vv().encode(),
      });
    } else {
      events.push(RuntimeEvent::ProjectionUpdated {
        document: Box::new(self.projection.clone()),
        invalidation,
        frontier: self.projection.frontier.clone(),
        version_vector: self.doc.state_vv().encode(),
      });
    }

    // §P2a: repair any malformed canonical state this transaction surfaced. The
    // repair commits under the `repair` origin and, unlike the refresh path, its
    // `LocalUpdate` + `ProjectionUpdated` events are surfaced to the caller so
    // peers receive the repair immediately (in addition to persistence).
    if !transaction_defects.is_empty() {
      match self.schedule_projection_repairs(transaction_defects) {
        Ok(repair_events) => events.extend(repair_events),
        Err(error) => tracing::error!(%error, "scheduling projection repairs after committed editor transaction failed"),
      }
    }

    Ok(EditorCommitResult {
      transaction_id,
      base_frontier: batch_before_frontier,
      new_frontier: self.projection.frontier.clone(),
      events,
    })
  }

  pub fn command(&mut self, command: SemanticCommand) -> Result<Vec<RuntimeEvent>> {
    let restore_undo_selection = matches!(&command, SemanticCommand::Undo | SemanticCommand::Redo);
    let before_projection = self.projection.clone();
    let before_body = body_text(&self.doc).to_string();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let mutates_document = match &command {
      SemanticCommand::InsertText { text, .. } => !text.is_empty(),
      SemanticCommand::DeleteRange { unicode_len, .. } => *unicode_len > 0,
      SemanticCommand::OpenRevision { .. } | SemanticCommand::ForkRevision { .. } | SemanticCommand::Undo | SemanticCommand::Redo => false,
      _ => true,
    };
    if mutates_document {
      flowstate_document::touch_document_metadata(&self.doc).context("updating canonical document metadata for semantic command")?;
    }
    #[allow(clippy::needless_late_init, reason = "assigned across match arms that interleave with diverging early-return arms")]
    let projection_invalidation;
    match command {
      SemanticCommand::InsertText { unicode_index, text, styles } => {
        if text.is_empty() {
          return Ok(Vec::new());
        }
        let body = body_text(&self.doc);
        let newline_boundaries = inserted_newline_boundaries(unicode_index, &text);
        body
          .insert(unicode_index, &text)
          .context("inserting text into Loro body flow")?;
        let inserted_len = text.chars().count();
        if inserted_len > 0 {
          mark_run_styles(&body, unicode_index..unicode_index + inserted_len, styles).context("marking inserted run styles")?;
        }
        repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &newline_boundaries, "semantic_insert_text")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_text(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_index, inserted_len);
      },
      SemanticCommand::DeleteRange { unicode_index, unicode_len } => {
        // §5 sentinel protection (preflight): clamp/reject any range that would
        // delete the boundary-0 sentinel newline before mutating the body.
        let Some((unicode_index, unicode_len)) = sentinel_protected_delete_range(unicode_index, unicode_len) else {
          return Ok(Vec::new());
        };
        let body = body_text(&self.doc);
        body
          .delete(unicode_index, unicode_len)
          .context("deleting text from Loro body flow")?;
        // §5: drop object blocks whose U+FFFC placeholder this delete removed, in
        // the same transaction, so they never linger as unresolved-anchor records.
        prune_orphaned_body_object_blocks(&self.doc, &body)?;
        repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &[], "semantic_delete_range")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_text(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_index, unicode_len);
      },
      SemanticCommand::SplitParagraph {
        unicode_index,
        inherited_style,
      } => {
        let body = body_text(&self.doc);
        body
          .insert(unicode_index, "\n")
          .context("splitting Loro body paragraph")?;
        body
          .mark(
            unicode_index..unicode_index + 1,
            MARK_PARAGRAPH_STYLE,
            paragraph_style_value(inherited_style),
          )
          .context("marking split paragraph boundary")?;
        repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &[unicode_index], "semantic_split_paragraph")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_text(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_index, 1);
      },
      SemanticCommand::SetParagraphStyle {
        boundary_unicode_index,
        style,
      } => {
        let body = body_text(&self.doc);
        body
          .mark(
            boundary_unicode_index..boundary_unicode_index + 1,
            MARK_PARAGRAPH_STYLE,
            paragraph_style_value(style),
          )
          .context("marking paragraph style in Loro body flow")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_style(from_frontier.encode(), self.doc.state_frontiers().encode(), boundary_unicode_index, 1);
      },
      SemanticCommand::SetRunStyles { unicode_range, styles } => {
        if unicode_range.is_empty() {
          return Ok(Vec::new());
        }
        let unicode_start = unicode_range.start;
        let unicode_len = unicode_range.end.saturating_sub(unicode_range.start);
        mark_run_styles(&body_text(&self.doc), unicode_range, styles).context("marking run styles in Loro body flow")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_style(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_start, unicode_len);
      },
      SemanticCommand::InsertImage {
        unicode_index,
        asset_id,
        alt_text,
        caption,
        sizing,
        alignment,
      } => {
        insert_image_block(&self.doc, unicode_index, asset_id, &alt_text, caption.as_deref(), sizing, alignment)
          .context("inserting image block into Loro document")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_object(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_index, "image");
      },
      SemanticCommand::InsertEquation {
        unicode_index,
        source,
        display,
      } => {
        insert_equation_block(&self.doc, unicode_index, &source, display).context("inserting equation block into Loro document")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_object(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_index, "equation");
      },
      SemanticCommand::InsertTable {
        unicode_index,
        rows,
        columns,
        column_widths,
        header_row,
      } => {
        insert_table_block(&self.doc, unicode_index, rows, columns, &column_widths, header_row)
          .context("inserting table block into Loro document")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation =
          ProjectionInvalidation::body_object(from_frontier.encode(), self.doc.state_frontiers().encode(), unicode_index, "table");
      },
      SemanticCommand::OpenRevision { revision_id } => {
        let document = self.revision_projection(revision_id)?;
        return Ok(vec![RuntimeEvent::RevisionOpened {
          revision_id,
          document: Box::new(document),
        }]);
      },
      SemanticCommand::ForkRevision { revision_id } => {
        let (document, package) = self.fork_revision(revision_id)?;
        return Ok(vec![RuntimeEvent::RevisionForked {
          revision_id,
          document: Box::new(document),
          package: Box::new(package),
        }]);
      },
      SemanticCommand::Undo => {
        let applied = self.undo.undo().context("applying Loro undo")?;
        // §fidelity: record the undo's frontier transition and assert it only
        // introduced local-peer ops (remote-origin ops are excluded from undo).
        if fidelity::enabled() {
          fidelity::event(FidelityClass::Undo, "undo", || {
            format!("applied={applied} frontier {:?} -> {:?}", from_frontier.encode(), self.doc.state_frontiers().encode())
          });
          if applied {
            self.fidelity_check_undo_local_only("undo", &from_vv);
          }
        }
        if !applied {
          return Ok(Vec::new());
        }
        projection_invalidation = ProjectionInvalidation {
          frontier_before: from_frontier.encode(),
          frontier_after: self.doc.state_frontiers().encode(),
          changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
          ..ProjectionInvalidation::default()
        };
      },
      SemanticCommand::Redo => {
        let applied = self.undo.redo().context("applying Loro redo")?;
        // §fidelity: record the redo's frontier transition and assert it only
        // introduced local-peer ops (remote-origin ops are excluded from redo).
        if fidelity::enabled() {
          fidelity::event(FidelityClass::Undo, "redo", || {
            format!("applied={applied} frontier {:?} -> {:?}", from_frontier.encode(), self.doc.state_frontiers().encode())
          });
          if applied {
            self.fidelity_check_undo_local_only("redo", &from_vv);
          }
        }
        if !applied {
          return Ok(Vec::new());
        }
        projection_invalidation = ProjectionInvalidation {
          frontier_before: from_frontier.encode(),
          frontier_after: self.doc.state_frontiers().encode(),
          changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
          ..ProjectionInvalidation::default()
        };
      },
    }
    let mut projection_invalidation = projection_invalidation;
    self.merge_subscription_invalidation(&mut projection_invalidation);
    let mut events = self.events_after_local_change(from_frontier, from_vv, projection_invalidation.clone(), false)?;
    let after_body = body_text(&self.doc).to_string();
    if let Some(patches) = remote_body_projection_patches(&before_projection, &before_body, &after_body, &self.doc, &projection_invalidation) {
      self.apply_projection_patch_set(&patches);
      self.projection.frontier = self.doc.state_frontiers().encode();
      events.push(self.projection_patched_event(patches, projection_invalidation));
    } else {
      self.refresh_projection()?;
      let reason = if restore_undo_selection {
        "undo_redo_structural_projection_fallback"
      } else {
        "semantic_command_structural_projection_fallback"
      };
      events.push(self.projection_change_event(
        &before_projection,
        ProjectionInvalidation::full_rebuild(projection_invalidation.frontier_before, projection_invalidation.frontier_after, reason),
      )?);
    }
    if restore_undo_selection && let Some(snapshot) = self.take_restored_undo_selection() {
      if let Some(selection) = self.resolve_undo_selection(&snapshot) {
        events.push(RuntimeEvent::SelectionRestored { selection });
      } else if let Ok(mut state) = self.undo_selection.lock() {
        state.restored_selection = Some(snapshot);
      }
    }
    Ok(events)
  }

  fn resolve_undo_selection(&mut self, snapshot: &UndoSelectionSnapshot) -> Option<EditorSelection> {
    // §16: restore the stored affinity onto the rebuilt selection. Gravity is
    // not persisted in the undo snapshot, so it resolves to neutral.
    // §24: `&mut self` so the cursor resolutions can memoize through the index's
    // cursor cache without interior mutability (this stays on the actor's
    // single-threaded `&mut self` command flow).
    Some(EditorSelection {
      anchor: self.resolve_undo_cursor(&snapshot.anchor_cursor)?,
      head: self.resolve_undo_cursor(&snapshot.head_cursor)?,
      anchor_affinity: gpui_affinity_from_undo(snapshot.anchor_affinity),
      head_affinity: gpui_affinity_from_undo(snapshot.head_affinity),
      anchor_gravity: gpui_flowtext::VisualGravity::Neutral,
      head_gravity: gpui_flowtext::VisualGravity::Neutral,
    })
  }

  fn resolve_undo_cursor(&mut self, encoded: &[u8]) -> Option<DocumentOffset> {
    // §24 cursor resolution cache: memoize the (expensive) `get_cursor_pos`
    // resolution keyed by the encoded cursor bytes. The cache is cleared on every
    // projection rebuild/incremental update, so a hit always reflects the current
    // projection.
    if let Some(offset) = self.projection_index.cursor_resolution_cache.get(encoded) {
      return Some(*offset);
    }
    let cursor = Cursor::decode(encoded).ok()?;
    let body = body_text(&self.doc);
    if cursor.container != body.id() {
      return None;
    }
    let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
    let unicode = resolved_cursor_boundary_unicode(&body, &resolved)?;
    let offset = self
      .projection_index
      .offset_for_body_unicode(&self.projection, unicode)?;
    self
      .projection_index
      .cursor_resolution_cache
      .insert(encoded.to_vec(), offset);
    Some(offset)
  }

  pub fn revision_projection(&self, revision_id: u128) -> Result<DocumentProjection> {
    let revision_doc = self
      .package
      .as_ref()
      .context("cannot open revision without a package-backed runtime")?
      .load_revision_loro_doc(revision_id)
      .context("loading revision Loro snapshot")?;
    let mut document = document_from_loro(&revision_doc).context("projecting revision document")?;
    if let Some(package) = &self.package {
      attach_package_assets(&mut document, package);
    }
    Ok(document)
  }

  pub fn fork_revision(&self, revision_id: u128) -> Result<(DocumentProjection, DocumentPackage)> {
    let package = self
      .package
      .as_ref()
      .context("cannot fork revision without a package-backed runtime")?;
    let revision_doc = package
      .load_revision_loro_doc(revision_id)
      .context("loading revision Loro snapshot for fork")?;
    let forked_doc = revision_doc.fork();
    flowstate_document::fork_document_lineage(&forked_doc).context("assigning forked document lineage")?;
    let forked_package = DocumentPackage::from_loro_snapshot_with_assets(&forked_doc, "Forked revision", package.assets.clone())
      .context("creating forked revision package")?;
    let mut document = document_from_loro(&forked_doc).context("projecting forked revision")?;
    attach_package_assets(&mut document, &forked_package);
    Ok((document, forked_package))
  }

  pub fn import_remote_update(&mut self, bytes: &[u8]) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    // §fidelity: capture the pre-import version only when tracing so a disabled
    // build pays nothing; used to assert the import advanced (never regressed) the
    // canonical frontier below.
    let fidelity_before_vv = fidelity::enabled().then(|| self.doc.state_vv());
    let status = self
      .doc
      .import_with(bytes, "remote")
      .context("importing remote Loro update")?;
    let after_remote_vv = self.doc.state_vv();
    if let Some(before_vv) = &fidelity_before_vv {
      self.fidelity_frontier_transition("import", &from_frontier, before_vv);
    }
    let repair_update = if status.pending.is_none() && repair_missing_paragraph_style_marks(&self.doc)? {
      self.local_update_bytes(&after_remote_vv)?
    } else {
      Vec::new()
    };
    let frontier_after = self.doc.state_frontiers();
    let version_vector = self.doc.state_vv();
    // §22: when the import is missing dependencies, surface the pending version
    // range so the UI session can trigger immediate update pull/anti-entropy
    // rather than waiting for the periodic digest. The range is both logged here
    // and carried on `RemoteUpdateApplied { pending }` below.
    if let Some(missing) = Self::missing_dependency_request(&status) {
      tracing::debug!(?missing, "remote Loro import is missing dependencies; requesting anti-entropy pull");
    }
    let mut events = vec![RuntimeEvent::RemoteUpdateApplied {
      pending: status.pending.clone(),
      frontier: frontier_after.encode(),
      version_vector: version_vector.encode(),
    }];
    if !repair_update.is_empty() {
      events.push(RuntimeEvent::LocalUpdate {
        bytes: repair_update,
        frontier: frontier_after.encode(),
        version_vector: version_vector.encode(),
      });
    }
    let frontier_before = from_frontier.encode();
    let frontier_after = frontier_after.encode();
    if status.pending.is_none() {
      let mut invalidation = ProjectionInvalidation {
        frontier_before,
        frontier_after,
        changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
        ..ProjectionInvalidation::default()
      };
      self.merge_subscription_invalidation(&mut invalidation);
      let live_starts = paragraph_unicode_starts_from_body(&body_text(&self.doc).to_string());
      // Structural-change backstop: `live_starts.len()` is the POST-import paragraph
      // count (one per body `\n`). If it differs from the pre-import projection count, a
      // paragraph boundary was inserted or DELETED by the merge. The `inserted_structure`
      // flag catches inserted boundaries directly, but a deleted boundary is only inferred
      // from `deleted_range_contains_structure`, which maps the delete position against the
      // STALE pre-import boundary index and can miss it after concurrent inserts shift
      // coordinates — leaving the incremental path to keep a paragraph the authoritative
      // rebuild dropped. A count mismatch is an unambiguous, coordinate-free signal to take
      // the object-aware full rebuild instead.
      if live_starts.len() != self.projection.paragraphs.len() {
        invalidation.rebuild_required = true;
        invalidation.fallback_reason = Some("body_paragraph_count_changed");
      }
      let touched_paragraphs = self.projection_index.paragraphs_for_changed_ranges(
        &invalidation.changed_text_ranges,
        self.projection.paragraphs.len(),
        &live_starts,
      );
      if let Some(patches) = remote_nonstructural_projection_patches(&self.projection, &self.doc, &invalidation, &touched_paragraphs) {
        self.apply_projection_patch_set(&patches);
        self.projection.frontier = self.doc.state_frontiers().encode();
        events.push(self.projection_patched_event(patches, invalidation));
      } else {
        let before_projection = self.projection.clone();
        self.refresh_projection()?;
        events.push(self.projection_change_event(&before_projection, invalidation)?);
      }
    } else {
      let mut invalidation = ProjectionInvalidation::full_rebuild(frontier_before, frontier_after, "remote_update_pending_projection_fallback");
      self.merge_subscription_invalidation(&mut invalidation);
      self.refresh_projection()?;
      events.push(self.projection_event(invalidation)?);
    }
    if status.pending.is_none() {
      // The remote update has already merged into the canonical Loro doc above;
      // durability (revision sync + update-segment persistence) is a SECONDARY
      // concern and MUST NOT be able to discard a successful merge. Propagating a
      // persist error here (`?`) previously made the caller drop the whole import
      // (session_io) so the peer never projected the remote edits/presence — a
      // one-directional-sync failure. Log and keep the merge in memory instead;
      // the segment persistence self-heals (re-snapshots) in `persist_update_segment`.
      if let Some(package) = &mut self.package
        && let Err(error) = package.sync_revisions_from_loro(&self.doc)
      {
        tracing::error!(%error, "syncing revisions after remote import failed; kept the merged update in memory");
      }
      if let Err(error) = self.persist_update_from_last_frontier() {
        tracing::error!(%error, "persisting merged remote update failed; kept the merge in memory (durability degraded until the next successful save)");
        fidelity::event(FidelityClass::Persistence, "remote-persist-failed", || format!("{error:#}"));
      }
    }
    Ok(events)
  }

  fn projection_event(&self, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
    self.record_projection_fallback(&invalidation);
    Ok(RuntimeEvent::ProjectionUpdated {
      document: Box::new(self.projection_snapshot()?),
      invalidation,
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    })
  }

  pub fn export_updates_for(&self, remote_vv: &VersionVector) -> Result<Vec<u8>> {
    self
      .doc
      .export(ExportMode::updates(remote_vv))
      .context("exporting Loro updates for anti-entropy")
  }

  pub fn missing_dependency_request(status: &ImportStatus) -> Option<&VersionRange> {
    status.pending.as_ref()
  }

  pub fn save_package(&mut self) -> io::Result<()> {
    let Some(package) = &self.package else {
      return Ok(());
    };
    let Some(path) = &self.package_path else {
      return Ok(());
    };
    package.write(path)?;
    self.package_journal_prepared = true;
    Ok(())
  }

  fn projection_change_event(&self, before: &DocumentProjection, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
    if let Some(patches) = projection_patches_between(before, &self.projection) {
      self.record_projection_fallback(&invalidation);
      return Ok(RuntimeEvent::ProjectionPatched {
        batch: ProjectionPatchBatch {
          transaction_id: uuid::Uuid::new_v4().as_u128(),
          base_frontier: before.frontier.clone(),
          new_frontier: self.doc.state_frontiers().encode(),
          patches,
        },
        invalidation,
        version_vector: self.doc.state_vv().encode(),
      });
    }
    self.projection_event(ProjectionInvalidation::full_rebuild(
      invalidation.frontier_before,
      invalidation.frontier_after,
      "projection_diff_ambiguous",
    ))
  }

  fn projection_patched_event(&self, patches: Vec<flowstate_document::ProjectionPatch>, invalidation: ProjectionInvalidation) -> RuntimeEvent {
    // §fidelity: the single choke point for every incrementally-patched projection
    // emission (local semantic commands + remote non-structural imports). Verify
    // the maintained projection still matches a fresh full rebuild.
    self.fidelity_verify_incremental_projection("projection-patched");
    RuntimeEvent::ProjectionPatched {
      batch: ProjectionPatchBatch {
        transaction_id: uuid::Uuid::new_v4().as_u128(),
        base_frontier: invalidation.frontier_before.clone(),
        new_frontier: self.doc.state_frontiers().encode(),
        patches,
      },
      invalidation,
      version_vector: self.doc.state_vv().encode(),
    }
  }

  fn record_projection_fallback(&self, invalidation: &ProjectionInvalidation) {
    if !invalidation.rebuild_required {
      return;
    }
    let reason = invalidation
      .fallback_reason
      .unwrap_or("unspecified_projection_fallback");
    if let Ok(mut counts) = self.projection_fallback_counts.lock() {
      *counts.entry(reason.to_string()).or_default() += 1;
    }
    fidelity::event(FidelityClass::Projection, "full-rebuild-fallback", || format!("reason={reason}"));
    tracing::warn!(reason, "Flowstate projection used a full rebuild fallback");
  }

  /// §fidelity: when heavy tracing is enabled, verify the incrementally-maintained
  /// `self.projection` still equals a fresh full projection built from canonical
  /// Loro state via [`document_from_loro`]. A mismatch means an incremental patch
  /// diverged from the authoritative materializer (kind
  /// `incremental-vs-full-divergence`). Read-only; cheap firehose tracing does not
  /// run this full reprojection because it perturbs large-document profiles.
  fn fidelity_verify_incremental_projection(&self, context: &str) {
    if !fidelity::expensive_checks_enabled() {
      return;
    }
    match document_from_loro(&self.doc) {
      Ok(fresh) => {
        fidelity::check(
          projections_semantically_equal(&self.projection, &fresh),
          FidelityClass::Projection,
          "incremental-vs-full-divergence",
          || {
            format!(
              "{context}: incremental projection diverged from full rebuild [first_divergence: {}] (incremental_paragraphs={}, full_paragraphs={}, incremental_blocks={}, full_blocks={})",
              first_projection_divergence(&self.projection, &fresh),
              self.projection.paragraphs.len(),
              fresh.paragraphs.len(),
              self.projection.blocks.len(),
              fresh.blocks.len(),
            )
          },
        );
      },
      Err(error) => fidelity::event(FidelityClass::Projection, "full-rebuild-verify-error", || format!("{context}: {error}")),
    }
  }

  /// §fidelity: log a canonical frontier transition and assert the version only
  /// advances (never regresses). Local edits and remote imports are monotone
  /// merges, so `before_vv <= after_vv`; a regression (kind `frontier-regressed`)
  /// signals canonical-state corruption. No-op (one atomic load) when off.
  fn fidelity_frontier_transition(&self, context: &'static str, before_frontier: &Frontiers, before_vv: &VersionVector) {
    if !fidelity::enabled() {
      return;
    }
    let after_frontier = self.doc.state_frontiers();
    let after_vv = self.doc.state_vv();
    fidelity::event(FidelityClass::Frontier, context, || {
      format!("frontier {:?} -> {:?}", before_frontier.encode(), after_frontier.encode())
    });
    fidelity::check(
      matches!(before_vv.partial_cmp(&after_vv), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)),
      FidelityClass::Frontier,
      "frontier-regressed",
      || {
        format!(
          "{context}: canonical version regressed (before_vv={:?}, after_vv={:?})",
          before_vv.encode(),
          after_vv.encode()
        )
      },
    );
  }

  /// §fidelity: assert an undo/redo introduced only local-peer operations. Remote-
  /// origin changes are excluded from the undo stack
  /// (`add_exclude_origin_prefix("remote")`), so an undo/redo must never advance a
  /// foreign peer's version. Violation kind `remote-origin-op-in-undo`. No-op off.
  fn fidelity_check_undo_local_only(&self, op: &str, before_vv: &VersionVector) {
    if !fidelity::enabled() {
      return;
    }
    let after_vv = self.doc.state_vv();
    let local_peer = self.doc.peer_id();
    let foreign: Vec<(u64, i32)> = after_vv
      .iter()
      .filter_map(|(peer, counter)| {
        let before = before_vv.get(peer).copied().unwrap_or(0);
        (*peer != local_peer && *counter > before).then_some((*peer, *counter - before))
      })
      .collect();
    fidelity::check(
      foreign.is_empty(),
      FidelityClass::Undo,
      "remote-origin-op-in-undo",
      || format!("{op} advanced non-local peers {foreign:?} (local_peer={local_peer}); remote-origin ops must be excluded from undo"),
    );
  }

  pub fn projection_fallback_stats(&self) -> ProjectionFallbackStats {
    let by_reason = self
      .projection_fallback_counts
      .lock()
      .map(|counts| counts.clone())
      .unwrap_or_default();
    ProjectionFallbackStats {
      total: by_reason.values().copied().sum(),
      by_reason,
    }
  }

  /// §P2a: telemetry snapshot of projection-defect repair attempts, keyed by
  /// defect `stable_key`. Mirrors [`Self::projection_fallback_stats`].
  pub fn projection_repair_stats(&self) -> ProjectionFallbackStats {
    let by_reason = self
      .projection_repair_counts
      .lock()
      .map(|counts| counts.clone())
      .unwrap_or_default();
    ProjectionFallbackStats {
      total: by_reason.values().copied().sum(),
      by_reason,
    }
  }

  /// §P2a: record and return the repair-attempt count for `stable_key`.
  fn record_projection_repair_attempt(&self, stable_key: &str) -> u64 {
    if let Ok(mut counts) = self.projection_repair_counts.lock() {
      let entry = counts.entry(stable_key.to_string()).or_default();
      *entry += 1;
      *entry
    } else {
      // A poisoned lock cannot account attempts; treat as over-cap so we quarantine
      // rather than risk an unbounded repair loop.
      PROJECTION_REPAIR_ATTEMPT_CAP + 1
    }
  }

  /// §P2a: apply the idempotent canonical repair for each reported projection
  /// defect, then commit the batch under the `repair` origin, re-project, persist
  /// the update segment, and return the `LocalUpdate` (+ `ProjectionUpdated`)
  /// events so peers receive the repair.
  ///
  /// Defects are deduplicated by `stable_key` and capped at
  /// [`PROJECTION_REPAIR_ATTEMPT_CAP`] attempts per key: a defect that persists
  /// across repair passes is logged and quarantined (left as the deterministic
  /// projection) rather than retried forever. Convergence under concurrent
  /// multi-peer repair is guaranteed by the check-before-write, stable-key-keyed
  /// mutations in [`projection_repair`] plus Loro map/mark LWW semantics.
  ///
  /// Re-entrant calls (the inner refresh re-projecting after a repair) are
  /// no-ops via the `repairing_projection_defects` guard.
  pub fn schedule_projection_repairs(&mut self, defects: Vec<ProjectionDefect>) -> Result<Vec<RuntimeEvent>> {
    if self.repairing_projection_defects || defects.is_empty() {
      return Ok(Vec::new());
    }
    let mut seen = FxHashSet::default();
    let mut actionable = Vec::new();
    for defect in defects {
      let stable_key = defect.stable_key();
      if !seen.insert(stable_key.clone()) {
        continue;
      }
      let attempts = self.record_projection_repair_attempt(&stable_key);
      if attempts > PROJECTION_REPAIR_ATTEMPT_CAP {
        tracing::error!(
          stable_key = %stable_key,
          class = defect.class(),
          attempts,
          cap = PROJECTION_REPAIR_ATTEMPT_CAP,
          "projection defect exceeded repair attempt cap; quarantining (leaving deterministic projection)"
        );
        continue;
      }
      // §fidelity: record each defect queued for canonical repair, with its class,
      // stable key, and this-pass attempt count (bounded by the attempt cap).
      fidelity::event(fidelity_class_for_defect(&defect), "repair-scheduled", || {
        format!("class={} key={stable_key} attempt={attempts}", defect.class())
      });
      actionable.push(defect);
    }
    if actionable.is_empty() {
      return Ok(Vec::new());
    }

    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    self.repairing_projection_defects = true;
    let mut applied = 0_usize;
    for defect in &actionable {
      match projection_repair::apply_projection_repair(&self.doc, defect) {
        Ok(true) => applied += 1,
        Ok(false) => {},
        Err(error) => tracing::error!(%error, stable_key = %defect.stable_key(), class = defect.class(), "applying projection defect repair failed"),
      }
    }
    if applied == 0 {
      self.repairing_projection_defects = false;
      return Ok(Vec::new());
    }

    // Commit the whole repair batch atomically under the dedicated origin so it is
    // excluded from undo and identifiable by peers.
    self.doc.set_next_commit_origin(REPAIR_ORIGIN);
    self.doc.commit();
    // Re-project onto the repaired canonical state. The guard keeps this refresh
    // from scheduling another (recursive) repair pass; the next external refresh
    // re-checks and, per the attempt cap, either converges to zero defects or
    // quarantines the residual.
    let refresh_result = self.refresh_projection();
    self.repairing_projection_defects = false;
    refresh_result?;

    // §fidelity: a repair pass must make progress — re-projecting the repaired
    // canonical state must not surface MORE defects than it started with. A
    // genuinely repairable defect count drops; an unrepairable one is bounded by
    // the per-key attempt cap (`PROJECTION_REPAIR_ATTEMPT_CAP`) rather than
    // growing (kind `repair-not-converging`). Gated so it costs nothing when off.
    if fidelity::enabled() {
      let scheduled = actionable.len();
      let remaining = document_from_loro_with_defects(&self.doc)
        .map(|(_, defects)| defects.len())
        .unwrap_or(0);
      fidelity::check(
        remaining <= scheduled,
        FidelityClass::Structure,
        "repair-not-converging",
        || format!("repair pass scheduled {scheduled} defect(s) but {remaining} remain after re-projection (cap={PROJECTION_REPAIR_ATTEMPT_CAP})"),
      );
    }

    // Encode the pre-repair frontier before `from_frontier` is consumed by
    // `persist_update_segment` below.
    let repair_frontier_before = from_frontier.encode();
    let mut events = Vec::new();
    match self.local_update_bytes(&from_vv) {
      Ok(update) if !update.is_empty() => {
        if let Err(error) = self.persist_update_segment(from_frontier, from_vv, update.clone()) {
          tracing::error!(%error, "persisting projection repair update segment failed");
        }
        events.push(RuntimeEvent::LocalUpdate {
          bytes: update,
          frontier: self.doc.state_frontiers().encode(),
          version_vector: self.doc.state_vv().encode(),
        });
      },
      Ok(_) => {},
      Err(error) => tracing::error!(%error, "exporting projection repair update failed; later synchronization must recover it"),
    }
    let invalidation =
      ProjectionInvalidation::full_rebuild(repair_frontier_before, self.doc.state_frontiers().encode(), "projection_defect_repair");
    events.push(RuntimeEvent::ProjectionUpdated {
      document: Box::new(self.projection.clone()),
      invalidation,
      frontier: self.projection.frontier.clone(),
      version_vector: self.doc.state_vv().encode(),
    });
    Ok(events)
  }

  fn refresh_projection(&mut self) -> Result<()> {
    let current_assets = self.projection.assets.clone();
    // §P2a: a full rebuild is where malformed canonical state surfaces; collect
    // the projection defects so we can schedule their canonical repair.
    let (mut projection, defects) =
      document_from_loro_with_defects(&self.doc).context("refreshing projection from canonical Loro state")?;
    if let Some(package) = &self.package {
      attach_package_assets(&mut projection, package);
    }
    for (id, record) in current_assets.assets {
      projection.assets.assets.insert(id, record);
    }
    projection.theme = self.projection.theme.clone();
    self.projection = projection;
    self.projection_index = ProjectionRuntimeIndex::from_projection(&self.projection);
    // §23: a full rebuild discards the meaning of any incremental summary buffered
    // before this point. Every full-rebuild path (local structural fallback, remote
    // non-structural fallback, and the pending/again-changed remote import that
    // forces a rebuild) routes through here, so bumping once here covers them all.
    self.bump_runtime_epoch();
    // §P2a: schedule canonical repair for any defects. The re-entrancy guard
    // (`repairing_projection_defects`) stops the inner re-projection that
    // `schedule_projection_repairs` performs from recursing back into a repair
    // pass. Repairs committed here are persisted (durable + anti-entropy), so
    // peers converge even though this low-level helper cannot surface the
    // repair's `LocalUpdate` event to the caller.
    if !self.repairing_projection_defects
      && !defects.is_empty()
      && let Err(error) = self.schedule_projection_repairs(defects)
    {
      tracing::error!(%error, "scheduling projection repairs after projection refresh failed");
    }
    Ok(())
  }

  /// §23: advance the runtime epoch after a full projection reset/rebuild.
  ///
  /// `merge_subscription_invalidation` reads the live epoch and discards buffered
  /// summaries stamped at an earlier epoch, so the permanent subscription stays
  /// correct without depending on synchronous drain timing around import/checkout.
  fn bump_runtime_epoch(&self) {
    let previous = self.runtime_epoch.fetch_add(1, AtomicOrdering::SeqCst);
    tracing::trace!(
      previous_epoch = previous,
      new_epoch = previous.wrapping_add(1),
      "Flowstate runtime epoch bumped after full projection rebuild"
    );
  }

  fn apply_projection_patch_set(&mut self, patches: &[ProjectionPatch]) {
    let rebuild_index = self
      .projection_index
      .update_for_patches(&self.projection, patches);
    let batch = ProjectionPatchBatch {
      transaction_id: uuid::Uuid::new_v4().as_u128(),
      base_frontier: self.projection.frontier.clone(),
      new_frontier: self.doc.state_frontiers().encode(),
      patches: patches.to_vec(),
    };
    if let Err(error) = apply_projection_patch_batch(&mut self.projection, &batch) {
      tracing::warn!(%error, "incremental runtime projection patch failed; refreshing projection");
      if let Err(error) = self.refresh_projection() {
        tracing::error!(%error, "refreshing projection after patch failure failed");
      }
      return;
    }
    if rebuild_index {
      self.projection_index = ProjectionRuntimeIndex::from_projection(&self.projection);
    }
  }

  pub fn save_package_to(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
    self.package_path = Some(path.as_ref().to_path_buf());
    self.package_journal_prepared = false;
    self.save_package()
  }

  pub fn checkpoint_package(&mut self, title: &str, path: Option<PathBuf>) -> io::Result<Vec<RuntimeEvent>> {
    let revision_id = Uuid::new_v4().as_u128();
    let revision_frontiers = self.doc.state_frontiers();
    let revision_frontier = revision_frontiers.encode();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    flowstate_document::touch_document_metadata(&self.doc).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    flowstate_document::record_revision(&self.doc, revision_id, revision_frontier, title, "Explicit save", self.author_user_id)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut revision_invalidation = ProjectionInvalidation::default();
    self.merge_subscription_invalidation(&mut revision_invalidation);
    let update = self
      .local_update_bytes(&from_vv)
      .map_err(|error| io::Error::other(error.to_string()))?;
    let mut events = Vec::new();
    if !update.is_empty() {
      let event_update = update.clone();
      self
        .persist_update_segment(from_frontier, from_vv, update)
        .map_err(|error| io::Error::other(error.to_string()))?;
      events.push(RuntimeEvent::LocalUpdate {
        bytes: event_update,
        frontier: self.doc.state_frontiers().encode(),
        version_vector: self.doc.state_vv().encode(),
      });
    }
    if self.package.is_none() {
      let package_creation_vv = self.doc.state_vv();
      self.package = Some(DocumentPackage::from_loro_snapshot_with_assets(
        &self.doc,
        title,
        assets_from_document(&self.projection),
      )?);
      let package_creation_update = self
        .local_update_bytes(&package_creation_vv)
        .map_err(|error| io::Error::other(error.to_string()))?;
      if !package_creation_update.is_empty() {
        events.push(RuntimeEvent::LocalUpdate {
          bytes: package_creation_update,
          frontier: self.doc.state_frontiers().encode(),
          version_vector: self.doc.state_vv().encode(),
        });
      }
      self.last_persisted_frontier = self.doc.state_frontiers();
      self.last_persisted_vv = self.doc.state_vv();
    }
    let Some(package) = &mut self.package else {
      return Ok(events);
    };
    package.replace_assets_from_document(&self.projection)?;
    package.rebuild_projection_cache_from_loro(&self.doc)?;
    package.rebuild_search_units_from_loro(&self.doc)?;
    package.compact_to_snapshot(&self.doc)?;
    package.create_named_revision_at_with_id(
      &self.doc,
      revision_id,
      &revision_frontiers,
      title,
      "Explicit save",
      self.author_user_id,
      Some(self.doc.peer_id() as u128),
    )?;
    if let Some(path) = path {
      self.package_path = Some(path);
      self.package_journal_prepared = false;
    }
    self.save_package()?;
    Ok(events)
  }

  pub fn package_bytes(&mut self, title: &str) -> io::Result<Vec<u8>> {
    if self.package.is_none() {
      self.package = Some(DocumentPackage::from_loro_snapshot_with_assets(
        &self.doc,
        title,
        assets_from_document(&self.projection),
      )?);
    }
    let Some(package) = &mut self.package else {
      return Err(io::Error::other("runtime package was not initialized"));
    };
    package.replace_assets_from_document(&self.projection)?;
    package.rebuild_projection_cache_from_loro(&self.doc)?;
    package.rebuild_search_units_from_loro(&self.doc)?;
    package.to_bytes()
  }

  fn events_after_local_change(
    &mut self,
    from_frontier: Frontiers,
    from_vv: VersionVector,
    invalidation: ProjectionInvalidation,
    emit_projection: bool,
  ) -> Result<Vec<RuntimeEvent>> {
    let update = self.local_update_bytes(&from_vv)?;
    let mut events = Vec::new();
    if !update.is_empty() {
      self.persist_update_segment(from_frontier, from_vv, update.clone())?;
      events.push(RuntimeEvent::LocalUpdate {
        bytes: update,
        frontier: self.doc.state_frontiers().encode(),
        version_vector: self.doc.state_vv().encode(),
      });
    }
    if emit_projection {
      events.push(self.projection_event(invalidation)?);
    }
    Ok(events)
  }

  fn local_update_bytes(&self, from_vv: &VersionVector) -> Result<Vec<u8>> {
    let mut subscribed = self
      .local_subscription_updates
      .lock()
      .map(|mut updates| std::mem::take(&mut *updates))
      .unwrap_or_default();
    if subscribed.len() == 1 {
      return Ok(subscribed.pop().unwrap_or_default());
    }
    self
      .doc
      .export(ExportMode::updates(from_vv))
      .context("exporting local Loro update fallback")
  }

  /// §23: drain the permanent subscription buffer and fold the in-epoch, in-frontier
  /// summaries into `invalidation`, filtering/processing by runtime epoch, emit-time
  /// frontier, origin, and trigger.
  ///
  /// * Epoch — summaries stamped before the most recent full rebuild are discarded.
  /// * Frontier — summaries stamped strictly ahead of `frontier_after` belong to a
  ///   later batch and are returned to the buffer for the next drain.
  /// * Origin — a remote-origin summary sets `has_remote_origin` (telemetry/bias)
  ///   without forcing a rebuild, so the incremental remote fast paths still apply.
  /// * Trigger — a checkout-triggered event forces a conservative full rebuild;
  ///   import/local triggers are left to the existing structural detection.
  fn merge_subscription_invalidation(&self, invalidation: &mut ProjectionInvalidation) {
    let summaries = self
      .subscription_events
      .lock()
      .map(|mut events| std::mem::take(&mut *events))
      .unwrap_or_default();
    let body_target = body_text(&self.doc).id().to_string();
    let current_epoch = self.runtime_epoch.load(AtomicOrdering::SeqCst);
    // §23 FRONTIER: decode the drain target once so each summary can be compared
    // causally. Empty/undecodable targets (e.g. revision bookkeeping drains) disable
    // the frontier filter and fall back to draining everything in-epoch.
    let target_frontier = if invalidation.frontier_after.is_empty() {
      None
    } else {
      Frontiers::decode(&invalidation.frontier_after).ok()
    };
    let mut deferred: Vec<SubscriptionEventSummary> = Vec::new();
    let mut has_remote_origin = false;
    for summary in summaries {
      // §23 EPOCH: drop summaries emitted before the latest full projection
      // rebuild/reset; their incremental meaning no longer maps onto the projection.
      if summary.epoch != current_epoch {
        tracing::trace!(
          summary_epoch = summary.epoch,
          current_epoch,
          origin = %summary.origin,
          trigger = %summary.triggered_by,
          "Flowstate discarding stale pre-reset Loro subscription summary",
        );
        continue;
      }
      // §23 FRONTIER: a summary stamped strictly ahead of the drain target is from a
      // later batch. Re-buffer it instead of misattributing it to this invalidation.
      if let Some(target) = target_frontier.as_ref()
        && !summary.frontier.is_empty()
        && let Ok(summary_frontier) = Frontiers::decode(&summary.frontier)
        && matches!(self.doc.cmp_frontiers(&summary_frontier, target), Ok(Some(std::cmp::Ordering::Greater)))
      {
        tracing::trace!(
          origin = %summary.origin,
          trigger = %summary.triggered_by,
          "Flowstate deferring Loro subscription summary stamped ahead of the drain frontier",
        );
        deferred.push(summary);
        continue;
      }
      // §23 ORIGIN: record remote-origin processing. The UndoManager keeps its
      // `add_exclude_origin_prefix("remote")` exclusion unchanged.
      has_remote_origin |= summary.origin.starts_with("remote");
      // §23 TRIGGER: checkout events reflect time-travel/detached state that cannot be
      // expressed as an incremental projection patch, so force a conservative rebuild.
      // Ordinary local and import triggers preserve the incremental fast paths.
      if summary.triggered_by.eq_ignore_ascii_case("checkout") {
        invalidation.rebuild_required = true;
        invalidation.fallback_reason = Some("checkout_trigger_projection_rebuild");
        tracing::debug!(origin = %summary.origin, "Flowstate forcing projection rebuild for checkout-triggered Loro event");
      }
      for change in summary.changes {
        match change {
          SubscriptionChange::Text {
            target,
            unicode_start,
            unicode_len,
            deleted_len,
            inserted_structure,
          } if target == body_target => {
            if inserted_structure
              || self
                .projection_index
                .deleted_range_contains_structure(unicode_start, deleted_len)
            {
              invalidation.rebuild_required = true;
              invalidation.fallback_reason = Some("structural_body_text_change");
            }
            invalidation
              .changed_flows
              .push(ROOT_BODY_FLOW_ID.to_string());
            invalidation.changed_text_ranges.push(ProjectionTextRange {
              flow_id: ROOT_BODY_FLOW_ID.to_string(),
              unicode_start,
              unicode_len,
            });
          },
          SubscriptionChange::Text { target, .. } => invalidation.changed_flows.push(target),
          SubscriptionChange::Map { target, keys } => classify_map_invalidation(invalidation, &target, &keys),
          SubscriptionChange::List { target } => invalidation.changed_blocks.push(target),
          SubscriptionChange::Unknown { target } => {
            invalidation.rebuild_required = true;
            invalidation.fallback_reason = Some("unknown_loro_subscription_diff");
            invalidation.changed_blocks.push(target);
          },
        }
      }
    }
    // §23 ORIGIN: surface remote-origin processing for downstream consumers/telemetry.
    invalidation.has_remote_origin |= has_remote_origin;
    // §24: fold the auxiliary projection indexes into the accumulated invalidation
    // before the sort/dedup below so any enrichment is normalized with the rest.
    self.consume_projection_indexes(invalidation);
    invalidation.changed_flows.sort();
    invalidation.changed_flows.dedup();
    invalidation.changed_blocks.sort();
    invalidation.changed_blocks.dedup();
    invalidation.changed_tables.sort();
    invalidation.changed_tables.dedup();
    invalidation.changed_assets.sort();
    invalidation.changed_assets.dedup();
    invalidation.changed_sections.sort();
    invalidation.changed_sections.dedup();
    // §23 FRONTIER: return later-batch summaries to the front of the buffer (ahead of
    // anything emitted after this drain) so ordering is preserved on the next drain.
    if !deferred.is_empty()
      && let Ok(mut events) = self.subscription_events.lock()
    {
      let newly_buffered = std::mem::replace(&mut *events, deferred);
      events.extend(newly_buffered);
    }
  }

  /// §24: consume the auxiliary projection indexes while merging an invalidation.
  ///
  /// Two functional, additive, conservative enrichments and several behavior-
  /// neutral diagnostic reads:
  /// * Asset reference index — a changed asset id (the `merge_asset_records`
  ///   path records ids in their `u128` form) is mapped to the image blocks that
  ///   embed it, whose ids are appended to `changed_blocks`.
  /// * Table row/column/cell index — a table-map diff names the Loro container,
  ///   not the projected block id, so when any table changed every indexed table
  ///   block id is surfaced into `changed_blocks`.
  ///
  /// Downstream code only ever tests `changed_blocks`/`changed_tables` for
  /// *emptiness* (never their contents), so this can only widen coverage and
  /// never drops a needed invalidation. The section/style/search reads do not
  /// mutate the invalidation; they map it onto the remaining indexes so those
  /// materialized structures have live consumers for the incremental work §24
  /// builds toward.
  fn consume_projection_indexes(&self, invalidation: &mut ProjectionInvalidation) {
    let index = &self.projection_index;

    // Asset reference index → changed_blocks.
    if !invalidation.changed_assets.is_empty() && !index.asset_refs_by_id.is_empty() {
      let mut referenced = Vec::new();
      for asset in &invalidation.changed_assets {
        if let Ok(asset_id) = asset.parse::<u128>()
          && let Some(blocks) = index.asset_refs_by_id.get(&AssetId(asset_id))
        {
          referenced.extend(blocks.iter().map(|block| block.0.to_string()));
        }
      }
      invalidation.changed_blocks.extend(referenced);
    }

    // Table row/column/cell index → changed_blocks (conservative).
    if !invalidation.changed_tables.is_empty() && !index.table_cells_by_block.is_empty() {
      let mut total_cells = 0usize;
      let mut dense_cells = 0usize;
      let mut table_blocks = Vec::new();
      for (block_id, entry) in &index.table_cells_by_block {
        table_blocks.push(block_id.0.to_string());
        total_cells = total_cells.saturating_add(entry.cells.len());
        dense_cells = dense_cells.saturating_add(entry.row_ids.len().saturating_mul(entry.column_ids.len()));
      }
      tracing::trace!(
        tables = index.table_cells_by_block.len(),
        total_cells = total_cells,
        dense_cells = dense_cells,
        "Flowstate §24 table change surfaced indexed table blocks",
      );
      invalidation.changed_blocks.extend(table_blocks);
    }

    // Search-unit and style-interval indexes (diagnostic, behavior-neutral). Gated
    // on the trace level so the O(spans) mapping never runs on the hot text-edit
    // path in production; the field reads stay compiled so the indexes are live.
    if !invalidation.changed_text_ranges.is_empty() && tracing::enabled!(tracing::Level::TRACE) {
      let touched_units = index.search_units_for_changed_ranges(&invalidation.changed_text_ranges);
      let mut paragraph_units = 0usize;
      let mut object_units = 0usize;
      let mut styled_units = 0usize;
      for unit_ix in &touched_units {
        match index
          .search_unit_spans
          .get(*unit_ix)
          .and_then(|span| span.paragraph)
        {
          Some(paragraph_ix) => {
            paragraph_units += 1;
            if index
              .run_styles_at(paragraph_ix, 0)
              .is_some_and(|styles| styles != RunStyles::default())
            {
              styled_units += 1;
            }
          },
          None => object_units += 1,
        }
      }
      tracing::trace!(
        touched_units = touched_units.len(),
        paragraph_units = paragraph_units,
        object_units = object_units,
        styled_units = styled_units,
        "Flowstate §24 changed body ranges mapped onto search-unit / style-interval indexes",
      );
    }

    // Section anchor index (diagnostic, behavior-neutral).
    if !invalidation.changed_sections.is_empty() {
      let anchored = index
        .section_anchor_by_id
        .values()
        .filter(|start| index.paragraph_metadata_by_id.contains_key(*start))
        .count();
      tracing::trace!(
        changed_sections = invalidation.changed_sections.len(),
        anchored_sections = anchored,
        sections = index.section_anchor_by_id.len(),
        "Flowstate §24 section change mapped onto section-anchor index",
      );
    }
  }

  fn persist_update_from_last_frontier(&mut self) -> Result<()> {
    let from_frontier = self.last_persisted_frontier.clone();
    let from_vv = self.last_persisted_vv.clone();
    let update = self
      .doc
      .export(ExportMode::updates(&from_vv))
      .context("exporting accepted remote Loro update for persistence")?;
    if update.is_empty() {
      return Ok(());
    }
    self.persist_update_segment(from_frontier, from_vv, update)
  }

  fn persist_update_segment(&mut self, from_frontier: Frontiers, from_vv: VersionVector, update: Vec<u8>) -> Result<()> {
    if let Some(package) = &mut self.package {
      match package.append_update_segment(&from_frontier, &from_vv, &self.doc.state_frontiers(), &self.doc.state_vv(), update) {
        Ok(_) => {
          let compacted = package.compact_update_segments_if_needed(&self.doc, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD)?;
          if let Some(path) = &self.package_path {
            if compacted.is_some() {
              package.write(path)?;
              self.package_journal_prepared = true;
            } else if self.package_journal_prepared {
              package.append_latest_update_to_prepared_path(path)?;
            } else {
              package.append_latest_update_to_path(path)?;
              self.package_journal_prepared = true;
            }
          }
        },
        Err(error) => {
          // The linear update-segment chain cannot represent this frontier
          // transition — most often a concurrent multi-head merge whose
          // `from_frontier` does not chain from the last persisted head. Rather
          // than fail the persist (and, before the caller was hardened, lose the
          // already-merged remote update), re-base the whole package onto a fresh
          // snapshot of the current doc: a snapshot is always a valid, complete
          // save. Fine-grained update-segment history folds into the snapshot;
          // document data and convergence are preserved.
          tracing::warn!(%error, "update-segment chain rejected a merge; re-basing the package onto a fresh snapshot");
          fidelity::event(FidelityClass::Persistence, "segment-chain-resnapshot", || format!("{error:#}"));
          package.compact_to_snapshot(&self.doc)?;
          if let Some(path) = &self.package_path {
            package.write(path)?;
            self.package_journal_prepared = true;
          }
        },
      }
    }
    self.last_persisted_frontier = self.doc.state_frontiers();
    self.last_persisted_vv = self.doc.state_vv();
    Ok(())
  }
}

/// Body-unicode start position of each paragraph, derived from the LIVE Loro body.
/// The body is `sentinel\n` then `paragraph\n` per boundary; every `\n` at unicode
/// position `i` starts the following paragraph at `i + 1` (the sentinel starts
/// paragraph 0). This is the POST-import coordinate space that subscription diff
/// ranges use, so mapping changed ranges against it (rather than the stale pre-import
/// projection index) picks the right paragraphs after concurrent inserts shift them.
fn paragraph_unicode_starts_from_body(body: &str) -> Vec<usize> {
  body
    .chars()
    .enumerate()
    .filter_map(|(unicode_pos, ch)| (ch == '\n').then_some(unicode_pos + 1))
    .collect()
}

fn summarize_subscription_event(event: &DiffEvent<'_>) -> SubscriptionEventSummary {
  let mut changes = Vec::new();
  for container in &event.events {
    let target = container.target.to_string();
    match &container.diff {
      Diff::Text(delta) => {
        let mut cursor = 0usize;
        for item in delta {
          match item {
            loro::TextDelta::Retain { retain, attributes } => {
              if attributes.is_some() {
                changes.push(SubscriptionChange::Text {
                  target: target.clone(),
                  unicode_start: cursor,
                  unicode_len: *retain,
                  deleted_len: 0,
                  inserted_structure: false,
                });
              }
              cursor = cursor.saturating_add(*retain);
            },
            loro::TextDelta::Insert { insert, .. } => {
              let len = insert.chars().count();
              changes.push(SubscriptionChange::Text {
                target: target.clone(),
                unicode_start: cursor,
                unicode_len: len,
                deleted_len: 0,
                inserted_structure: insert
                  .chars()
                  .any(|ch| ch == '\n' || ch == OBJECT_REPLACEMENT),
              });
              cursor = cursor.saturating_add(len);
            },
            loro::TextDelta::Delete { delete } => {
              changes.push(SubscriptionChange::Text {
                target: target.clone(),
                unicode_start: cursor,
                unicode_len: *delete,
                deleted_len: *delete,
                inserted_structure: false,
              });
            },
          }
        }
      },
      Diff::Map(delta) => changes.push(SubscriptionChange::Map {
        target,
        keys: delta.updated.keys().map(|key| key.to_string()).collect(),
      }),
      Diff::List(_) => changes.push(SubscriptionChange::List { target }),
      Diff::Tree(_) | Diff::Unknown => changes.push(SubscriptionChange::Unknown { target }),
      Diff::Counter(_) => changes.push(SubscriptionChange::Unknown { target }),
    }
  }
  SubscriptionEventSummary {
    origin: event.origin.to_string(),
    triggered_by: format!("{:?}", event.triggered_by),
    // §23: `summarize_subscription_event` stays pure — it cannot read the runtime
    // epoch or the live doc frontier. These are stamped by the root callback after
    // summarization. The unstamped defaults (epoch 0 / empty frontier) are treated
    // as "not stamped" by `merge_subscription_invalidation`.
    epoch: 0,
    frontier: Vec::new(),
    changes,
  }
}

fn classify_map_invalidation(invalidation: &mut ProjectionInvalidation, target: &str, keys: &[String]) {
  // §divergence: a change to a paragraph/block metadata record's durable identity
  // or anchor alters which id a boundary resolves to. The incremental remote patch
  // path applies content only and NEVER re-derives the paragraph_ids/block_ids
  // arrays, so on such a change the incremental projection would freeze stale ids
  // that diverge from the authoritative full rebuild (and clobber peers). Force a
  // full rebuild — which re-resolves the ids — for any id-affecting record change.
  // (Text-structural changes already force a rebuild above; this covers the
  // non-structural record writes, e.g. a peer's repaired metadata record syncing.)
  if keys
    .iter()
    .any(|key| matches!(key.as_str(), "id" | "boundary_cursor" | "start_cursor" | "anchor_cursor"))
  {
    invalidation.rebuild_required = true;
    invalidation.fallback_reason = Some("metadata_record_id_change");
  }
  if keys.iter().any(|key| {
    matches!(
      key.as_str(),
      "asset_id" | "content_hash" | "mime_type" | "byte_length" | "dimensions" | "original_name"
    )
  }) {
    invalidation.changed_assets.push(target.to_string());
  }
  if keys.iter().any(|key| {
    matches!(
      key.as_str(),
      "row_order" | "rows_by_id" | "column_order" | "columns_by_id" | "cells_by_id" | "row_span" | "column_span"
    )
  }) {
    invalidation.changed_tables.push(target.to_string());
  }
  if keys
    .iter()
    .any(|key| matches!(key.as_str(), "kind" | "flow_id" | "anchor_cursor" | "attrs" | "nested_refs"))
  {
    invalidation.changed_blocks.push(target.to_string());
  }
  if keys
    .iter()
    .any(|key| key == "section_id" || key == "sections_by_id")
  {
    invalidation.changed_sections.push(target.to_string());
  }
}

fn editor_semantic_command_is_noop(command: &EditorSemanticCommand) -> bool {
  match command {
    EditorSemanticCommand::InsertText { text, .. } => text.is_empty(),
    EditorSemanticCommand::DeleteRange { range } => range.start == range.end,
    EditorSemanticCommand::SetRunStyles { range, .. } => range.start == range.end,
    _ => false,
  }
}

fn editor_semantic_command_requires_staging_validation(command: &EditorSemanticCommand) -> bool {
  matches!(
    command,
    EditorSemanticCommand::ReplaceParagraphSpan { .. }
      | EditorSemanticCommand::InsertBlock { .. }
      | EditorSemanticCommand::DeleteBlock { .. }
      | EditorSemanticCommand::MoveBlock { .. }
      | EditorSemanticCommand::ReplaceBlock { .. }
      | EditorSemanticCommand::InsertTableRow { .. }
      | EditorSemanticCommand::DeleteTableRow { .. }
      | EditorSemanticCommand::MoveTableRow { .. }
      | EditorSemanticCommand::InsertTableColumn { .. }
      | EditorSemanticCommand::DeleteTableColumn { .. }
      | EditorSemanticCommand::MoveTableColumn { .. }
      | EditorSemanticCommand::ReplaceTableCell { .. }
      | EditorSemanticCommand::SetTableCellSpan { .. }
      | EditorSemanticCommand::ReplaceEquationSourceRange { .. }
      | EditorSemanticCommand::ReplaceImageAltText { .. }
      | EditorSemanticCommand::ReplaceImageCaption { .. }
      | EditorSemanticCommand::SetImageLayout { .. }
      | EditorSemanticCommand::SetTableColumnWidth { .. }
  )
}

/// §5 sentinel protection: the boundary-0 [`SENTINEL_NEWLINE`] anchors the first
/// paragraph and must never be deleted. Given a requested body delete of `len`
/// unicode chars starting at `start`, return the largest sub-range that leaves
/// position 0 intact, or `None` when nothing outside the sentinel remains to
/// delete. Clamping here in *preflight* keeps a malformed delete from
/// half-applying. A well-formed editor command never targets position 0 (the
/// first paragraph's text starts at unicode index 1), so the clamp only fires on
/// corruption or an explicit whole-document delete — which correctly keeps the
/// lone sentinel and drops everything after it.
fn sentinel_protected_delete_range(start: usize, len: usize) -> Option<(usize, usize)> {
  if len == 0 {
    return None;
  }
  let end = start.saturating_add(len);
  let protected_start = start.max(1);
  (end > protected_start).then_some((protected_start, end - protected_start))
}

/// §5 sentinel/object coupling: after a body text delete, an object block whose
/// U+FFFC placeholder was removed must not linger as a dangling record — it would
/// otherwise project as an `UnresolvedObjectAnchor` quarantine on every future
/// projection. Remove such body object blocks in the **same transaction** as the
/// delete so canonical state stays coherent (the paired paragraph metadata is
/// handled separately by [`repair_paragraph_metadata_after_text_flow_edit`]).
///
/// Convergent: deletion is keyed on the block's stable map key, so two peers that
/// concurrently delete the same placeholder converge on the same removed record.
/// Returns the number of blocks pruned.
fn prune_orphaned_body_object_blocks(doc: &LoroDoc, body: &loro::LoroText) -> loro::LoroResult<usize> {
  let body_snapshot = body.to_string();
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(0);
  };
  let mut pruned = 0_usize;
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "flow_id").as_deref() != Some(ROOT_BODY_FLOW_ID) {
      continue;
    }
    // Only object blocks (image/equation/table) anchor to a U+FFFC placeholder;
    // paragraph blocks are pruned by the paragraph-metadata repair path.
    match map_string_opt(&block, "kind").as_deref() {
      Some("paragraph") | None => continue,
      Some(_) => {},
    }
    if live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor").is_none() {
      blocks.delete(&key)?;
      pruned += 1;
    }
  }
  Ok(pruned)
}

/// §fidelity: boolean mirror of the test helper `assert_semantic_projection_eq`
/// (`crdt_runtime/editor_transaction_tests.rs`) — whether two projections are
/// semantically equal across identity, sections, frontier, per-paragraph
/// style/runs/text, and object (non-paragraph) blocks. Paragraph `Block`s are
/// covered by the per-paragraph comparison, so only object blocks are compared
/// structurally. Used to detect incremental-vs-full projection divergence without
/// panicking; assets and theme are intentionally excluded (mirrors the helper).
fn projections_semantically_equal(left: &DocumentProjection, right: &DocumentProjection) -> bool {
  if left.ids != right.ids || left.sections != right.sections || left.frontier != right.frontier {
    return false;
  }
  if left.paragraphs.len() != right.paragraphs.len() {
    return false;
  }
  for paragraph_ix in 0..left.paragraphs.len() {
    let left_paragraph = &left.paragraphs[paragraph_ix];
    let right_paragraph = &right.paragraphs[paragraph_ix];
    if left_paragraph.style != right_paragraph.style
      || left_paragraph.runs != right_paragraph.runs
      || flowstate_document::paragraph_text(left, paragraph_ix) != flowstate_document::paragraph_text(right, paragraph_ix)
    {
      return false;
    }
  }
  if left.blocks.len() != right.blocks.len() {
    return false;
  }
  left
    .blocks
    .iter()
    .zip(right.blocks.iter())
    .all(|(left_block, right_block)| match (left_block, right_block) {
      (Block::Paragraph(_), Block::Paragraph(_)) => true,
      _ => left_block == right_block,
    })
}

/// §divergence-diagnostic: name the FIRST concrete field where `left` (the
/// incremental projection) differs from `right` (the authoritative full rebuild),
/// so the `incremental-vs-full-divergence` event pinpoints the exact id/text
/// instead of only counts. Called only once a divergence is already known.
fn first_projection_divergence(left: &DocumentProjection, right: &DocumentProjection) -> String {
  if left.ids.document_id != right.ids.document_id {
    return format!("document_id {} != {}", left.ids.document_id, right.ids.document_id);
  }
  if left.ids.paragraph_ids.len() != right.ids.paragraph_ids.len() {
    return format!("paragraph_ids len {} != {}", left.ids.paragraph_ids.len(), right.ids.paragraph_ids.len());
  }
  for (ix, (l, r)) in left.ids.paragraph_ids.iter().zip(&right.ids.paragraph_ids).enumerate() {
    if l != r {
      return format!("paragraph_ids[{ix}] incremental={} full={} text={:?}", l.0, r.0, flowstate_document::paragraph_text(right, ix));
    }
  }
  if left.ids.block_ids.len() != right.ids.block_ids.len() {
    return format!("block_ids len {} != {}", left.ids.block_ids.len(), right.ids.block_ids.len());
  }
  for (ix, (l, r)) in left.ids.block_ids.iter().zip(&right.ids.block_ids).enumerate() {
    if l != r {
      return format!("block_ids[{ix}] incremental={} full={}", l.0, r.0);
    }
  }
  if left.sections != right.sections {
    return "sections differ".to_string();
  }
  for ix in 0..left.paragraphs.len().min(right.paragraphs.len()) {
    if flowstate_document::paragraph_text(left, ix) != flowstate_document::paragraph_text(right, ix) {
      return format!("paragraph[{ix}] text incremental={:?} full={:?}", flowstate_document::paragraph_text(left, ix), flowstate_document::paragraph_text(right, ix));
    }
    if left.paragraphs[ix].style != right.paragraphs[ix].style || left.paragraphs[ix].runs != right.paragraphs[ix].runs {
      return format!("paragraph[{ix}] style/runs differ");
    }
  }
  "blocks or other non-id content differ".to_string()
}

/// §fidelity: classify a projection defect into a fidelity event class. Paragraph
/// / block / asset identity defects carry durable ids (Identity); object-anchor
/// and paragraph-style-mark defects are structural (Structure).
fn fidelity_class_for_defect(defect: &ProjectionDefect) -> FidelityClass {
  match defect {
    ProjectionDefect::MissingParagraphMetadata { .. }
    | ProjectionDefect::MissingParagraphBlock { .. }
    | ProjectionDefect::InvalidAssetId { .. } => FidelityClass::Identity,
    ProjectionDefect::MissingParagraphStyleMark { .. }
    | ProjectionDefect::UnresolvedObjectAnchor { .. }
    | ProjectionDefect::CollidingObjectAnchors { .. }
    | ProjectionDefect::OrphanObjectPlaceholder { .. }
    | ProjectionDefect::TableTopology { .. } => FidelityClass::Structure,
  }
}

pub fn apply_editor_semantic_command(doc: &LoroDoc, projection: &DocumentProjection, command: &EditorSemanticCommand) -> Result<bool> {
  match command {
    EditorSemanticCommand::InsertText { at, text, styles } => {
      let unicode_index = projection_offset_to_loro_body_unicode_index(doc, projection, *at);
      let body = body_text(doc);
      let newline_boundaries = inserted_newline_boundaries(unicode_index, text);
      body
        .insert(unicode_index, text)
        .context("inserting projection-scoped text command into Loro body flow")?;
      let inserted_len = text.chars().count();
      if inserted_len > 0 {
        mark_run_styles(&body, unicode_index..unicode_index + inserted_len, *styles).context("marking inserted run styles")?;
      }
      repair_paragraph_metadata_after_text_flow_edit(doc, &body, &newline_boundaries, "editor_insert_text")?;
      Ok(true)
    },
    EditorSemanticCommand::DeleteRange { range } => {
      let start = projection_offset_to_loro_body_unicode_index(doc, projection, range.start);
      let end = projection_offset_to_loro_body_unicode_index(doc, projection, range.end);
      // §5 sentinel protection (preflight): clamp/reject a range that would delete
      // the boundary-0 sentinel newline before mutating the body.
      if let Some((start, len)) = sentinel_protected_delete_range(start, end.saturating_sub(start)) {
        let body = body_text(doc);
        body
          .delete(start, len)
          .context("deleting projection-scoped text range from Loro body flow")?;
        prune_orphaned_body_object_blocks(doc, &body)?;
        repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[], "editor_delete_range")?;
        return Ok(true);
      }
      Ok(false)
    },
    EditorSemanticCommand::SplitParagraph {
      at,
      source_paragraph,
      source_block,
      new_paragraph,
      new_block,
      inherited_style,
    } => {
      if projection.ids.paragraph_ids.get(at.paragraph).copied() != Some(*source_paragraph) {
        return Ok(false);
      }
      let Some(source_block_ix) = flowstate_document::block_ix_for_paragraph(projection, at.paragraph) else {
        return Ok(false);
      };
      if projection.ids.block_ids.get(source_block_ix).copied() != Some(*source_block) {
        return Ok(false);
      }
      let unicode_index = projection_offset_to_loro_body_unicode_index(doc, projection, *at);
      let body = body_text(doc);
      body
        .insert(unicode_index, "\n")
        .context("splitting paragraph in Loro body flow")?;
      body
        .mark(
          unicode_index..unicode_index + 1,
          MARK_PARAGRAPH_STYLE,
          paragraph_style_value(*inherited_style),
        )
        .context("marking split paragraph style")?;
      repair_paragraph_metadata_after_stable_split(doc, &body, unicode_index, *new_paragraph, *new_block, "editor_split_paragraph")?;
      // §fidelity: make the client-supplied vs canonical id flow for a split
      // visible. The new paragraph/block ids the editor chose are written straight
      // into canonical state here, so any later id divergence is traceable.
      fidelity::event(FidelityClass::Identity, "client-id", || {
        format!(
          "SplitParagraph@para{}.byte{}: source_paragraph={source_paragraph:?} source_block={source_block:?} client_new_paragraph={new_paragraph:?} client_new_block={new_block:?} written_at_body_unicode={unicode_index}",
          at.paragraph, at.byte
        )
      });
      Ok(true)
    },
    EditorSemanticCommand::SetParagraphStyle { paragraph, style } => {
      if let Some(paragraph_ix) = projection
        .ids
        .paragraph_ids
        .iter()
        .position(|id| id == paragraph)
      {
        let boundary = paragraph_boundary_loro_unicode_index(doc, projection, paragraph_ix);
        body_text(doc)
          .mark(boundary..boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))
          .context("marking paragraph style from editor semantic command")?;
        return Ok(true);
      }
      Ok(false)
    },
    EditorSemanticCommand::SetRunStyles { paragraph, range, styles } => {
      if let Some(paragraph_ix) = projection
        .ids
        .paragraph_ids
        .iter()
        .position(|id| id == paragraph)
      {
        let start = projection_offset_to_loro_body_unicode_index(
          doc,
          projection,
          flowstate_document::DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.start,
          },
        );
        let end = projection_offset_to_loro_body_unicode_index(
          doc,
          projection,
          flowstate_document::DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.end,
          },
        );
        if end > start {
          mark_run_styles(&body_text(doc), start..end, *styles).context("marking run styles from editor semantic command")?;
          return Ok(true);
        }
      }
      Ok(false)
    },
    EditorSemanticCommand::JoinParagraphs { first, second } => {
      join_projection_paragraphs(doc, projection, *first, *second).context("joining paragraphs from editor semantic command")
    },
    EditorSemanticCommand::ReplaceParagraphSpan { start, before, after } => {
      replace_body_paragraph_span(doc, projection, *start, before, after).context("replacing paragraph span from editor semantic command")
    },
    EditorSemanticCommand::InsertBlock { block, block_ix, after } => insert_projection_object_block(doc, *block, *block_ix, after)
      .with_context(|| format!("inserting object block from editor semantic command at projection block {block_ix} ({block:?})")),
    EditorSemanticCommand::DeleteBlock { block } => {
      delete_projection_object_block(doc, *block).context("deleting object block from editor semantic command")
    },
    EditorSemanticCommand::MoveBlock { block, new_block_ix } => {
      move_projection_object_block(doc, *block, *new_block_ix).context("moving object block from editor semantic command")
    },
    EditorSemanticCommand::ReplaceBlock { block, block_ix, after } => replace_projection_object_block(doc, projection, *block, *block_ix, after)
      .with_context(|| format!("replacing object block from editor semantic command at projection block {block_ix} ({block:?})")),
    EditorSemanticCommand::InsertTableRow {
      table,
      new_row_id,
      after_row,
      row,
    } => table_ops::insert_table_row(doc, *table, *new_row_id, *after_row, row)
      .with_context(|| format!("inserting table row {new_row_id:?} from editor semantic command at table {table:?}")),
    EditorSemanticCommand::DeleteTableRow { table, row_id } => table_ops::delete_table_row(doc, *table, *row_id)
      .with_context(|| format!("deleting table row {row_id:?} from editor semantic command at table {table:?}")),
    EditorSemanticCommand::MoveTableRow { table, row_id, after_row } => table_ops::move_table_row(doc, *table, *row_id, *after_row)
      .with_context(|| format!("moving table row {row_id:?} after {after_row:?} at table {table:?}")),
    EditorSemanticCommand::InsertTableColumn {
      table,
      new_column_id,
      after_column,
      width,
      cells,
    } => table_ops::insert_table_column(doc, *table, *new_column_id, *after_column, width, cells)
      .with_context(|| format!("inserting table column {new_column_id:?} from editor semantic command at table {table:?}")),
    EditorSemanticCommand::DeleteTableColumn { table, column_id } => table_ops::delete_table_column(doc, *table, *column_id)
      .with_context(|| format!("deleting table column {column_id:?} from editor semantic command at table {table:?}")),
    EditorSemanticCommand::MoveTableColumn {
      table,
      column_id,
      after_column,
    } => table_ops::move_table_column(doc, *table, *column_id, *after_column)
      .with_context(|| format!("moving table column {column_id:?} after {after_column:?} at table {table:?}")),
    EditorSemanticCommand::ReplaceTableCell {
      table,
      row_id,
      column_id,
      cell,
    } => table_ops::replace_table_cell(doc, *table, *row_id, *column_id, cell)
      .with_context(|| format!("replacing table cell ({row_id:?},{column_id:?}) from editor semantic command at table {table:?}")),
    EditorSemanticCommand::SetTableCellSpan {
      table,
      row_id,
      column_id,
      row_span,
      column_span,
    } => table_ops::set_table_cell_span(doc, *table, *row_id, *column_id, *row_span, *column_span)
      .with_context(|| format!("setting table cell span at table {table:?}, cell ({row_id:?},{column_id:?})")),
    EditorSemanticCommand::ReplaceEquationSourceRange { equation, range, text } => {
      replace_projection_equation_source_range(doc, *equation, range, text)
        .with_context(|| format!("replacing equation source range from editor semantic command at equation {equation:?}, range {range:?}"))
    },
    EditorSemanticCommand::ReplaceImageAltText { image, text } => replace_projection_image_alt_text(doc, *image, text)
      .with_context(|| format!("replacing image alt text from editor semantic command at image {image:?}")),
    EditorSemanticCommand::ReplaceImageCaption { image, caption } => replace_projection_image_caption(doc, *image, caption.as_ref())
      .with_context(|| format!("replacing image caption from editor semantic command at image {image:?}")),
    EditorSemanticCommand::SetImageLayout { image, sizing, alignment } => set_projection_image_layout(doc, *image, sizing, *alignment)
      .with_context(|| format!("setting image layout from editor semantic command at image {image:?}")),
    EditorSemanticCommand::SetTableColumnWidth { table, column_ix, width } => table_ops::set_table_column_width(doc, *table, *column_ix, width)
      .with_context(|| format!("setting table column width from editor semantic command at table {table:?}, column {column_ix}")),
  }
}

#[derive(Clone, Debug)]
struct SubscriptionEventSummary {
  origin: String,
  triggered_by: String,
  // §23: runtime epoch read when the event was emitted. `summarize_subscription_event`
  // leaves this at 0 (it stays a pure function of the diff); the permanent root
  // callback stamps the live epoch before buffering.
  epoch: u64,
  // §23: doc state frontier captured at emit time (`doc.state_frontiers().encode()`).
  // Left empty by `summarize_subscription_event` and stamped by the root callback,
  // which holds a reference clone of the document.
  frontier: Vec<u8>,
  changes: Vec<SubscriptionChange>,
}

#[derive(Clone, Debug)]
enum SubscriptionChange {
  Text {
    target: String,
    unicode_start: usize,
    unicode_len: usize,
    deleted_len: usize,
    inserted_structure: bool,
  },
  Map {
    target: String,
    keys: Vec<String>,
  },
  List {
    target: String,
  },
  Unknown {
    target: String,
  },
}

fn apply_editor_semantic_command_body_fast_path(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  projection_index: &ProjectionRuntimeIndex,
  command: &EditorSemanticCommand,
) -> Result<bool> {
  match command {
    EditorSemanticCommand::InsertText { at, text, styles } => {
      let body = body_text(doc);
      let Some(unicode_index) = projection_index.body_unicode_for_offset_in_loro(doc, projection, *at) else {
        return Ok(false);
      };
      let newline_boundaries = inserted_newline_boundaries(unicode_index, text);
      body
        .insert(unicode_index, text)
        .context("inserting text into Loro body flow without projection snapshot")?;
      let inserted_len = text.chars().count();
      if inserted_len > 0 {
        mark_run_styles(&body, unicode_index..unicode_index + inserted_len, *styles).context("marking inserted run styles")?;
      }
      repair_paragraph_metadata_after_text_flow_edit(doc, &body, &newline_boundaries, "editor_insert_text_fast_path")?;
      Ok(true)
    },
    EditorSemanticCommand::DeleteRange { range } => {
      let body = body_text(doc);
      let Some(start) = projection_index.body_unicode_for_offset_in_loro(doc, projection, range.start) else {
        return Ok(false);
      };
      let Some(end) = projection_index.body_unicode_for_offset_in_loro(doc, projection, range.end) else {
        return Ok(false);
      };
      // §5 sentinel protection (preflight): clamp/reject a range that would delete
      // the boundary-0 sentinel newline before mutating the body.
      if let Some((start, len)) = sentinel_protected_delete_range(start, end.saturating_sub(start)) {
        body
          .delete(start, len)
          .context("deleting text from Loro body flow without projection snapshot")?;
        prune_orphaned_body_object_blocks(doc, &body)?;
        repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[], "editor_delete_range_fast_path")?;
        return Ok(true);
      }
      Ok(false)
    },
    EditorSemanticCommand::SplitParagraph {
      at,
      source_paragraph,
      source_block,
      new_paragraph,
      new_block,
      inherited_style,
    } => {
      if projection.ids.paragraph_ids.get(at.paragraph).copied() != Some(*source_paragraph) {
        return Ok(false);
      }
      let Some(source_block_ix) = flowstate_document::block_ix_for_paragraph(projection, at.paragraph) else {
        return Ok(false);
      };
      if projection.ids.block_ids.get(source_block_ix).copied() != Some(*source_block) {
        return Ok(false);
      }
      let body = body_text(doc);
      let Some(unicode_index) = projection_index.body_unicode_for_offset_in_loro(doc, projection, *at) else {
        return Ok(false);
      };
      body
        .insert(unicode_index, "\n")
        .context("splitting paragraph in Loro body flow without projection snapshot")?;
      body
        .mark(
          unicode_index..unicode_index + 1,
          MARK_PARAGRAPH_STYLE,
          paragraph_style_value(*inherited_style),
        )
        .context("marking split paragraph style")?;
      repair_paragraph_metadata_after_stable_split(doc, &body, unicode_index, *new_paragraph, *new_block, "editor_split_paragraph_fast_path")?;
      Ok(true)
    },
    EditorSemanticCommand::SetParagraphStyle { .. }
    | EditorSemanticCommand::SetRunStyles { .. }
    | EditorSemanticCommand::JoinParagraphs { .. }
    | EditorSemanticCommand::ReplaceParagraphSpan { .. }
    | EditorSemanticCommand::InsertBlock { .. }
    | EditorSemanticCommand::DeleteBlock { .. }
    | EditorSemanticCommand::MoveBlock { .. }
    | EditorSemanticCommand::ReplaceBlock { .. }
    | EditorSemanticCommand::InsertTableRow { .. }
    | EditorSemanticCommand::DeleteTableRow { .. }
    | EditorSemanticCommand::MoveTableRow { .. }
    | EditorSemanticCommand::InsertTableColumn { .. }
    | EditorSemanticCommand::DeleteTableColumn { .. }
    | EditorSemanticCommand::MoveTableColumn { .. }
    | EditorSemanticCommand::ReplaceTableCell { .. }
    | EditorSemanticCommand::SetTableCellSpan { .. }
    | EditorSemanticCommand::ReplaceEquationSourceRange { .. }
    | EditorSemanticCommand::ReplaceImageAltText { .. }
    | EditorSemanticCommand::ReplaceImageCaption { .. }
    | EditorSemanticCommand::SetImageLayout { .. }
    | EditorSemanticCommand::SetTableColumnWidth { .. } => Ok(false),
  }
}

fn incremental_projection_patches_for_command(
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  doc: &LoroDoc,
  command: &EditorSemanticCommand,
) -> Option<Vec<flowstate_document::ProjectionPatch>> {
  match command {
    EditorSemanticCommand::InsertText { at, text, .. } if !text.contains('\n') && !text.contains(OBJECT_REPLACEMENT) => {
      let row = flowstate_document::block_ix_for_paragraph(projection, at.paragraph)?;
      let old_len = flowstate_document::paragraph_text_len(projection.paragraphs.get(at.paragraph)?);
      let new = body_input_paragraph(doc, at.paragraph)?;
      Some(vec![flowstate_document::ProjectionPatch::ParagraphText {
        block_id: projection.ids.block_ids[row],
        paragraph_id: projection.ids.paragraph_ids[at.paragraph],
        row_hint: row,
        new,
        delta_utf8: projection_text_delta(at.byte.min(old_len), 0, text.len(), old_len.saturating_sub(at.byte.min(old_len))),
      }])
    },
    EditorSemanticCommand::DeleteRange { range } if range.start.paragraph == range.end.paragraph => {
      let paragraph_ix = range.start.paragraph;
      let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      let old_len = flowstate_document::paragraph_text_len(projection.paragraphs.get(paragraph_ix)?);
      let start = range.start.byte.min(old_len);
      let end = range.end.byte.min(old_len).max(start);
      let new = body_input_paragraph(doc, paragraph_ix)?;
      Some(vec![flowstate_document::ProjectionPatch::ParagraphText {
        block_id: projection.ids.block_ids[row],
        paragraph_id: projection.ids.paragraph_ids[paragraph_ix],
        row_hint: row,
        new,
        delta_utf8: projection_text_delta(start, end - start, 0, old_len.saturating_sub(end)),
      }])
    },
    EditorSemanticCommand::SetParagraphStyle { paragraph, style } => {
      // §24: O(1) paragraph metadata index lookup replaces the linear
      // `paragraph_ids` scan (with an identical linear fallback).
      let paragraph_ix = index.paragraph_index_for_id(projection, *paragraph)?;
      let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      Some(vec![flowstate_document::ProjectionPatch::ParagraphStyle {
        block_id: projection.ids.block_ids[row],
        paragraph_id: *paragraph,
        row_hint: row,
        style: *style,
      }])
    },
    EditorSemanticCommand::SetRunStyles { paragraph, .. } => {
      // §24: O(1) paragraph metadata index lookup replaces the linear
      // `paragraph_ids` scan (with an identical linear fallback).
      let paragraph_ix = index.paragraph_index_for_id(projection, *paragraph)?;
      let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      let new = body_input_paragraph(doc, paragraph_ix)?;
      Some(vec![flowstate_document::ProjectionPatch::ParagraphRuns {
        block_id: projection.ids.block_ids[row],
        paragraph_id: *paragraph,
        row_hint: row,
        runs: flowstate_document::document_from_input_blocks(projection.theme.clone(), vec![InputBlock::Paragraph(new)])
          .paragraphs
          .first()?
          .runs
          .clone(),
      }])
    },
    _ => structured_projection_patches_for_command(projection, index, command),
  }
}

fn structured_projection_patches_for_command(
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  command: &EditorSemanticCommand,
) -> Option<Vec<ProjectionPatch>> {
  match command {
    EditorSemanticCommand::InsertBlock { block, block_ix, after } => Some(vec![ProjectionPatch::InsertBlocks {
      before: projection.ids.block_ids.get(*block_ix).copied(),
      row_hint: (*block_ix).min(projection.blocks.len()),
      blocks: vec![ProjectionStructuralBlock {
        block_id: *block,
        paragraph_id: None,
        block: after.clone(),
      }],
    }]),
    // §24: O(1) block anchor index lookups replace the linear `block_ids` scans
    // (each with an identical linear fallback for ids absent from the index).
    EditorSemanticCommand::DeleteBlock { block } => Some(vec![ProjectionPatch::DeleteBlocks {
      block_ids: vec![*block],
      row_hint: index.block_index_for_id(projection, *block)?,
    }]),
    EditorSemanticCommand::MoveBlock { block, new_block_ix } => Some(vec![ProjectionPatch::MoveBlock {
      block_id: *block,
      before: projection.ids.block_ids.get(*new_block_ix).copied(),
      from_hint: index.block_index_for_id(projection, *block)?,
      to_hint: (*new_block_ix).min(projection.blocks.len().saturating_sub(1)),
    }]),
    EditorSemanticCommand::ReplaceBlock { block, block_ix, after } => object_replacement_patch(
      projection,
      block
        .and_then(|id| index.block_index_for_id(projection, id))
        .unwrap_or(*block_ix),
      after.clone(),
    ),
    EditorSemanticCommand::InsertTableRow {
      table,
      new_row_id,
      after_row,
      row,
    } => {
      // §P2b: mutate the id-bearing InputTableBlock by the SAME id + anchor the
      // canonical apply uses, so the optimistic prediction is byte-identical.
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      if !table_input.rows.iter().any(|existing| existing.id == *new_row_id) {
        let pos = input_table_row_insert_pos(&table_input, *after_row);
        table_input.rows.insert(pos, row.clone());
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::DeleteTableRow { table, row_id } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      let pos = table_input.rows.iter().position(|row| row.id == *row_id)?;
      table_input.rows.remove(pos);
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::MoveTableRow { table, row_id, after_row } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      let from = table_input.rows.iter().position(|row| row.id == *row_id)?;
      let row = table_input.rows.remove(from);
      let pos = input_table_row_insert_pos(&table_input, *after_row);
      table_input.rows.insert(pos.min(table_input.rows.len()), row);
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::InsertTableColumn {
      table,
      new_column_id,
      after_column,
      width,
      cells,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      if !table_input.columns.iter().any(|existing| existing.id == *new_column_id) {
        let pos = input_table_column_insert_pos(&table_input, *after_column);
        table_input.columns.insert(pos, InputTableColumn {
          id: *new_column_id,
          width: width.clone(),
        });
        for (row_ix, row) in table_input.rows.iter_mut().enumerate() {
          let row_id = row.id;
          let cell = cells
            .get(row_ix)
            .cloned()
            .unwrap_or_else(|| empty_input_table_cell(row_id, *new_column_id));
          let cell_pos = pos.min(row.cells.len());
          row.cells.insert(cell_pos, cell);
        }
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::DeleteTableColumn { table, column_id } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      let pos = table_input.columns.iter().position(|column| column.id == *column_id)?;
      table_input.columns.remove(pos);
      for row in &mut table_input.rows {
        if let Some(cell_ix) = row.cells.iter().position(|cell| cell.column_id == *column_id) {
          row.cells.remove(cell_ix);
        } else if pos < row.cells.len() {
          row.cells.remove(pos);
        }
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::MoveTableColumn {
      table,
      column_id,
      after_column,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      let from = table_input.columns.iter().position(|column| column.id == *column_id)?;
      let column = table_input.columns.remove(from);
      let pos = input_table_column_insert_pos(&table_input, *after_column);
      table_input.columns.insert(pos.min(table_input.columns.len()), column);
      for row in &mut table_input.rows {
        if let Some(cell_from) = row.cells.iter().position(|cell| cell.column_id == *column_id) {
          let cell = row.cells.remove(cell_from);
          let cell_to = pos.min(row.cells.len());
          row.cells.insert(cell_to, cell);
        }
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::ReplaceTableCell {
      table,
      row_id,
      column_id,
      cell,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      let row = table_input.rows.iter_mut().find(|row| row.id == *row_id)?;
      let target = row.cells.iter_mut().find(|existing| existing.column_id == *column_id)?;
      *target = cell.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::SetTableCellSpan {
      table,
      row_id,
      column_id,
      row_span,
      column_span,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      let row = table_input.rows.iter_mut().find(|row| row.id == *row_id)?;
      let cell = row.cells.iter_mut().find(|cell| cell.column_id == *column_id)?;
      cell.row_span = (*row_span).max(1);
      cell.col_span = (*column_span).max(1);
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::SetTableColumnWidth { table, column_ix, width } => {
      let (block_ix, mut table_input) = projected_table_input(projection, index, *table)?;
      table_input.columns.get_mut(*column_ix)?.width = width.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::ReplaceEquationSourceRange { equation, range, text } => {
      // §24: O(1) block anchor index lookup (linear fallback preserved).
      let block_ix = index.block_index_for_id(projection, *equation)?;
      let InputBlock::Equation(mut equation_input) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
        return None;
      };
      if range.start > range.end
        || range.end > equation_input.source.len()
        || !equation_input.source.is_char_boundary(range.start)
        || !equation_input.source.is_char_boundary(range.end)
      {
        return None;
      }
      equation_input.source.replace_range(range.clone(), text);
      object_replacement_patch(projection, block_ix, InputBlock::Equation(equation_input))
    },
    EditorSemanticCommand::ReplaceImageAltText { image, text } => {
      // §24: O(1) block anchor index lookup (linear fallback preserved).
      let block_ix = index.block_index_for_id(projection, *image)?;
      let InputBlock::Image(mut image_input) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
        return None;
      };
      image_input.alt_text = text.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
    },
    EditorSemanticCommand::ReplaceImageCaption { image, caption } => {
      // §24: O(1) block anchor index lookup (linear fallback preserved).
      let block_ix = index.block_index_for_id(projection, *image)?;
      let InputBlock::Image(mut image_input) = projection
        .blocks
        .get(block_ix)
        .map(flowstate_document::input_block_from_block)?
      else {
        return None;
      };
      image_input.caption = caption.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
    },
    EditorSemanticCommand::SetImageLayout { image, sizing, alignment } => {
      // §24: O(1) block anchor index lookup (linear fallback preserved).
      let block_ix = index.block_index_for_id(projection, *image)?;
      let InputBlock::Image(mut image_input) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
        return None;
      };
      image_input.sizing = sizing.clone();
      image_input.alignment = *alignment;
      object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
    },
    EditorSemanticCommand::InsertText { .. }
    | EditorSemanticCommand::DeleteRange { .. }
    | EditorSemanticCommand::SplitParagraph { .. }
    | EditorSemanticCommand::JoinParagraphs { .. }
    | EditorSemanticCommand::SetParagraphStyle { .. }
    | EditorSemanticCommand::SetRunStyles { .. }
    | EditorSemanticCommand::ReplaceParagraphSpan { .. } => None,
  }
}

fn projected_table_input(
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  table: flowstate_document::BlockId,
) -> Option<(usize, InputTableBlock)> {
  // §24: O(1) block anchor index lookup replaces the linear `block_ids` scan that
  // every table structural command funnels through (identical linear fallback).
  let block_ix = index.block_index_for_id(projection, table)?;
  let InputBlock::Table(table) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
    return None;
  };
  Some((block_ix, table))
}

/// §P2b anchor→index for an id-bearing predicted [`InputTableBlock`] row list:
/// `None` => head; a present `after_row` => immediately after it; an absent anchor
/// (concurrently deleted) => tail. Matches the canonical apply's row resolution.
fn input_table_row_insert_pos(table: &InputTableBlock, after_row: Option<RowId>) -> usize {
  match after_row {
    None => 0,
    Some(anchor) => table
      .rows
      .iter()
      .position(|row| row.id == anchor)
      .map_or(table.rows.len(), |ix| ix + 1),
  }
}

/// §P2b anchor→index for an id-bearing predicted [`InputTableBlock`] column list.
/// See [`input_table_row_insert_pos`].
fn input_table_column_insert_pos(table: &InputTableBlock, after_column: Option<ColumnId>) -> usize {
  match after_column {
    None => 0,
    Some(anchor) => table
      .columns
      .iter()
      .position(|column| column.id == anchor)
      .map_or(table.columns.len(), |ix| ix + 1),
  }
}

fn object_replacement_patch(projection: &DocumentProjection, block_ix: usize, block: InputBlock) -> Option<Vec<ProjectionPatch>> {
  Some(vec![ProjectionPatch::ReplaceObjectBlock {
    block_id: *projection.ids.block_ids.get(block_ix)?,
    row_hint: block_ix,
    block: ProjectionStructuralBlock {
      block_id: *projection.ids.block_ids.get(block_ix)?,
      paragraph_id: None,
      block,
    },
  }])
}

fn projection_text_delta(
  prefix_retain: usize,
  delete_len: usize,
  insert_len: usize,
  trailing_retain: usize,
) -> Vec<flowstate_document::ProjectionTextDelta> {
  let mut delta = Vec::new();
  if prefix_retain > 0 {
    delta.push(flowstate_document::ProjectionTextDelta::Retain(prefix_retain));
  }
  if delete_len > 0 {
    delta.push(flowstate_document::ProjectionTextDelta::Delete(delete_len));
  }
  if insert_len > 0 {
    delta.push(flowstate_document::ProjectionTextDelta::Insert(insert_len));
  }
  if trailing_retain > 0 {
    delta.push(flowstate_document::ProjectionTextDelta::Retain(trailing_retain));
  }
  delta
}

fn editor_command_invalidation(
  projection: &DocumentProjection,
  command: &EditorSemanticCommand,
  frontier_before: Vec<u8>,
  frontier_after: Vec<u8>,
) -> ProjectionInvalidation {
  match command {
    EditorSemanticCommand::InsertText { at, text, .. } => ProjectionInvalidation::body_text(
      frontier_before,
      frontier_after,
      projection_offset_to_body_unicode_index(projection, *at),
      text.chars().count(),
    ),
    EditorSemanticCommand::DeleteRange { range } => {
      let start = projection_offset_to_body_unicode_index(projection, range.start);
      let end = projection_offset_to_body_unicode_index(projection, range.end);
      ProjectionInvalidation::body_text(frontier_before, frontier_after, start, end.saturating_sub(start))
    },
    EditorSemanticCommand::SetParagraphStyle { paragraph, .. } => {
      let paragraph_ix = projection
        .ids
        .paragraph_ids
        .iter()
        .position(|id| id == paragraph)
        .unwrap_or_default();
      ProjectionInvalidation::body_style(
        frontier_before,
        frontier_after,
        paragraph_boundary_unicode_index(projection, paragraph_ix),
        1,
      )
    },
    EditorSemanticCommand::SetRunStyles { paragraph, range, .. } => {
      let paragraph_ix = projection
        .ids
        .paragraph_ids
        .iter()
        .position(|id| id == paragraph)
        .unwrap_or_default();
      let start = projection_offset_to_body_unicode_index(
        projection,
        DocumentOffset {
          paragraph: paragraph_ix,
          byte: range.start,
        },
      );
      ProjectionInvalidation::body_style(frontier_before, frontier_after, start, range.end.saturating_sub(range.start))
    },
    _ => ProjectionInvalidation::full_rebuild(frontier_before, frontier_after, "editor_structural_projection_fallback"),
  }
}

fn insert_projection_object_block(doc: &LoroDoc, block_id: flowstate_document::BlockId, block_ix: usize, input: &InputBlock) -> Result<bool> {
  if matches!(input, InputBlock::Paragraph(_)) {
    tracing::warn!(
      block_ix,
      ?block_id,
      "skipping InsertBlock for paragraph payload; paragraph edits must use text/paragraph semantic commands"
    );
    return Ok(false);
  }

  let body = body_text(doc);
  if object_loro_block_by_projected_id(doc, &body, block_id).is_some() {
    tracing::warn!(block_ix, ?block_id, "skipping InsertBlock because the Loro object block already exists");
    return Ok(false);
  }
  let Some(unicode_index) = object_insert_unicode_pos_for_projection_block(&body, block_ix) else {
    tracing::warn!(
      block_ix,
      ?block_id,
      "skipping InsertBlock because no Loro insertion point maps to the projection block index"
    );
    return Ok(false);
  };
  insert_input_object_block(doc, unicode_index, block_id, input)?;
  Ok(true)
}

fn insert_input_object_block(doc: &LoroDoc, unicode_index: usize, block_id: flowstate_document::BlockId, input: &InputBlock) -> Result<()> {
  match input {
    InputBlock::Image(image) => insert_image_block_with_id(doc, unicode_index, block_id, image),
    InputBlock::Equation(equation) => insert_equation_block_with_id(doc, unicode_index, block_id, equation),
    InputBlock::Table(table) => insert_table_block_with_id(doc, unicode_index, block_id, table),
    InputBlock::Paragraph(_) => Ok(()),
  }
}

fn replace_projection_object_block(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  block_id: Option<flowstate_document::BlockId>,
  block_ix: usize,
  after: &InputBlock,
) -> Result<bool> {
  if matches!(after, InputBlock::Paragraph(_)) {
    tracing::warn!(
      block_ix,
      "skipping ReplaceBlock for paragraph payload; paragraph edits must use text/paragraph semantic commands"
    );
    return Ok(false);
  }
  if block_id.is_none() && projection.blocks.get(block_ix).is_none() {
    tracing::warn!(block_ix, "skipping ReplaceBlock because the projection block index is out of range");
    return Ok(false);
  }

  let body = body_text(doc);
  let block = block_id
    .and_then(|block_id| object_loro_block_by_projected_id(doc, &body, block_id).map(|(_, block, _)| block))
    .or_else(|| {
      projection
        .ids
        .block_ids
        .get(block_ix)
        .and_then(|block_id| object_loro_block_by_projected_id(doc, &body, *block_id).map(|(_, block, _)| block))
    })
    .or_else(|| {
      let anchor_pos = object_unicode_pos_for_projection_block(&body, block_ix)?;
      object_loro_block_at_unicode_pos(doc, &body, anchor_pos)
    });
  let Some(block) = block else {
    tracing::warn!(block_ix, "skipping ReplaceBlock because no Loro object block maps to the projected block");
    return Ok(false);
  };

  match after {
    InputBlock::Image(image) => replace_image_block_from_input(doc, &block, image)?,
    InputBlock::Equation(equation) => replace_equation_block_from_input(doc, &block, equation)?,
    InputBlock::Table(table) => {
      tracing::warn!(
        block_ix,
        "applying coarse structured table ReplaceBlock; editor should emit finer table operations later"
      );
      replace_table_block_from_input(doc, &block, table)?;
    },
    InputBlock::Paragraph(_) => unreachable!("paragraph payload was handled above"),
  }
  Ok(true)
}

fn replace_projection_equation_source_range(
  doc: &LoroDoc,
  equation_block_id: flowstate_document::BlockId,
  range: &std::ops::Range<usize>,
  replacement: &str,
) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, equation_block_id) else {
    tracing::warn!(
      ?equation_block_id,
      ?range,
      "skipping equation source edit because no Loro equation maps to the projected block id"
    );
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("equation") {
    tracing::warn!(
      ?equation_block_id,
      ?range,
      "skipping equation source edit because the projected block is not an equation"
    );
    return Ok(false);
  }
  let source_flow_id = map_string_opt(&block, "source_flow_id").unwrap_or_else(|| nested_flow_id("equation_source"));
  block.insert("source_flow_id", source_flow_id.as_str())?;
  let source_flow = ensure_flow(doc, &source_flow_id, "equation_source")?;
  // §28: resolve the source flow's text via its stored `text_container_id`.
  let source_text = flow_text(doc, &source_flow)?;
  let before = source_text.to_string();
  let Some(start) = byte_index_to_unicode_index(&before, range.start) else {
    tracing::warn!(
      ?equation_block_id,
      ?range,
      "skipping equation source edit because the start byte is not a source boundary"
    );
    return Ok(false);
  };
  let Some(end) = byte_index_to_unicode_index(&before, range.end) else {
    tracing::warn!(
      ?equation_block_id,
      ?range,
      "skipping equation source edit because the end byte is not a source boundary"
    );
    return Ok(false);
  };
  if end < start {
    tracing::warn!(?equation_block_id, ?range, "skipping equation source edit because the range is inverted");
    return Ok(false);
  }
  if end > start {
    source_text.delete(start, end - start)?;
  }
  if !replacement.is_empty() {
    source_text.insert(start, replacement)?;
  }
  Ok(true)
}

fn replace_projection_image_alt_text(doc: &LoroDoc, image_block_id: flowstate_document::BlockId, text: &str) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, image_block_id) else {
    tracing::warn!(
      ?image_block_id,
      "skipping image alt text edit because no Loro image maps to the projected block id"
    );
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    tracing::warn!(
      ?image_block_id,
      "skipping image alt text edit because the projected block is not an image"
    );
    return Ok(false);
  }
  let alt_flow_id = map_string_opt(&block, "alt_text_flow_id").unwrap_or_else(|| nested_flow_id("image_alt"));
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  // §28: resolve the alt-text flow's text via its stored `text_container_id`.
  replace_text_incrementally(&flow_text(doc, &alt_flow)?, text)?;
  Ok(true)
}

fn replace_projection_image_caption(
  doc: &LoroDoc,
  image_block_id: flowstate_document::BlockId,
  caption: Option<&InputParagraph>,
) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, image_block_id) else {
    tracing::warn!(
      ?image_block_id,
      "skipping image caption edit because no Loro image maps to the projected block id"
    );
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    return Ok(false);
  }
  if let Some(caption) = caption {
    let caption_flow_id = map_string_opt(&block, "caption_flow_id").unwrap_or_else(|| nested_flow_id("image_caption"));
    block.insert("caption_flow_id", caption_flow_id.as_str())?;
    let caption_flow = ensure_flow(doc, &caption_flow_id, "caption")?;
    let text = caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
    let desired = format!(
      "{SENTINEL_NEWLINE}{}",
      caption
        .runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>()
    );
    replace_text_incrementally(&text, &desired)?;
    let len = text.len_unicode();
    for key in [
      MARK_PARAGRAPH_STYLE,
      MARK_RUN_SEMANTIC_STYLE,
      MARK_HIGHLIGHT_STYLE,
      MARK_DIRECT_UNDERLINE,
      MARK_STRIKETHROUGH,
    ] {
      text.unmark(0..len, key)?;
    }
    text.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(caption.style))?;
    let mut cursor = 1usize;
    for run in &caption.runs {
      let run_len = run.text.chars().count();
      if run_len > 0 {
        mark_run_styles(&text, cursor..cursor + run_len, run.styles)?;
      }
      cursor += run_len;
    }
  } else {
    block.delete("caption_flow_id")?;
  }
  Ok(true)
}

fn set_projection_image_layout(
  doc: &LoroDoc,
  image_block_id: flowstate_document::BlockId,
  sizing: &InputImageSizing,
  alignment: InputBlockAlignment,
) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, image_block_id) else {
    tracing::warn!(
      ?image_block_id,
      "skipping image layout edit because no Loro image maps to the projected block id"
    );
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    tracing::warn!(?image_block_id, "skipping image layout edit because the projected block is not an image");
    return Ok(false);
  }
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(alignment))?;
  write_image_sizing_attrs(&attrs, sizing)?;
  Ok(true)
}

fn byte_index_to_unicode_index(value: &str, byte: usize) -> Option<usize> {
  (byte <= value.len() && value.is_char_boundary(byte)).then(|| value[..byte].chars().count())
}

/// §P2b: the deterministic empty cell for the `(row_id, column_id)` coordinate.
/// Its [`CellId`] is derived from the coordinate so a peer that fills a
/// grid-gap cell addresses the identical Loro container (LWW merge, never a
/// duplicate). Used as the fallback when a structural command's carried cell
/// list is shorter than the grid.
fn empty_input_table_cell(row_id: RowId, column_id: ColumnId) -> InputTableCell {
  InputTableCell {
    id: CellId::from_coordinate(row_id, column_id),
    row_id,
    column_id,
    blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    })],
    row_span: 1,
    col_span: 1,
  }
}

fn projection_table_map_by_block_id(doc: &LoroDoc, table_block_id: flowstate_document::BlockId) -> Option<LoroMap> {
  let body = body_text(doc);
  let (_, block, _) = object_loro_block_by_projected_id(doc, &body, table_block_id)?;
  if map_string_opt(&block, "kind").as_deref() != Some("table") {
    return None;
  }
  child_map(&block, "table")
}

fn delete_projection_object_block(doc: &LoroDoc, block_id: flowstate_document::BlockId) -> Result<bool> {
  let body = body_text(doc);
  let Some((key, _, anchor_pos)) = object_loro_block_by_projected_id(doc, &body, block_id) else {
    tracing::warn!(
      ?block_id,
      "skipping DeleteBlock because no Loro object block maps to the projected block id"
    );
    return Ok(false);
  };
  if body.to_string().chars().nth(anchor_pos) != Some(OBJECT_REPLACEMENT) {
    tracing::warn!(
      ?block_id,
      anchor_pos,
      "skipping DeleteBlock because the Loro object anchor is no longer live"
    );
    return Ok(false);
  }
  body
    .delete(anchor_pos, 1)
    .context("deleting object placeholder from body flow")?;
  doc
    .get_map(ROOT)
    .ensure_mergeable_map(BLOCKS_BY_ID)?
    .delete(&key)
    .context("deleting object block metadata")?;
  Ok(true)
}

fn move_projection_object_block(doc: &LoroDoc, block_id: flowstate_document::BlockId, new_block_ix: usize) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, anchor_pos)) = object_loro_block_by_projected_id(doc, &body, block_id) else {
    tracing::warn!(
      ?block_id,
      new_block_ix,
      "skipping MoveBlock because no Loro object block maps to the projected block id"
    );
    return Ok(false);
  };
  if body.to_string().chars().nth(anchor_pos) != Some(OBJECT_REPLACEMENT) {
    tracing::warn!(
      ?block_id,
      anchor_pos,
      "skipping MoveBlock because the Loro object anchor is no longer live"
    );
    return Ok(false);
  }
  body
    .delete(anchor_pos, 1)
    .context("deleting object placeholder before move")?;
  let insert_pos = object_insert_unicode_pos_for_projection_block(&body, new_block_ix).unwrap_or_else(|| body.len_unicode());
  body
    .insert(insert_pos, &OBJECT_REPLACEMENT.to_string())
    .context("reinserting object placeholder after move")?;
  if let Some(cursor) = body.get_cursor(insert_pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  Ok(true)
}

fn object_unicode_pos_for_projection_block(body: &LoroText, target_block_ix: usize) -> Option<usize> {
  let mut block_ix = 0_usize;
  let mut current_paragraph_has_text = false;
  let mut seen_sentinel = false;

  for (unicode_pos, ch) in body.to_string().chars().enumerate() {
    match ch {
      '\n' => {
        if seen_sentinel {
          if block_ix == target_block_ix {
            return None;
          }
          block_ix += 1;
        } else {
          seen_sentinel = true;
        }
        current_paragraph_has_text = false;
      },
      OBJECT_REPLACEMENT => {
        if current_paragraph_has_text {
          if block_ix == target_block_ix {
            return None;
          }
          block_ix += 1;
          current_paragraph_has_text = false;
        }
        if block_ix == target_block_ix {
          return Some(unicode_pos);
        }
        block_ix += 1;
      },
      _ => current_paragraph_has_text = true,
    }
  }
  None
}

fn object_insert_unicode_pos_for_projection_block(body: &LoroText, target_block_ix: usize) -> Option<usize> {
  let mut block_ix = 0_usize;
  let mut current_paragraph_has_text = false;
  let mut seen_sentinel = false;
  let mut last_pos = 0_usize;

  for (unicode_pos, ch) in body.to_string().chars().enumerate() {
    last_pos = unicode_pos + 1;
    match ch {
      '\n' => {
        if seen_sentinel {
          if block_ix >= target_block_ix {
            return Some(unicode_pos);
          }
          block_ix += 1;
        } else {
          seen_sentinel = true;
        }
        current_paragraph_has_text = false;
      },
      OBJECT_REPLACEMENT => {
        if current_paragraph_has_text {
          if block_ix >= target_block_ix {
            return Some(unicode_pos);
          }
          block_ix += 1;
          current_paragraph_has_text = false;
        }
        if block_ix >= target_block_ix {
          return Some(unicode_pos);
        }
        block_ix += 1;
      },
      _ => current_paragraph_has_text = true,
    }
  }

  if current_paragraph_has_text {
    if block_ix >= target_block_ix {
      return Some(last_pos);
    }
    block_ix += 1;
  }
  (block_ix <= target_block_ix).then_some(last_pos)
}

/// Live start (unicode) of the paragraph identified by `paragraph_id` in the actual
/// Loro body flow, resolved from its durable boundary cursor — the paragraph's text
/// begins just past its boundary newline. Coalescing-agnostic: unlike the
/// projection-derived body-unicode index, this reflects boundary newlines that are
/// physically present in the body even when the projection has coalesced that
/// paragraph out of view (an object-adjacent empty paragraph). Returns `None` when
/// the durable record or its cursor cannot be resolved, so callers fall back to the
/// projection-space start.
fn paragraph_body_start_in_loro(doc: &LoroDoc, paragraph_id: ParagraphId) -> Option<usize> {
  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID).ok()?;
  for key in map_keys(&paragraphs) {
    if loro_id_u128(&key) != paragraph_id.0 {
      continue;
    }
    let paragraph = child_map(&paragraphs, &key)?;
    for field in ["boundary_cursor", "start_cursor"] {
      if let Some(bytes) = map_binary_opt(&paragraph, field)
        && let Ok(cursor) = Cursor::decode(&bytes)
        && let Ok(resolved) = doc.get_cursor_pos(&cursor)
      {
        return Some(resolved.current.pos.saturating_add(1));
      }
    }
    return None;
  }
  None
}

/// Loro-space boundary position (the paragraph's leading `\n`) for projection
/// paragraph `paragraph_ix`, resolved from its durable cursor so coalesced empties
/// don't shift it. Loro-space counterpart of [`paragraph_boundary_unicode_index`];
/// use at Loro body-mutation sites (join delete, style mark). The paragraph's text
/// begins one unicode past its boundary. Falls back to the projection-space boundary
/// when the durable record can't be resolved (e.g. the boundary-0 sentinel).
fn paragraph_boundary_loro_unicode_index(doc: &LoroDoc, projection: &DocumentProjection, paragraph_ix: usize) -> usize {
  if paragraph_ix == 0 {
    return 0;
  }
  projection
    .ids
    .paragraph_ids
    .get(paragraph_ix)
    .and_then(|paragraph_id| paragraph_body_start_in_loro(doc, *paragraph_id))
    .map(|start| start.saturating_sub(1))
    .unwrap_or_else(|| paragraph_boundary_unicode_index(projection, paragraph_ix))
}

fn object_loro_block_by_projected_id(doc: &LoroDoc, body: &LoroText, block_id: flowstate_document::BlockId) -> Option<(String, LoroMap, usize)> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID).ok()?;
  let body_snapshot = body.to_string();
  for key in map_keys(&blocks) {
    if loro_id_u128(&key) != block_id.0 {
      continue;
    }
    let block = child_map(&blocks, &key)?;
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      return None;
    }
    let anchor_pos = live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor")?;
    return Some((key, block, anchor_pos));
  }
  for key in map_keys(&blocks) {
    let block = child_map(&blocks, &key)?;
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      continue;
    }
    if map_string_opt(&block, "id").is_some_and(|id| loro_id_u128(&id) == block_id.0) {
      let anchor_pos = live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor")?;
      return Some((key, block, anchor_pos));
    }
  }
  None
}

fn object_loro_block_at_unicode_pos(doc: &LoroDoc, body: &LoroText, unicode_pos: usize) -> Option<LoroMap> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID).ok()?;
  let body_snapshot = body.to_string();
  for key in map_keys(&blocks) {
    let block = child_map(&blocks, &key)?;
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      continue;
    }
    if live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor") == Some(unicode_pos) {
      return Some(block);
    }
  }
  None
}

fn loro_id_u128(id: &str) -> u128 {
  if let Some(value) = id
    .rsplit('.')
    .next()
    .and_then(|suffix| suffix.parse::<u128>().ok())
  {
    return value;
  }
  Uuid::new_v5(&Uuid::NAMESPACE_OID, id.as_bytes()).as_u128()
}

fn replace_image_block_from_input(doc: &LoroDoc, block: &LoroMap, image: &flowstate_document::InputImageBlock) -> Result<()> {
  block.insert("kind", "image")?;
  block.insert("asset_id", image.asset_id.0.to_string())?;
  copy_asset_metadata_to_image_block(doc, block, image.asset_id.0)?;

  let alt_flow_id = map_string_opt(block, "alt_text_flow_id").unwrap_or_else(|| nested_flow_id("image_alt"));
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, image.alt_text.as_ref())?;

  if let Some(caption) = &image.caption {
    let caption_flow_id = map_string_opt(block, "caption_flow_id").unwrap_or_else(|| nested_flow_id("image_caption"));
    block.insert("caption_flow_id", caption_flow_id.as_str())?;
    let caption_flow = ensure_flow(doc, &caption_flow_id, "caption")?;
    let caption_text = caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
    replace_text(&caption_text, SENTINEL_NEWLINE)?;
    append_input_paragraph_text_only(&caption_text, caption)?;
  } else {
    block.delete("caption_flow_id")?;
  }

  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(image.alignment))?;
  write_image_sizing_attrs(&attrs, &image.sizing)?;
  Ok(())
}

fn copy_asset_metadata_to_image_block(doc: &LoroDoc, block: &LoroMap, asset_id: u128) -> Result<()> {
  let root = doc.get_map(ROOT);
  let Some(assets) = child_map(&root, flowstate_document::loro_schema::ASSETS_BY_ID) else {
    return Ok(());
  };
  let Some(asset) = child_map(&assets, &asset_id.to_string()) else {
    return Ok(());
  };
  for field in ["content_hash", "mime_type", "byte_length"] {
    if let Some(ValueOrContainer::Value(value)) = asset.get(field) {
      block.insert(field, value)?;
    }
  }
  Ok(())
}

fn refresh_image_asset_metadata(doc: &LoroDoc) -> Result<()> {
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(());
  };
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("image") {
      continue;
    }
    let Some(asset_id) = map_string_opt(&block, "asset_id").and_then(|id| id.parse().ok()) else {
      continue;
    };
    copy_asset_metadata_to_image_block(doc, &block, asset_id)?;
  }
  Ok(())
}

fn replace_equation_block_from_input(doc: &LoroDoc, block: &LoroMap, equation: &flowstate_document::InputEquationBlock) -> Result<()> {
  block.insert("kind", "equation")?;
  let source_flow_id = map_string_opt(block, "source_flow_id").unwrap_or_else(|| nested_flow_id("equation_source"));
  block.insert("source_flow_id", source_flow_id.as_str())?;
  let source_flow = ensure_flow(doc, &source_flow_id, "equation_source")?;
  replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, &equation.source)?;
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("syntax", "latex")?;
  attrs.insert("display", equation_display_name(equation.display))?;
  Ok(())
}

fn replace_table_block_from_input(doc: &LoroDoc, block: &LoroMap, table: &InputTableBlock) -> Result<()> {
  block.insert("kind", "table")?;
  let table_map = block.ensure_mergeable_map("table")?;
  // §P2b: the create-only writer reuses the carried durable ids and writes into
  // the same durable-block-keyed container, so it no longer rekeys the table.
  write_table_map_from_input(doc, &table_map, table)
}

fn write_image_sizing_attrs(attrs: &LoroMap, sizing: &InputImageSizing) -> Result<()> {
  attrs.delete("width_px")?;
  attrs.delete("height_px")?;
  match sizing {
    InputImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
    InputImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
    InputImageSizing::Fixed { width_px, height_px } => {
      attrs.insert("sizing", "fixed")?;
      attrs.insert("width_px", i64::from(*width_px))?;
      if let Some(height_px) = *height_px {
        attrs.insert("height_px", i64::from(height_px))?;
      }
    },
  };
  Ok(())
}

/// §P2b create-only whole-table writer, mirroring `loro_import::import_table`.
///
/// Every column / row / cell is addressed by its carried durable id via `ensure_*`
/// and its `*_loro_id` string, and an id is only pushed into an order list when it
/// is not already present. There is no positional `{prefix}.row.{ix}` scheme and
/// no clear + repopulate, so concurrent creation of the same id merges (Loro LWW)
/// instead of duplicating, and re-applying the same input rewrites in place
/// (which is what lets `replace_table_block_from_input` reuse existing ids).
fn write_table_map_from_input(doc: &LoroDoc, table_map: &LoroMap, table: &InputTableBlock) -> Result<()> {
  table_map.insert("header_row", table.style.header_row)?;
  let row_order = table_map.ensure_mergeable_movable_list("row_order")?;
  let column_order = table_map.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table_map.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table_map.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table_map.ensure_mergeable_map("cells_by_id")?;
  table_map.insert("container_id", table_map.id().to_string())?;
  table_map.insert("row_order_container_id", row_order.id().to_string())?;
  table_map.insert("column_order_container_id", column_order.id().to_string())?;
  table_map.insert("rows_container_id", rows_by_id.id().to_string())?;
  table_map.insert("columns_container_id", columns_by_id.id().to_string())?;
  table_map.insert("cells_container_id", cells_by_id.id().to_string())?;

  let existing_columns = movable_list_strings(&column_order);
  for column in &table.columns {
    let column_id = column_loro_id(column.id);
    if !existing_columns.iter().any(|id| id == &column_id) {
      column_order.push(column_id.as_str())?;
    }
    let column_map = columns_by_id.ensure_mergeable_map(&column_id)?;
    column_map.insert("id", column_id.as_str())?;
    column_map.insert("container_id", column_map.id().to_string())?;
    let attrs = column_map.ensure_mergeable_map("attrs")?;
    column_map.insert("attrs_container_id", attrs.id().to_string())?;
    write_table_column_width(&column_map, &column.width)?;
  }

  let existing_rows = movable_list_strings(&row_order);
  for row in &table.rows {
    let row_id = row_loro_id(row.id);
    if !existing_rows.iter().any(|id| id == &row_id) {
      row_order.push(row_id.as_str())?;
    }
    let row_map = rows_by_id.ensure_mergeable_map(&row_id)?;
    row_map.insert("id", row_id.as_str())?;
    row_map.insert("container_id", row_map.id().to_string())?;
    let attrs = row_map.ensure_mergeable_map("attrs")?;
    row_map.insert("attrs_container_id", attrs.id().to_string())?;
    for cell in &row.cells {
      let cell_id = cell_loro_id(cell.id);
      let cell_row_id = row_loro_id(cell.row_id);
      let cell_column_id = column_loro_id(cell.column_id);
      let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
      write_table_cell_map_from_input(doc, &cell_map, &cell_id, &cell_row_id, &cell_column_id, cell, true)?;
    }
  }
  Ok(())
}

fn write_table_cell_map_from_input(
  doc: &LoroDoc,
  cell_map: &LoroMap,
  cell_id: &str,
  row_id: &str,
  column_id: &str,
  cell: &InputTableCell,
  // §P2b: when `false`, ensure the (empty) cell flow container but write no
  // sentinel `\n` or block content. The topology-repair pass uses this so two
  // peers concurrently materializing the SAME missing coordinate converge to an
  // empty flow (one empty paragraph) instead of racing two `\n` inserts into the
  // same deterministic flow (which would merge to `\n\n` = two empty paragraphs).
  seed_flow: bool,
) -> Result<()> {
  cell_map.insert("id", cell_id)?;
  cell_map.insert("container_id", cell_map.id().to_string())?;
  cell_map.insert("row_id", row_id)?;
  cell_map.insert("column_id", column_id)?;
  cell_map.insert("row_span", i64::from(cell.row_span))?;
  cell_map.insert("column_span", i64::from(cell.col_span))?;
  let attrs = cell_map.ensure_mergeable_map("attrs")?;
  cell_map.insert("attrs_container_id", attrs.id().to_string())?;
  let nested_table_ids = cell_map.ensure_mergeable_movable_list("nested_table_ids")?;
  let nested_tables_by_id = cell_map.ensure_mergeable_map("nested_tables_by_id")?;
  cell_map.insert("nested_table_order_container_id", nested_table_ids.id().to_string())?;
  cell_map.insert("nested_tables_container_id", nested_tables_by_id.id().to_string())?;
  clear_movable_list(&nested_table_ids)?;
  clear_map(&nested_tables_by_id)?;
  let flow_id = format!("{cell_id}.flow");
  cell_map.insert("flow_id", flow_id.as_str())?;
  let flow = ensure_flow(doc, &flow_id, "table_cell")?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  cell_map.insert("flow_container_id", flow.id().to_string())?;
  cell_map.insert("text_container_id", text.id().to_string())?;
  if !seed_flow {
    // Empty cell flow: the projector reads it back as a single empty paragraph,
    // and doing no text insert keeps concurrent coordinate-cell repair idempotent.
    return Ok(());
  }
  replace_text(&text, SENTINEL_NEWLINE)?;
  text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
  for (block_ix, cell_block) in cell.blocks.iter().enumerate() {
    match cell_block {
      InputTableCellBlock::Paragraph(paragraph) => append_input_paragraph_text_only(&text, paragraph)?,
      InputTableCellBlock::Table(nested) => {
        let pos = text.len_unicode();
        text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
        let nested_table_id = format!("{cell_id}.nested_table.{block_ix}");
        nested_table_ids.push(nested_table_id.as_str())?;
        let nested_map = nested_tables_by_id.ensure_mergeable_map(&nested_table_id)?;
        nested_map.insert("id", nested_table_id.as_str())?;
        nested_map.insert("container_id", nested_map.id().to_string())?;
        nested_map.insert("kind", "table")?;
        if let Some(cursor) = text.get_cursor(pos, Side::Left) {
          nested_map.insert("anchor_cursor", cursor.encode())?;
        }
        nested_map.ensure_mergeable_map("attrs")?;
        write_table_map_from_input(doc, &nested_map.ensure_mergeable_map("table")?, nested)?;
      },
    }
  }
  Ok(())
}

fn update_table_cell_map_from_input(
  doc: &LoroDoc,
  cell_map: &LoroMap,
  cell_id: &str,
  row_id: &str,
  column_id: &str,
  cell: &InputTableCell,
) -> Result<()> {
  if cell
    .blocks
    .iter()
    .any(|block| matches!(block, InputTableCellBlock::Table(_)))
  {
    tracing::warn!(cell_id, "using full table-cell rebuild fallback for nested table structure");
    return write_table_cell_map_from_input(doc, cell_map, cell_id, row_id, column_id, cell, true);
  }
  cell_map.insert("id", cell_id)?;
  cell_map.insert("container_id", cell_map.id().to_string())?;
  cell_map.insert("row_id", row_id)?;
  cell_map.insert("column_id", column_id)?;
  cell_map.insert("row_span", i64::from(cell.row_span))?;
  cell_map.insert("column_span", i64::from(cell.col_span))?;
  let attrs = cell_map.ensure_mergeable_map("attrs")?;
  cell_map.insert("attrs_container_id", attrs.id().to_string())?;
  let flow_id = map_string_opt(cell_map, "flow_id").unwrap_or_else(|| format!("{cell_id}.flow"));
  cell_map.insert("flow_id", flow_id.as_str())?;
  let flow = ensure_flow(doc, &flow_id, "table_cell")?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  cell_map.insert("flow_container_id", flow.id().to_string())?;
  cell_map.insert("text_container_id", text.id().to_string())?;

  let paragraphs = cell
    .blocks
    .iter()
    .filter_map(|block| match block {
      InputTableCellBlock::Paragraph(paragraph) => Some(paragraph),
      InputTableCellBlock::Table(_) => None,
    })
    .collect::<Vec<_>>();
  let desired = if paragraphs.is_empty() {
    SENTINEL_NEWLINE.to_string()
  } else {
    let mut desired = String::from(SENTINEL_NEWLINE);
    for (paragraph_ix, paragraph) in paragraphs.iter().enumerate() {
      if paragraph_ix > 0 {
        desired.push('\n');
      }
      for run in &paragraph.runs {
        desired.push_str(&run.text);
      }
    }
    desired
  };
  replace_text_incrementally(&text, &desired)?;
  let len = text.len_unicode();
  for key in [
    MARK_PARAGRAPH_STYLE,
    MARK_RUN_SEMANTIC_STYLE,
    MARK_HIGHLIGHT_STYLE,
    MARK_DIRECT_UNDERLINE,
    MARK_STRIKETHROUGH,
  ] {
    text.unmark(0..len, key)?;
  }
  if paragraphs.is_empty() {
    text.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(ParagraphStyle::Normal))?;
    return Ok(());
  }
  let mut cursor = 0usize;
  for paragraph in paragraphs {
    text.mark(cursor..cursor + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))?;
    cursor += 1;
    for run in &paragraph.runs {
      let run_len = run.text.chars().count();
      if run_len > 0 {
        mark_run_styles(&text, cursor..cursor + run_len, run.styles)?;
      }
      cursor += run_len;
    }
  }
  Ok(())
}

fn replace_text_incrementally(text: &LoroText, desired: &str) -> loro::LoroResult<()> {
  let current = text.to_string();
  if current == desired {
    return Ok(());
  }
  let current_chars = current.chars().collect::<Vec<_>>();
  let desired_chars = desired.chars().collect::<Vec<_>>();
  let prefix = current_chars
    .iter()
    .zip(&desired_chars)
    .take_while(|(left, right)| left == right)
    .count();
  let suffix = current_chars
    .iter()
    .skip(prefix)
    .rev()
    .zip(desired_chars.iter().skip(prefix).rev())
    .take_while(|(left, right)| left == right)
    .count();
  let delete_len = current_chars.len().saturating_sub(prefix + suffix);
  if delete_len > 0 {
    text.delete(prefix, delete_len)?;
  }
  let insert_end = desired_chars.len().saturating_sub(suffix);
  if insert_end > prefix {
    let insert = desired_chars[prefix..insert_end].iter().collect::<String>();
    text.insert(prefix, &insert)?;
  }
  Ok(())
}

fn write_table_column_width(column: &LoroMap, width: &InputTableColumnWidth) -> Result<()> {
  column.delete("width_px")?;
  column.delete("fraction")?;
  match width {
    InputTableColumnWidth::Auto => column.insert("width_kind", "auto")?,
    InputTableColumnWidth::FixedPx(px) => {
      column.insert("width_kind", "fixed_px")?;
      column.insert("width_px", i64::from(*px))?;
    },
    InputTableColumnWidth::Fraction(fraction) => {
      column.insert("width_kind", "fraction")?;
      column.insert("fraction", i64::from(*fraction))?;
    },
  };
  Ok(())
}

fn append_input_paragraph_text_only(text: &LoroText, paragraph: &InputParagraph) -> Result<()> {
  let use_existing_sentinel = text.len_unicode() == 1 && text.to_string() == SENTINEL_NEWLINE;
  let boundary_pos = if use_existing_sentinel {
    0
  } else {
    let pos = text.len_unicode();
    text.insert(pos, "\n")?;
    pos
  };
  text.mark(
    boundary_pos..boundary_pos + 1,
    MARK_PARAGRAPH_STYLE,
    paragraph_style_value(paragraph.style),
  )?;
  for run in &paragraph.runs {
    if run.text.is_empty() {
      continue;
    }
    let start = text.len_unicode();
    text.insert(start, &run.text)?;
    let len = run.text.chars().count();
    mark_run_styles(text, start..start + len, run.styles)?;
  }
  Ok(())
}

fn clear_map(map: &LoroMap) -> loro::LoroResult<()> {
  // §29: rebuild/fallback paths use Loro's native container clear instead of a
  // manual per-key delete loop. Behavior is identical, just native.
  map.clear()
}

fn clear_movable_list(list: &LoroMovableList) -> loro::LoroResult<()> {
  // §29: native clear instead of `delete(0, len)`; identical effect.
  list.clear()
}

fn projection_offset_to_body_unicode_index(projection: &DocumentProjection, offset: flowstate_document::DocumentOffset) -> usize {
  ProjectionRuntimeIndex::from_projection(projection)
    .body_unicode_for_offset(projection, offset)
    .unwrap_or(1)
}

/// Loro-space counterpart of [`projection_offset_to_body_unicode_index`], for callers
/// that feed the result into a live Loro body mutation (`insert`/`delete`/`mark`).
/// Resolves via each paragraph's durable boundary cursor so coalesced object-adjacent
/// empty paragraphs (dropped from the projection but still present in the body) don't
/// shift the position; see [`ProjectionRuntimeIndex::body_unicode_for_offset_in_loro`].
fn projection_offset_to_loro_body_unicode_index(doc: &LoroDoc, projection: &DocumentProjection, offset: flowstate_document::DocumentOffset) -> usize {
  ProjectionRuntimeIndex::from_projection(projection)
    .body_unicode_for_offset_in_loro(doc, projection, offset)
    .unwrap_or(1)
}

fn clamp_projection_offset(projection: &DocumentProjection, offset: DocumentOffset) -> DocumentOffset {
  let paragraph = offset
    .paragraph
    .min(projection.paragraphs.len().saturating_sub(1));
  let byte = projection
    .paragraphs
    .get(paragraph)
    .map(flowstate_document::paragraph_text_len)
    .unwrap_or_default()
    .min(offset.byte);
  DocumentOffset { paragraph, byte }
}

fn paragraph_boundary_unicode_index(projection: &DocumentProjection, paragraph_ix: usize) -> usize {
  if paragraph_ix == 0 {
    return 0;
  }
  projection_offset_to_body_unicode_index(
    projection,
    flowstate_document::DocumentOffset {
      paragraph: paragraph_ix,
      byte: 0,
    },
  ) - 1
}

fn join_projection_paragraphs(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  first: flowstate_document::ParagraphId,
  second: flowstate_document::ParagraphId,
) -> Result<bool> {
  let Some(first_ix) = projection
    .ids
    .paragraph_ids
    .iter()
    .position(|id| *id == first)
  else {
    tracing::warn!(
      ?first,
      ?second,
      "skipping JoinParagraphs because the first paragraph id is absent from the supplied projection"
    );
    return Ok(false);
  };
  let Some(second_ix) = projection
    .ids
    .paragraph_ids
    .iter()
    .position(|id| *id == second)
  else {
    tracing::warn!(
      ?first,
      ?second,
      "skipping JoinParagraphs because the second paragraph id is absent from the supplied projection"
    );
    return Ok(false);
  };
  if first_ix + 1 != second_ix {
    tracing::warn!(
      ?first,
      ?second,
      first_ix,
      second_ix,
      "skipping JoinParagraphs for non-adjacent paragraphs"
    );
    return Ok(false);
  }
  if !projection_paragraph_blocks_are_adjacent(projection, first_ix, second_ix) {
    tracing::warn!(
      ?first,
      ?second,
      first_ix,
      second_ix,
      "skipping JoinParagraphs because an object block separates the paragraphs and projection offsets are not object-aware",
    );
    return Ok(false);
  }

  let Some(second_block_ix) = flowstate_document::block_ix_for_paragraph(projection, second_ix) else {
    return Ok(false);
  };
  let Some(second_block) = projection.ids.block_ids.get(second_block_ix).copied() else {
    return Ok(false);
  };

  let boundary = paragraph_boundary_loro_unicode_index(doc, projection, second_ix);
  let body = body_text(doc);
  if !boundary_is_live(&body.to_string(), boundary) {
    tracing::warn!(
      ?first,
      ?second,
      boundary,
      "skipping JoinParagraphs because the computed Loro boundary is not a live paragraph newline",
    );
    return Ok(false);
  }
  // Atomicity: only drop the second paragraph's durable metadata once we have
  // committed to deleting its boundary. Deleting the record before the liveness
  // check could orphan the boundary (record gone, '\n' still live) when the join
  // then skips — a full reprojection fabricates a fresh id there and diverges from
  // the incremental projection, which still holds the record's original id.
  delete_projection_paragraph_metadata(doc, second, second_block)?;
  body
    .delete(boundary, 1)
    .context("deleting joined paragraph boundary from Loro body flow")?;
  repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[], "editor_join_paragraphs")?;
  Ok(true)
}

fn delete_projection_paragraph_metadata(doc: &LoroDoc, paragraph_id: ParagraphId, block_id: BlockId) -> loro::LoroResult<()> {
  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  for key in map_keys(&paragraphs) {
    let id = child_map(&paragraphs, &key)
      .and_then(|paragraph| map_string_opt(&paragraph, "id"))
      .unwrap_or_else(|| key.clone());
    if loro_id_u128(&id) == paragraph_id.0 {
      paragraphs.delete(&key)?;
    }
  }

  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  for key in map_keys(&blocks) {
    let id = child_map(&blocks, &key)
      .and_then(|block| map_string_opt(&block, "id"))
      .unwrap_or_else(|| key.clone());
    if loro_id_u128(&id) == block_id.0 {
      blocks.delete(&key)?;
    }
  }
  Ok(())
}

fn projection_paragraph_blocks_are_adjacent(projection: &DocumentProjection, first_ix: usize, second_ix: usize) -> bool {
  let Some(first_block_ix) = flowstate_document::block_ix_for_paragraph(projection, first_ix) else {
    return false;
  };
  let Some(second_block_ix) = flowstate_document::block_ix_for_paragraph(projection, second_ix) else {
    return false;
  };
  second_block_ix == first_block_ix + 1
}

fn replace_body_paragraph_span(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  start: Option<flowstate_document::DocumentOffset>,
  before: &flowstate_document::DocumentSpan,
  after: &flowstate_document::DocumentSpan,
) -> Result<bool> {
  if before.paragraphs.is_empty() && after.paragraphs.is_empty() {
    return Ok(false);
  }
  let start = projection_offset_to_body_unicode_index(
    projection,
    start.unwrap_or(flowstate_document::DocumentOffset {
      paragraph: before.start_paragraph,
      byte: 0,
    }),
  );
  let paragraph_texts = span_paragraph_texts(after);
  let replacement = paragraph_texts.join("\n");
  let before_chars = before.text.chars().collect::<Vec<_>>();
  let replacement_chars = replacement.chars().collect::<Vec<_>>();
  let common_prefix = before_chars
    .iter()
    .zip(&replacement_chars)
    .take_while(|(before, after)| before == after)
    .count();
  let max_suffix = before_chars
    .len()
    .min(replacement_chars.len())
    .saturating_sub(common_prefix);
  let common_suffix = before_chars
    .iter()
    .rev()
    .zip(replacement_chars.iter().rev())
    .take(max_suffix)
    .take_while(|(before, after)| before == after)
    .count();
  let before_changed_len = before_chars
    .len()
    .saturating_sub(common_prefix + common_suffix);
  let replacement_changed_end = replacement_chars.len().saturating_sub(common_suffix);
  let replacement_changed = replacement_chars[common_prefix..replacement_changed_end]
    .iter()
    .collect::<String>();
  // Drop the metadata for paragraphs this replacement REMOVES (ids in `before`
  // but not `after`) before touching the body text. Otherwise a removed
  // paragraph's record survives the edit, re-anchors its cursor to the following
  // paragraph's boundary, and — via the metadata prune's lexicographic tie-break
  // — can displace that paragraph's real id, diverging canonical block/paragraph
  // ids from the optimistic replay. This covers boundary-0 joins too, where
  // `overwrite_span_paragraph_metadata_ids` deliberately leaves the reserved root
  // record alone and so cannot clean an orphaned sibling.
  delete_removed_span_metadata(doc, before, after)?;
  let body = body_text(doc);
  let start = start.min(body.len_unicode());
  let change_start = start.saturating_add(common_prefix).min(body.len_unicode());
  let change_end = change_start
    .saturating_add(before_changed_len)
    .min(body.len_unicode());
  if change_end > change_start {
    body.delete(change_start, change_end - change_start)?;
  }
  if !replacement_changed.is_empty() {
    body.insert(change_start, &replacement_changed)?;
  }
  let first_boundary = start.saturating_sub(1);
  mark_replacement_span(&body, first_boundary, start, after, &paragraph_texts)?;
  let boundaries = replacement_span_boundaries(first_boundary, start, &paragraph_texts);
  repair_paragraph_metadata_after_text_flow_edit(doc, &body, &boundaries, "editor_replace_paragraph_span")?;
  // Crux of the identity-divergence fix: the text-flow repair above lets Loro
  // metadata survival decide which durable id lives at each resulting boundary,
  // which can disagree with the editor's optimistic replay. Overwrite each
  // boundary's paragraph/block record with the editor-supplied ids so canonical
  // == predicted == optimistic by construction.
  overwrite_span_paragraph_metadata_ids(doc, &body, &boundaries, &after.paragraph_ids, &after.block_ids)?;
  Ok(true)
}

/// Deletes the Loro paragraph and paragraph-block metadata records for paragraphs
/// a span replacement REMOVES — durable ids present in `before` but absent from
/// `after`, compared by id rather than position.
///
/// A removed paragraph's record would otherwise survive the body edit, re-anchor
/// its cursor onto the following (surviving) paragraph's boundary, and get kept by
/// [`prune_stale_paragraph_metadata`]'s lexicographic tie-break in place of that
/// paragraph's real record — so the canonical projection reads the removed id at
/// the shifted index while the optimistic replay reads the correct one. Deleting
/// the record up front makes the survivor unambiguous. Records are matched on the
/// stored `id` field (falling back to the map key) in `PARAGRAPHS_BY_ID` /
/// `BLOCKS_BY_ID`, exactly like [`delete_projection_paragraph_metadata`]. The
/// reserved boundary-0 anchors ([`ROOT_FIRST_PARAGRAPH_ID`]/[`MAIN_BODY_BLOCK_ID`])
/// are never deleted; other machinery keys off those names and boundary 0's id
/// convergence is handled separately.
fn delete_removed_span_metadata(
  doc: &LoroDoc,
  before: &flowstate_document::DocumentSpan,
  after: &flowstate_document::DocumentSpan,
) -> loro::LoroResult<()> {
  let removed_paragraph_ids = before
    .paragraph_ids
    .iter()
    .copied()
    .filter(|id| !after.paragraph_ids.contains(id))
    .map(|id| id.0)
    .collect::<Vec<u128>>();
  let removed_block_ids = before
    .block_ids
    .iter()
    .copied()
    .filter(|id| !after.block_ids.contains(id))
    .map(|id| id.0)
    .collect::<Vec<u128>>();
  if removed_paragraph_ids.is_empty() && removed_block_ids.is_empty() {
    return Ok(());
  }

  let root = doc.get_map(ROOT);
  if !removed_paragraph_ids.is_empty() {
    let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
    for key in map_keys(&paragraphs) {
      if key == ROOT_FIRST_PARAGRAPH_ID {
        continue;
      }
      let id = child_map(&paragraphs, &key)
        .and_then(|paragraph| map_string_opt(&paragraph, "id"))
        .unwrap_or_else(|| key.clone());
      if removed_paragraph_ids.contains(&loro_id_u128(&id)) {
        paragraphs.delete(&key)?;
      }
    }
  }
  if !removed_block_ids.is_empty() {
    let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
    for key in map_keys(&blocks) {
      if key == MAIN_BODY_BLOCK_ID {
        continue;
      }
      let id = child_map(&blocks, &key)
        .and_then(|block| map_string_opt(&block, "id"))
        .unwrap_or_else(|| key.clone());
      if removed_block_ids.contains(&loro_id_u128(&id)) {
        blocks.delete(&key)?;
      }
    }
  }
  Ok(())
}

/// Forces the canonical paragraph/block metadata at each resulting span boundary
/// to the editor-supplied durable ids (`ReplaceParagraphSpan`'s `after` span).
///
/// [`repair_paragraph_metadata_after_text_flow_edit`] normalizes the replaced run
/// to exactly one paragraph and one block record per boundary, but the id that
/// survives is chosen by Loro cursor survival + lexicographic tie-break. That can
/// keep a different id than the optimistic positional replay assigned (e.g. a
/// join that drops a middle boundary), diverging the two projections and later
/// dropping pending edits that reference the id the other side kept.
///
/// Mirrors [`repair_paragraph_metadata_after_stable_split`]'s forced-id write via
/// [`ensure_paragraph_metadata_at_boundary_with_ids`], extended to every boundary
/// of the run: it writes the forced record and drops the pre-existing survivor so
/// the projection reads back exactly `after.paragraph_ids[i]`/`after.block_ids[i]`.
/// Reserved boundary-0 root records already converge numerically with the client's
/// first-paragraph id and are keyed on by other machinery, so they are left as-is.
fn overwrite_span_paragraph_metadata_ids(
  doc: &LoroDoc,
  body: &loro::LoroText,
  boundaries: &[usize],
  paragraph_ids: &[flowstate_document::ParagraphId],
  block_ids: &[flowstate_document::BlockId],
) -> loro::LoroResult<()> {
  let body_snapshot = body.to_string();
  for (ix, &boundary) in boundaries.iter().enumerate() {
    let (Some(&paragraph_id), Some(&block_id)) = (paragraph_ids.get(ix), block_ids.get(ix)) else {
      continue;
    };
    if !boundary_is_live(&body_snapshot, boundary) {
      continue;
    }
    let root = doc.get_map(ROOT);
    let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
    let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
    // The prior repair pass left exactly one paragraph/block record live at this
    // boundary; find it (mirroring the projection's own selection) before writing
    // the forced record so the loser can be dropped afterwards.
    let existing_paragraph_key = paragraph_metadata_key_at_boundary(doc, body, &paragraphs, boundary);
    let existing_block_key = paragraph_block_key_at_boundary(doc, body, &blocks, boundary);
    // Boundary 0's reserved root records already converge numerically with the
    // client's first-paragraph id, and other machinery keys off those reserved
    // names, so never rewrite or delete them.
    if existing_paragraph_key.as_deref() == Some(ROOT_FIRST_PARAGRAPH_ID) || existing_block_key.as_deref() == Some(MAIN_BODY_BLOCK_ID) {
      continue;
    }
    let paragraph_matches = existing_paragraph_key.as_deref().map(loro_id_u128) == Some(paragraph_id.0);
    let block_matches = existing_block_key.as_deref().map(loro_id_u128) == Some(block_id.0);
    if paragraph_matches && block_matches {
      continue;
    }
    ensure_paragraph_metadata_at_boundary_with_ids(doc, body, boundary, paragraph_id, block_id)?;
    if let Some(old) = existing_paragraph_key
      && loro_id_u128(&old) != paragraph_id.0
    {
      paragraphs.delete(&old)?;
    }
    if let Some(old) = existing_block_key
      && loro_id_u128(&old) != block_id.0
    {
      blocks.delete(&old)?;
    }
  }
  Ok(())
}

fn span_paragraph_texts(span: &flowstate_document::DocumentSpan) -> Vec<String> {
  let mut offset = 0_usize;
  span
    .paragraphs
    .iter()
    .enumerate()
    .map(|(paragraph_ix, paragraph)| {
      if paragraph_ix > 0
        && span
          .text
          .get(offset..)
          .is_some_and(|text| text.starts_with('\n'))
      {
        offset += '\n'.len_utf8();
      }
      let len = flowstate_document::paragraph_text_len(paragraph);
      let end = offset.saturating_add(len).min(span.text.len());
      let text = span.text.get(offset..end).unwrap_or_default().to_string();
      offset = end;
      text
    })
    .collect()
}

fn mark_replacement_span(
  body: &loro::LoroText,
  first_boundary_unicode: usize,
  text_start_unicode: usize,
  span: &flowstate_document::DocumentSpan,
  paragraph_texts: &[String],
) -> loro::LoroResult<()> {
  if let Some(first) = span.paragraphs.first() {
    body.mark(
      first_boundary_unicode..first_boundary_unicode + 1,
      MARK_PARAGRAPH_STYLE,
      paragraph_style_value(first.style),
    )?;
  }
  mark_projected_paragraphs(body, text_start_unicode, &span.paragraphs, paragraph_texts)
}

fn mark_projected_paragraphs(
  body: &loro::LoroText,
  text_start_unicode: usize,
  paragraphs: &[flowstate_document::Paragraph],
  paragraph_texts: &[String],
) -> loro::LoroResult<()> {
  let mut paragraph_start = text_start_unicode;
  for (paragraph_ix, (paragraph, paragraph_text)) in paragraphs.iter().zip(paragraph_texts).enumerate() {
    if paragraph_ix > 0 {
      let boundary = paragraph_start.saturating_sub(1);
      body.mark(boundary..boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))?;
    }
    mark_paragraph_runs(body, paragraph_start, paragraph_text, &paragraph.runs)?;
    paragraph_start += paragraph_text.chars().count() + 1;
  }
  Ok(())
}

fn mark_paragraph_runs(
  body: &loro::LoroText,
  paragraph_start_unicode: usize,
  paragraph_text: &str,
  runs: &[flowstate_document::TextRun],
) -> loro::LoroResult<()> {
  let mut byte_offset = 0_usize;
  for run in runs {
    let end = byte_offset
      .saturating_add(run.len)
      .min(paragraph_text.len());
    let Some(run_text) = paragraph_text.get(byte_offset..end) else {
      break;
    };
    let run_len = run_text.chars().count();
    if run_len > 0 {
      let run_start = paragraph_start_unicode
        + paragraph_text
          .get(..byte_offset)
          .unwrap_or_default()
          .chars()
          .count();
      mark_run_styles(body, run_start..run_start + run_len, run.styles)?;
    }
    byte_offset = end;
  }
  Ok(())
}

fn inserted_newline_boundaries(start_unicode: usize, text: &str) -> Vec<usize> {
  text
    .chars()
    .enumerate()
    .filter_map(|(offset, ch)| (ch == '\n').then_some(start_unicode + offset))
    .collect()
}

fn persist_body_paragraph_style_mark_repair(doc: &LoroDoc, package: Option<&mut DocumentPackage>, package_path: Option<&Path>) -> Result<bool> {
  let from_frontier = doc.state_frontiers();
  let from_vv = doc.state_vv();
  let replica_registered = flowstate_document::register_replica(doc, None)?;
  let paragraph_marks_repaired = repair_missing_paragraph_style_marks(doc)?;
  if !replica_registered && !paragraph_marks_repaired {
    return Ok(false);
  }
  let Some(package) = package else {
    return Ok(paragraph_marks_repaired);
  };
  package.sync_revisions_from_loro(doc)?;
  let update = doc
    .export(ExportMode::updates(&from_vv))
    .context("exporting paragraph style repair update")?;
  if !update.is_empty() {
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    package.compact_update_segments_if_needed(doc, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD)?;
  }
  package.rebuild_search_units_from_loro(doc)?;
  if let Some(path) = package_path {
    package.write(path)?;
  }
  Ok(paragraph_marks_repaired)
}

fn repair_missing_paragraph_style_marks(doc: &LoroDoc) -> Result<bool> {
  let root = doc.get_map(ROOT);
  let Some(flows) = child_map(&root, FLOWS_BY_ID) else {
    return Ok(false);
  };
  let mut repaired = false;
  for flow_id in map_keys(&flows) {
    let Some(flow) = child_map(&flows, &flow_id) else {
      continue;
    };
    if !matches!(
      map_string_opt(&flow, FLOW_KIND_KEY).as_deref(),
      Some("body" | "table_cell" | "caption" | "header" | "footer")
    ) {
      continue;
    }
    let Some(ValueOrContainer::Container(Container::Text(text))) = flow.get(FLOW_TEXT_KEY) else {
      continue;
    };
    for boundary in body_paragraph_boundaries_missing_style_mark(&text) {
      text
        .mark(
          boundary..boundary + 1,
          MARK_PARAGRAPH_STYLE,
          paragraph_style_value(ParagraphStyle::Normal),
        )
        .context("repairing missing paragraph style mark")?;
      repaired = true;
    }
  }
  if repaired {
    doc.commit();
  }
  Ok(repaired)
}

fn body_paragraph_boundaries_missing_style_mark(body: &loro::LoroText) -> Vec<usize> {
  let mut missing = Vec::new();
  let mut unicode_pos = 0_usize;
  for item in body.to_delta() {
    let loro::TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    let has_paragraph_style = paragraph_style_from_attrs(attributes.as_ref()).is_some();
    for ch in insert.chars() {
      if ch == '\n' && !has_paragraph_style {
        missing.push(unicode_pos);
      }
      unicode_pos += 1;
    }
  }
  missing
}

fn replacement_span_boundaries(first_boundary_unicode: usize, text_start_unicode: usize, paragraph_texts: &[String]) -> Vec<usize> {
  if paragraph_texts.is_empty() {
    return Vec::new();
  }
  let mut boundaries = Vec::with_capacity(paragraph_texts.len());
  boundaries.push(first_boundary_unicode);
  let mut paragraph_start = text_start_unicode;
  for (paragraph_ix, paragraph_text) in paragraph_texts.iter().enumerate() {
    if paragraph_ix > 0 {
      boundaries.push(paragraph_start.saturating_sub(1));
    }
    paragraph_start += paragraph_text.chars().count() + 1;
  }
  boundaries
}

fn repair_paragraph_metadata_after_text_flow_edit(
  doc: &LoroDoc,
  body: &loro::LoroText,
  live_boundaries: &[usize],
  reason: &'static str,
) -> loro::LoroResult<()> {
  for boundary in live_boundaries {
    ensure_paragraph_metadata_at_boundary(doc, body, *boundary)?;
  }
  let pruned = prune_stale_paragraph_metadata(doc, body)?;
  if pruned.changed() {
    tracing::warn!(
      reason,
      stale_paragraphs = pruned.stale_paragraphs,
      duplicate_paragraphs = pruned.duplicate_paragraphs,
      stale_blocks = pruned.stale_blocks,
      duplicate_blocks = pruned.duplicate_blocks,
      "pruned stale Loro paragraph metadata after text-flow edit",
    );
  }
  Ok(())
}

fn repair_paragraph_metadata_after_stable_split(
  doc: &LoroDoc,
  body: &loro::LoroText,
  boundary: usize,
  paragraph_id: flowstate_document::ParagraphId,
  block_id: flowstate_document::BlockId,
  reason: &'static str,
) -> loro::LoroResult<()> {
  ensure_paragraph_metadata_at_boundary_with_ids(doc, body, boundary, paragraph_id, block_id)?;
  let pruned = prune_stale_paragraph_metadata(doc, body)?;
  if pruned.changed() {
    tracing::warn!(
      reason,
      stale_paragraphs = pruned.stale_paragraphs,
      duplicate_paragraphs = pruned.duplicate_paragraphs,
      stale_blocks = pruned.stale_blocks,
      duplicate_blocks = pruned.duplicate_blocks,
      "pruned stale Loro paragraph metadata after stable split",
    );
  }
  Ok(())
}

fn ensure_paragraph_metadata_at_boundary(doc: &LoroDoc, body: &loro::LoroText, boundary: usize) -> loro::LoroResult<()> {
  ensure_paragraph_metadata_at_boundary_with_keys(doc, body, boundary, None, None)
}

fn ensure_paragraph_metadata_at_boundary_with_ids(
  doc: &LoroDoc,
  body: &loro::LoroText,
  boundary: usize,
  paragraph_id: flowstate_document::ParagraphId,
  block_id: flowstate_document::BlockId,
) -> loro::LoroResult<()> {
  ensure_paragraph_metadata_at_boundary_with_keys(
    doc,
    body,
    boundary,
    Some(format!("paragraph.{}", paragraph_id.0)),
    Some(format!("paragraph_block.{}", block_id.0)),
  )
}

fn ensure_paragraph_metadata_at_boundary_with_keys(
  doc: &LoroDoc,
  body: &loro::LoroText,
  boundary: usize,
  forced_paragraph_id: Option<String>,
  forced_block_id: Option<String>,
) -> loro::LoroResult<()> {
  let body_snapshot = body.to_string();
  if !boundary_is_live(&body_snapshot, boundary) {
    tracing::warn!(
      boundary,
      "cannot create paragraph metadata because boundary is not a live paragraph newline"
    );
    return Ok(());
  }

  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraph_id = forced_paragraph_id
    .or_else(|| paragraph_metadata_key_at_boundary(doc, body, &paragraphs, boundary))
    .unwrap_or_else(|| new_paragraph_metadata_id(boundary));
  let paragraph = paragraphs.ensure_mergeable_map(&paragraph_id)?;
  paragraph.insert("id", paragraph_id.as_str())?;
  paragraph.insert("container_id", paragraph.id().to_string())?;
  paragraph.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    paragraph.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    paragraph.insert("boundary_cursor", cursor.encode())?;
  }
  let paragraph_attrs = paragraph.ensure_mergeable_map("attrs")?;
  paragraph.insert("attrs_container_id", paragraph_attrs.id().to_string())?;

  let block_id = forced_block_id
    .or_else(|| paragraph_block_key_at_boundary(doc, body, &blocks, boundary))
    .unwrap_or_else(|| new_paragraph_block_id(boundary));
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("container_id", block.id().to_string())?;
  block.insert("kind", "paragraph")?;
  block.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  block.insert("paragraph_id", paragraph_id.as_str())?;
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let block_attrs = block.ensure_mergeable_map("attrs")?;
  let nested_refs = block.ensure_mergeable_map("nested_refs")?;
  block.insert("attrs_container_id", block_attrs.id().to_string())?;
  block.insert("nested_refs_container_id", nested_refs.id().to_string())?;
  Ok(())
}

fn paragraph_metadata_key_at_boundary(doc: &LoroDoc, body: &loro::LoroText, paragraphs: &LoroMap, boundary: usize) -> Option<String> {
  let mut keys = metadata_keys_at_boundary(doc, body, paragraphs, "boundary_cursor", boundary);
  if boundary == 0
    && let Some(root_ix) = keys.iter().position(|key| key == ROOT_FIRST_PARAGRAPH_ID)
  {
    return Some(keys.swap_remove(root_ix));
  }
  keys.into_iter().next()
}

fn paragraph_block_key_at_boundary(doc: &LoroDoc, body: &loro::LoroText, blocks: &LoroMap, boundary: usize) -> Option<String> {
  // `boundary` is already validated live by every caller, so the resolved position
  // only has to equal it — a single-element live set gives that test in O(1).
  let pos_by_id = boundary_cursor_positions(doc, body, blocks, &["anchor_cursor"]);
  let live = [boundary];
  let mut keys = Vec::new();
  for key in map_keys(blocks) {
    let Some(block) = child_map(blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("paragraph") {
      continue;
    }
    if live_cursor_pos(doc, &live, &pos_by_id, &block, "anchor_cursor") == Some(boundary) {
      keys.push(key);
    }
  }
  if boundary == 0
    && let Some(main_ix) = keys.iter().position(|key| key == MAIN_BODY_BLOCK_ID)
  {
    return Some(keys.swap_remove(main_ix));
  }
  keys.into_iter().next()
}

fn metadata_keys_at_boundary(doc: &LoroDoc, body: &loro::LoroText, maps: &LoroMap, cursor_key: &str, boundary: usize) -> Vec<String> {
  // `boundary` is already validated live by callers, so a single-element live set
  // reduces the per-record check to `resolved position == boundary` in O(1).
  let pos_by_id = boundary_cursor_positions(doc, body, maps, &[cursor_key]);
  let live = [boundary];
  map_keys(maps)
    .into_iter()
    .filter(|key| {
      child_map(maps, key)
        .as_ref()
        .and_then(|map| live_cursor_pos(doc, &live, &pos_by_id, map, cursor_key))
        == Some(boundary)
    })
    .collect()
}

#[derive(Default)]
struct ParagraphMetadataPrune {
  stale_paragraphs: usize,
  duplicate_paragraphs: usize,
  stale_blocks: usize,
  duplicate_blocks: usize,
}

impl ParagraphMetadataPrune {
  fn changed(&self) -> bool {
    self.stale_paragraphs > 0 || self.duplicate_paragraphs > 0 || self.stale_blocks > 0 || self.duplicate_blocks > 0
  }
}

fn prune_stale_paragraph_metadata(doc: &LoroDoc, body: &loro::LoroText) -> loro::LoroResult<ParagraphMetadataPrune> {
  let body_snapshot = body.to_string();
  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let mut pruned = ParagraphMetadataPrune::default();

  // Resolve every record's boundary in one batched pass instead of an O(records)
  // `get_cursor_pos` per record: `live_boundaries` validates liveness in O(log N),
  // and the two `*_pos` indexes give O(1) position lookups. `block_pos` is reused by
  // both block loops (the block registry is untouched between them); `paragraph_pos`
  // covers both the `boundary_cursor` and `start_cursor` fields.
  let live_boundaries = live_boundary_positions(&body_snapshot);
  let block_pos = boundary_cursor_positions(doc, body, &blocks, &["anchor_cursor"]);
  let paragraph_pos = boundary_cursor_positions(doc, body, &paragraphs, &["boundary_cursor", "start_cursor"]);

  let mut block_boundary_by_paragraph = FxHashMap::<String, usize>::default();
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("paragraph") {
      continue;
    }
    let Some(paragraph_id) = map_string_opt(&block, "paragraph_id") else {
      continue;
    };
    let Some(boundary) = live_cursor_pos(doc, &live_boundaries, &block_pos, &block, "anchor_cursor") else {
      continue;
    };
    block_boundary_by_paragraph
      .entry(paragraph_id)
      .or_insert(boundary);
  }

  let mut paragraph_by_boundary = BTreeMap::<usize, String>::new();
  let mut paragraphs_to_delete = Vec::new();
  for key in map_keys(&paragraphs) {
    let Some(paragraph) = child_map(&paragraphs, &key) else {
      paragraphs_to_delete.push(key);
      pruned.stale_paragraphs += 1;
      continue;
    };
    let boundary = live_cursor_pos(doc, &live_boundaries, &paragraph_pos, &paragraph, "boundary_cursor")
      .or_else(|| live_cursor_pos(doc, &live_boundaries, &paragraph_pos, &paragraph, "start_cursor"))
      .or_else(|| {
        let boundary = block_boundary_by_paragraph.get(&key).copied()?;
        repair_paragraph_boundary_cursors(body, &paragraph, boundary).ok()?;
        Some(boundary)
      });
    let Some(boundary) = boundary else {
      paragraphs_to_delete.push(key);
      pruned.stale_paragraphs += 1;
      continue;
    };
    if let Some(existing) = paragraph_by_boundary.get(&boundary) {
      if prefer_paragraph_metadata_key(boundary, existing, &key) {
        paragraphs_to_delete.push(existing.clone());
        paragraph_by_boundary.insert(boundary, key);
      } else {
        paragraphs_to_delete.push(key);
      }
      pruned.duplicate_paragraphs += 1;
    } else {
      paragraph_by_boundary.insert(boundary, key);
    }
  }
  for key in paragraphs_to_delete {
    paragraphs.delete(&key)?;
  }

  let mut block_by_boundary = BTreeMap::<usize, String>::new();
  let mut blocks_to_delete = Vec::new();
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("paragraph") {
      continue;
    }
    let Some(boundary) = live_cursor_pos(doc, &live_boundaries, &block_pos, &block, "anchor_cursor") else {
      blocks_to_delete.push(key);
      pruned.stale_blocks += 1;
      continue;
    };
    if let Some(existing) = block_by_boundary.get(&boundary) {
      if prefer_paragraph_block_key(boundary, existing, &key) {
        blocks_to_delete.push(existing.clone());
        block_by_boundary.insert(boundary, key);
      } else {
        blocks_to_delete.push(key);
      }
      pruned.duplicate_blocks += 1;
    } else {
      block_by_boundary.insert(boundary, key);
    }
  }
  for key in blocks_to_delete {
    blocks.delete(&key)?;
  }

  Ok(pruned)
}

fn repair_paragraph_boundary_cursors(body: &loro::LoroText, paragraph: &LoroMap, boundary: usize) -> loro::LoroResult<()> {
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    paragraph.insert("boundary_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    paragraph.insert("start_cursor", cursor.encode())?;
  }
  Ok(())
}

fn prefer_paragraph_metadata_key(boundary: usize, existing: &str, candidate: &str) -> bool {
  boundary == 0 && candidate == ROOT_FIRST_PARAGRAPH_ID && existing != ROOT_FIRST_PARAGRAPH_ID
}

fn prefer_paragraph_block_key(boundary: usize, existing: &str, candidate: &str) -> bool {
  boundary == 0 && candidate == MAIN_BODY_BLOCK_ID && existing != MAIN_BODY_BLOCK_ID
}

/// Resolve every live boundary cursor stored under `cursor_fields` across `records`
/// in a SINGLE pass, returning an `id → position` map. Mirrors the batch resolver in
/// `flowstate_document::loro_projection` (`boundary_cursor_positions`): each record
/// contributes an O(1) cursor decode, and the whole set of positions is resolved by
/// one vendored-Loro `query_text_id_positions` chunk scan instead of an O(records)
/// history-traced `get_cursor_pos` per record. That is what removes the O(records²)
/// scan which pinned the CRDT actor at 100% CPU — and drove the unbounded allocation
/// that OOM-killed the host — while editing a large document. Ids not present
/// (deleted) are simply absent; [`live_cursor_pos`] falls back to per-id
/// `get_cursor_pos` for those, preserving exact parity with the old scan.
fn boundary_cursor_positions(doc: &LoroDoc, body: &loro::LoroText, records: &LoroMap, cursor_fields: &[&str]) -> FxHashMap<ID, usize> {
  let container = body.id();
  let mut ids: Vec<ID> = Vec::new();
  for key in map_keys(records) {
    let Some(record) = child_map(records, &key) else {
      continue;
    };
    for field in cursor_fields {
      if let Some(bytes) = map_binary_opt(&record, field)
        && let Ok(cursor) = Cursor::decode(&bytes)
        && cursor.container == container
        && let Some(id) = cursor.id
      {
        ids.push(id);
      }
    }
  }
  let mut positions = FxHashMap::default();
  if ids.is_empty() {
    return positions;
  }
  for (id, pos) in ids.iter().copied().zip(doc.inner().query_text_id_positions(&container, &ids)) {
    if let Some(pos) = pos {
      positions.insert(id, pos);
    }
  }
  positions
}

/// Sorted Unicode-code-point indices of every paragraph-boundary newline in
/// `body_snapshot`, built in one O(N) pass so boundary-liveness can be tested with an
/// O(log N) `binary_search` instead of a per-record O(N) `chars().nth(pos)` — the
/// second quadratic factor (alongside `get_cursor_pos`) in the old per-record scan.
fn live_boundary_positions(body_snapshot: &str) -> Vec<usize> {
  body_snapshot
    .chars()
    .enumerate()
    .filter_map(|(i, c)| (c == '\n').then_some(i))
    .collect()
}

/// Resolve one record's boundary cursor to its live position. `pos_by_id` (built by
/// [`boundary_cursor_positions`]) gives an O(1) hit for every live cursor; only a
/// cursor whose id is no longer live falls back to the history-traced `get_cursor_pos`
/// (exact parity with the old path). The resolved position must land on a member of
/// the sorted `live_boundaries` set to count — pass the full newline set to validate
/// against the whole document, or a single-element slice to test one already-validated
/// live boundary.
fn live_cursor_pos(doc: &LoroDoc, live_boundaries: &[usize], pos_by_id: &FxHashMap<ID, usize>, map: &LoroMap, cursor_key: &str) -> Option<usize> {
  let cursor = Cursor::decode(&map_binary_opt(map, cursor_key)?).ok()?;
  let pos = match cursor.id.and_then(|id| pos_by_id.get(&id).copied()) {
    Some(pos) => pos,
    None => doc.get_cursor_pos(&cursor).ok()?.current.pos,
  };
  live_boundaries.binary_search(&pos).is_ok().then_some(pos)
}

fn live_object_cursor_pos(doc: &LoroDoc, body_snapshot: &str, map: &LoroMap, cursor_key: &str) -> Option<usize> {
  let cursor = Cursor::decode(&map_binary_opt(map, cursor_key)?).ok()?;
  let pos = doc.get_cursor_pos(&cursor).ok()?.current.pos;
  (body_snapshot.chars().nth(pos) == Some(OBJECT_REPLACEMENT)).then_some(pos)
}

fn boundary_is_live(body_snapshot: &str, boundary: usize) -> bool {
  body_snapshot.chars().nth(boundary) == Some('\n')
}

fn new_paragraph_metadata_id(boundary: usize) -> String {
  if boundary == 0 {
    ROOT_FIRST_PARAGRAPH_ID.to_string()
  } else {
    format!("paragraph.{}", Uuid::new_v4().as_u128())
  }
}

fn new_paragraph_block_id(boundary: usize) -> String {
  if boundary == 0 {
    MAIN_BODY_BLOCK_ID.to_string()
  } else {
    format!("paragraph_block.{}", Uuid::new_v4().as_u128())
  }
}

fn map_keys(map: &LoroMap) -> Vec<String> {
  let mut keys = map.keys().map(|key| key.to_string()).collect::<Vec<_>>();
  keys.sort();
  keys
}

fn child_map(parent: &LoroMap, key: &str) -> Option<LoroMap> {
  parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  })
}

/// §28: centralized resolution of a stored raw Loro container id string.
///
/// Parses the durable `*_container_id` string into a [`ContainerID`] and fetches
/// the live container directly from the document for efficient runtime access.
/// Returns `None` when the id is missing/unparseable or the container is
/// absent/detached/deleted, so callers can fall back to map-key traversal.
fn container_by_id(doc: &LoroDoc, container_id: &str) -> Option<Container> {
  let container = doc.get_container(ContainerID::try_from(container_id).ok()?)?;
  (container.is_attached() && !container.is_deleted()).then_some(container)
}

fn container_text_by_id(doc: &LoroDoc, container_id: &str) -> Option<LoroText> {
  container_by_id(doc, container_id)?.into_text().ok()
}

/// §28: resolve a flow's canonical `LoroText`, preferring direct resolution via
/// the flow's stored `text_container_id` and falling back to key traversal when
/// the id is missing/unresolvable.
fn flow_text(doc: &LoroDoc, flow: &LoroMap) -> loro::LoroResult<LoroText> {
  if let Some(container_id) = map_string_opt(flow, "text_container_id")
    && let Some(text) = container_text_by_id(doc, &container_id)
  {
    return Ok(text);
  }
  flow.ensure_mergeable_text(FLOW_TEXT_KEY)
}

fn child_movable_list(parent: &LoroMap, key: &str) -> Option<LoroMovableList> {
  parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(Container::MovableList(list)) => Some(list),
    _ => None,
  })
}

fn movable_list_strings(list: &LoroMovableList) -> Vec<String> {
  (0..list.len())
    .filter_map(|ix| match list.get(ix) {
      Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
      _ => None,
    })
    .collect()
}

fn map_string_opt(map: &LoroMap, key: &str) -> Option<String> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  })
}

fn map_binary_opt(map: &LoroMap, key: &str) -> Option<Vec<u8>> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Binary(value)) => Some(value.to_vec()),
    _ => None,
  })
}

fn attach_package_assets(document: &mut DocumentProjection, package: &DocumentPackage) {
  flowstate_document::attach_package_assets(document, &package.assets);
}

#[derive(Debug, Default)]
struct AssetMergeSummary {
  any_changed: bool,
  metadata_changed: bool,
  changed_asset_ids: Vec<String>,
}

fn merge_asset_records_into_projection(projection: &mut DocumentProjection, records: &[AssetRecord]) -> AssetMergeSummary {
  let mut summary = AssetMergeSummary::default();
  for record in records {
    let existing = projection.assets.assets.get(&record.id);
    let metadata_changed = existing.is_none_or(|existing| asset_record_metadata_changed(existing, record));
    let bytes_changed = existing.is_none_or(|existing| existing.bytes.as_ref() != record.bytes.as_ref());
    if !metadata_changed && !bytes_changed {
      continue;
    }
    summary.any_changed = true;
    if metadata_changed {
      summary.metadata_changed = true;
      let id = record.id.0.to_string();
      if !summary.changed_asset_ids.contains(&id) {
        summary.changed_asset_ids.push(id);
      }
    }
    projection.assets.assets.insert(record.id, record.clone());
  }
  summary
}

fn asset_record_metadata_changed(existing: &AssetRecord, next: &AssetRecord) -> bool {
  existing.mime_type != next.mime_type || existing.original_name != next.original_name || existing.content_hash != next.content_hash
}

fn install_undo_selection_callbacks(undo: &mut UndoManager, state: &Arc<Mutex<UndoSelectionState>>) {
  let push_state = Arc::clone(state);
  undo.set_on_push(Some(Box::new(move |_, _, _| {
    let mut meta = UndoItemMeta::new();
    if let Ok(state) = push_state.lock()
      && let Some(selection) = &state.pending_selection
    {
      meta.set_value(LoroValue::Binary(selection.clone().into()));
    }
    meta
  })));

  let pop_state = Arc::clone(state);
  undo.set_on_pop(Some(Box::new(move |_, _, meta| {
    let LoroValue::Binary(bytes) = meta.value else {
      return;
    };
    match postcard::from_bytes::<UndoSelectionSnapshot>(bytes.as_ref()) {
      Ok(selection) => {
        if let Ok(mut state) = pop_state.lock() {
          state.restored_selection = Some(selection);
        }
      },
      Err(error) => {
        tracing::warn!(error = %error, "decoding Loro undo selection metadata failed");
      },
    }
  })));
}

fn map_i64_opt(map: &LoroMap, key: &str) -> Option<i64> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  })
}

fn parse_blake3_hex(value: &str) -> Option<[u8; 32]> {
  if value.len() != 64 {
    return None;
  }
  let mut bytes = [0u8; 32];
  for (index, byte) in bytes.iter_mut().enumerate() {
    *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
  }
  Some(bytes)
}

fn selection_direction(anchor: DocumentOffset, head: DocumentOffset) -> SelectionDirection {
  match anchor.cmp(&head) {
    std::cmp::Ordering::Less => SelectionDirection::Forward,
    std::cmp::Ordering::Greater => SelectionDirection::Backward,
    std::cmp::Ordering::Equal => SelectionDirection::None,
  }
}

fn cursor_for_boundary(text: &LoroText, unicode_pos: usize, affinity: SelectionAffinity) -> Option<Cursor> {
  let len = text.len_unicode();
  let pos = unicode_pos.min(len);
  match affinity {
    SelectionAffinity::Before if pos > 0 => text.get_cursor(pos - 1, Side::Right),
    SelectionAffinity::Before => text.get_cursor(0, Side::Left),
    SelectionAffinity::After if pos < len => text.get_cursor(pos, Side::Left),
    SelectionAffinity::After => text.get_cursor(len, Side::Right),
    SelectionAffinity::Neutral => text.get_cursor(pos, Side::Middle),
  }
}

fn resolved_cursor_boundary_unicode(text: &LoroText, resolved: &loro::cursor::PosQueryResult) -> Option<usize> {
  let event_len = text.convert_pos(text.len_utf8(), PosType::Bytes, PosType::Event)?;
  let event_pos = resolved
    .current
    .pos
    .saturating_add(usize::from(resolved.current.side == Side::Right))
    .min(event_len);
  text.convert_pos(event_pos, PosType::Event, PosType::Unicode)
}

fn gpui_gravity_from_presence(gravity: VisualGravity) -> gpui_flowtext::VisualGravity {
  match gravity {
    VisualGravity::Upstream => gpui_flowtext::VisualGravity::Upstream,
    VisualGravity::Downstream => gpui_flowtext::VisualGravity::Downstream,
    VisualGravity::Neutral => gpui_flowtext::VisualGravity::Neutral,
  }
}

fn undo_affinity(affinity: SelectionAffinity) -> UndoSelectionAffinity {
  match affinity {
    SelectionAffinity::Before => UndoSelectionAffinity::Before,
    SelectionAffinity::After => UndoSelectionAffinity::After,
    SelectionAffinity::Neutral => UndoSelectionAffinity::Neutral,
  }
}

/// §16: map the persisted undo-snapshot affinity back onto the editor's
/// `gpui_flowtext::SelectionAffinity` when rebuilding a restored selection.
fn gpui_affinity_from_undo(affinity: UndoSelectionAffinity) -> gpui_flowtext::SelectionAffinity {
  match affinity {
    UndoSelectionAffinity::Before => gpui_flowtext::SelectionAffinity::Before,
    UndoSelectionAffinity::After => gpui_flowtext::SelectionAffinity::After,
    UndoSelectionAffinity::Neutral => gpui_flowtext::SelectionAffinity::Neutral,
  }
}

pub(super) fn paragraph_style_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> Option<ParagraphStyle> {
  let value = attrs?.get(MARK_PARAGRAPH_STYLE)?;
  match value {
    LoroValue::I64(0) => Some(ParagraphStyle::Normal),
    LoroValue::I64(slot) if *slot > 0 => u8::try_from(*slot - 1).ok().map(ParagraphStyle::Custom),
    _ => None,
  }
}

fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

fn mark_run_styles(text: &loro::LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> loro::LoroResult<()> {
  for key in [MARK_RUN_SEMANTIC_STYLE, MARK_HIGHLIGHT_STYLE, MARK_DIRECT_UNDERLINE, MARK_STRIKETHROUGH] {
    text.unmark(range.clone(), key)?;
  }
  if let RunSemanticStyle::Custom(slot) = styles.semantic {
    text.mark(range.clone(), MARK_RUN_SEMANTIC_STYLE, i64::from(slot))?;
  }
  if let Some(flowstate_document::HighlightStyle::Custom(slot)) = styles.highlight {
    text.mark(range.clone(), MARK_HIGHLIGHT_STYLE, i64::from(slot))?;
  }
  if styles.direct_underline {
    text.mark(range.clone(), MARK_DIRECT_UNDERLINE, true)?;
  }
  if styles.strikethrough {
    text.mark(range, MARK_STRIKETHROUGH, true)?;
  }
  Ok(())
}

fn insert_image_block(
  doc: &LoroDoc,
  unicode_index: usize,
  asset_id: u128,
  alt_text: &str,
  caption: Option<&str>,
  sizing: InputImageSizing,
  alignment: InputBlockAlignment,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block = ensure_block(doc, "image", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  block.insert("asset_id", asset_id.to_string())?;
  copy_asset_metadata_to_image_block(doc, &block, asset_id)?;

  let alt_flow_id = nested_flow_id("image_alt");
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  replace_text_incrementally(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, alt_text)?;

  if let Some(caption) = caption {
    let caption_flow_id = nested_flow_id("image_caption");
    block.insert("caption_flow_id", caption_flow_id.as_str())?;
    let caption_flow = ensure_flow(doc, &caption_flow_id, "caption")?;
    let caption_text = caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
    replace_text(&caption_text, SENTINEL_NEWLINE)?;
    caption_text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
    if !caption.is_empty() {
      caption_text.insert(1, caption)?;
    }
  }

  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(alignment))?;
  match sizing {
    InputImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
    InputImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
    InputImageSizing::Fixed { width_px, height_px } => {
      attrs.insert("sizing", "fixed")?;
      attrs.insert("width_px", i64::from(width_px))?;
      if let Some(height_px) = height_px {
        attrs.insert("height_px", i64::from(height_px))?;
      }
    },
  };
  Ok(())
}

fn insert_image_block_with_id(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  image: &flowstate_document::InputImageBlock,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block_key = object_block_key("image", block_id);
  let block = ensure_block_with_id(doc, &block_key, "image", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  replace_image_block_from_input(doc, &block, image)
}

fn insert_equation_block(doc: &LoroDoc, unicode_index: usize, source: &str, display: InputEquationDisplay) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block = ensure_block(doc, "equation", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  let source_flow_id = nested_flow_id("equation_source");
  block.insert("source_flow_id", source_flow_id.as_str())?;
  let source_flow = ensure_flow(doc, &source_flow_id, "equation_source")?;
  replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, source)?;
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("syntax", "latex")?;
  attrs.insert("display", equation_display_name(display))?;
  Ok(())
}

fn insert_equation_block_with_id(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  equation: &flowstate_document::InputEquationBlock,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block_key = object_block_key("equation", block_id);
  let block = ensure_block_with_id(doc, &block_key, "equation", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  replace_equation_block_from_input(doc, &block, equation)
}

fn insert_table_block(
  doc: &LoroDoc,
  unicode_index: usize,
  rows: usize,
  columns: usize,
  column_widths: &[InputTableColumnWidth],
  header_row: bool,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block = ensure_block(doc, "table", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  let table = block.ensure_mergeable_map("table")?;
  table.insert("header_row", header_row)?;
  let row_order = table.ensure_mergeable_movable_list("row_order")?;
  let column_order = table.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table.ensure_mergeable_map("cells_by_id")?;
  table.insert("container_id", table.id().to_string())?;
  table.insert("row_order_container_id", row_order.id().to_string())?;
  table.insert("column_order_container_id", column_order.id().to_string())?;
  table.insert("rows_container_id", rows_by_id.id().to_string())?;
  table.insert("columns_container_id", columns_by_id.id().to_string())?;
  table.insert("cells_container_id", cells_by_id.id().to_string())?;

  // §P2b: a dimensions-only `InsertTable` is a genuinely-new table, so mint fresh
  // durable row/column ids (mirroring the import path) and address every
  // container by its `*_loro_id` string. The deterministic `cell_loro_id_for`
  // keeps each coordinate's cell id a pure function of `(row, column)`.
  let mut minted_columns: Vec<(ColumnId, String)> = Vec::with_capacity(columns);
  for column_ix in 0..columns {
    let column_id = ColumnId(Uuid::new_v4().as_u128());
    let column_id_str = column_loro_id(column_id);
    column_order.push(column_id_str.as_str())?;
    let column = columns_by_id.ensure_mergeable_map(&column_id_str)?;
    column.insert("id", column_id_str.as_str())?;
    column.insert("container_id", column.id().to_string())?;
    let attrs = column.ensure_mergeable_map("attrs")?;
    column.insert("attrs_container_id", attrs.id().to_string())?;
    write_table_column_width(&column, column_widths.get(column_ix).unwrap_or(&InputTableColumnWidth::Auto))?;
    minted_columns.push((column_id, column_id_str));
  }

  for _ in 0..rows {
    let row_id = RowId(Uuid::new_v4().as_u128());
    let row_id_str = row_loro_id(row_id);
    row_order.push(row_id_str.as_str())?;
    let row = rows_by_id.ensure_mergeable_map(&row_id_str)?;
    row.insert("id", row_id_str.as_str())?;
    row.insert("container_id", row.id().to_string())?;
    let attrs = row.ensure_mergeable_map("attrs")?;
    row.insert("attrs_container_id", attrs.id().to_string())?;
    for (column_id, column_id_str) in &minted_columns {
      let cell_id_str = cell_loro_id_for(row_id, *column_id);
      let cell = cells_by_id.ensure_mergeable_map(&cell_id_str)?;
      cell.insert("id", cell_id_str.as_str())?;
      cell.insert("container_id", cell.id().to_string())?;
      cell.insert("row_id", row_id_str.as_str())?;
      cell.insert("column_id", column_id_str.as_str())?;
      cell.insert("row_span", 1_i64)?;
      cell.insert("column_span", 1_i64)?;
      let attrs = cell.ensure_mergeable_map("attrs")?;
      cell.insert("attrs_container_id", attrs.id().to_string())?;
      let nested_table_ids = cell.ensure_mergeable_movable_list("nested_table_ids")?;
      let nested_tables_by_id = cell.ensure_mergeable_map("nested_tables_by_id")?;
      cell.insert("nested_table_order_container_id", nested_table_ids.id().to_string())?;
      cell.insert("nested_tables_container_id", nested_tables_by_id.id().to_string())?;
      let flow_id = format!("{cell_id_str}.flow");
      cell.insert("flow_id", flow_id.as_str())?;
      let flow = ensure_flow(doc, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
      cell.insert("flow_container_id", flow.id().to_string())?;
      cell.insert("text_container_id", text.id().to_string())?;
      replace_text(&text, SENTINEL_NEWLINE)?;
      text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
    }
  }
  Ok(())
}

fn insert_table_block_with_id(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  table: &InputTableBlock,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block_key = object_block_key("table", block_id);
  let block = ensure_block_with_id(doc, &block_key, "table", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  replace_table_block_from_input(doc, &block, table)
}

fn ensure_flow(doc: &LoroDoc, flow_id: &str, kind: &str) -> loro::LoroResult<LoroMap> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let flow = flows.ensure_mergeable_map(flow_id)?;
  flow.insert(FLOW_ID_KEY, flow_id)?;
  flow.insert(FLOW_KIND_KEY, kind)?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("container_id", flow.id().to_string())?;
  flow.insert("text_container_id", text.id().to_string())?;
  flow.insert("attrs_container_id", attrs.id().to_string())?;
  Ok(flow)
}

fn ensure_block(doc: &LoroDoc, kind: &str, flow_id: &str, text: &loro::LoroText, pos: usize) -> loro::LoroResult<LoroMap> {
  let id = format!("{kind}.{}", Uuid::new_v4().as_u128());
  ensure_block_with_id(doc, &id, kind, flow_id, text, pos)
}

fn ensure_block_with_id(doc: &LoroDoc, id: &str, kind: &str, flow_id: &str, text: &loro::LoroText, pos: usize) -> loro::LoroResult<LoroMap> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let block = blocks.ensure_mergeable_map(id)?;
  block.insert("id", id)?;
  block.insert("container_id", block.id().to_string())?;
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = text.get_cursor(pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let attrs = block.ensure_mergeable_map("attrs")?;
  let nested_refs = block.ensure_mergeable_map("nested_refs")?;
  block.insert("attrs_container_id", attrs.id().to_string())?;
  block.insert("nested_refs_container_id", nested_refs.id().to_string())?;
  Ok(block)
}

fn object_block_key(kind: &str, block_id: flowstate_document::BlockId) -> String {
  format!("{kind}.{}", block_id.0)
}

fn replace_text(text: &loro::LoroText, value: &str) -> loro::LoroResult<()> {
  let len = text.len_unicode();
  if len > 0 {
    text.delete(0, len)?;
  }
  if !value.is_empty() {
    text.insert(0, value)?;
  }
  Ok(())
}

fn nested_flow_id(kind: &str) -> String {
  format!("{kind}.{}", Uuid::new_v4().as_u128())
}

fn alignment_name(alignment: InputBlockAlignment) -> &'static str {
  match alignment {
    InputBlockAlignment::Left => "left",
    InputBlockAlignment::Center => "center",
    InputBlockAlignment::Right => "right",
  }
}

fn equation_display_name(display: InputEquationDisplay) -> &'static str {
  match display {
    InputEquationDisplay::Display => "display",
    InputEquationDisplay::InlineLikeParagraph => "inline_like_paragraph",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_document::{DocumentPackage, InputRun, ProjectionPatch, ProjectionTextDelta, loro_schema::body_text};

  fn live_paragraph_metadata_boundaries(doc: &LoroDoc) -> Vec<usize> {
    let body = body_text(doc);
    let snapshot = body.to_string();
    let live_boundaries = live_boundary_positions(&snapshot);
    let root = doc.get_map(ROOT);
    let paragraphs = root
      .ensure_mergeable_map(PARAGRAPHS_BY_ID)
      .expect("paragraph registry");
    let pos_by_id = boundary_cursor_positions(doc, &body, &paragraphs, &["boundary_cursor"]);
    let mut boundaries = map_keys(&paragraphs)
      .into_iter()
      .filter_map(|key| child_map(&paragraphs, &key))
      .filter_map(|paragraph| live_cursor_pos(doc, &live_boundaries, &pos_by_id, &paragraph, "boundary_cursor"))
      .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries
  }

  fn live_paragraph_block_boundaries(doc: &LoroDoc) -> Vec<usize> {
    let body = body_text(doc);
    let snapshot = body.to_string();
    let live_boundaries = live_boundary_positions(&snapshot);
    let root = doc.get_map(ROOT);
    let blocks = root
      .ensure_mergeable_map(BLOCKS_BY_ID)
      .expect("block registry");
    let pos_by_id = boundary_cursor_positions(doc, &body, &blocks, &["anchor_cursor"]);
    let mut boundaries = map_keys(&blocks)
      .into_iter()
      .filter_map(|key| child_map(&blocks, &key))
      .filter(|block| map_string_opt(block, "kind").as_deref() == Some("paragraph"))
      .filter_map(|block| live_cursor_pos(doc, &live_boundaries, &pos_by_id, &block, "anchor_cursor"))
      .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries
  }

  fn input_paragraph(text: &str) -> flowstate_document::InputParagraph {
    flowstate_document::InputParagraph {
      style: flowstate_document::ParagraphStyle::Normal,
      runs: vec![flowstate_document::InputRun {
        text: text.to_string(),
        styles: flowstate_document::RunStyles::default(),
      }],
    }
  }

  /// §P2b test fixture id rule: mint deterministic local ids (`ColumnId(ix + 1)`,
  /// `RowId(ix + 1)`) so the built table is stable and internally consistent, and
  /// derive each cell's [`CellId`] from its coordinate.
  fn fixture_row_id(row_ix: usize) -> flowstate_document::RowId {
    flowstate_document::RowId(u128::try_from(row_ix).expect("row index fits u128") + 1)
  }

  fn fixture_column_id(column_ix: usize) -> flowstate_document::ColumnId {
    flowstate_document::ColumnId(u128::try_from(column_ix).expect("column index fits u128") + 1)
  }

  fn input_table(rows: Vec<Vec<&str>>, column_widths: Vec<flowstate_document::InputTableColumnWidth>, header_row: bool) -> InputTableBlock {
    InputTableBlock {
      rows: rows
        .into_iter()
        .enumerate()
        .map(|(row_ix, row)| {
          let row_id = fixture_row_id(row_ix);
          flowstate_document::InputTableRow {
            id: row_id,
            cells: row
              .into_iter()
              .enumerate()
              .map(|(column_ix, text)| input_table_cell(row_id, fixture_column_id(column_ix), text))
              .collect(),
          }
        })
        .collect(),
      columns: column_widths
        .into_iter()
        .enumerate()
        .map(|(column_ix, width)| flowstate_document::InputTableColumn {
          id: fixture_column_id(column_ix),
          width,
        })
        .collect(),
      style: flowstate_document::InputTableStyle { header_row },
    }
  }

  fn input_table_cell(
    row_id: flowstate_document::RowId,
    column_id: flowstate_document::ColumnId,
    text: &str,
  ) -> flowstate_document::InputTableCell {
    flowstate_document::InputTableCell {
      id: flowstate_document::CellId::from_coordinate(row_id, column_id),
      row_id,
      column_id,
      blocks: vec![InputTableCellBlock::Paragraph(input_paragraph(text))],
      row_span: 1,
      col_span: 1,
    }
  }

  fn projected_table_cell_text(table: &flowstate_document::TableBlock, row_ix: usize, cell_ix: usize) -> &str {
    let flowstate_document::TableCellBlock::Paragraph(paragraph) = &table.rows[row_ix].cells[cell_ix].blocks[0] else {
      panic!("expected paragraph table cell");
    };
    &paragraph.text
  }

  fn local_update_bytes(events: &[RuntimeEvent]) -> Vec<u8> {
    events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
        RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::ProjectionPatched { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("local update bytes")
  }

  #[test]
  fn local_insert_exports_update_and_invalidates_projection() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let events = runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    assert!(matches!(events.first(), Some(RuntimeEvent::LocalUpdate { bytes, .. }) if !bytes.is_empty()));
    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::ProjectionPatched { .. }))
    );
    assert_eq!(flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0), "hello");
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello");
    Ok(())
  }

  #[test]
  fn semantic_insert_text_projects_inserted_run_styles() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let styles = RunStyles {
      semantic: RunSemanticStyle::Custom(2),
      ..RunStyles::default()
    };
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "styled".to_string(),
      styles,
    })?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "styled");
    assert_eq!(projection.paragraphs[0].runs.len(), 1);
    assert_eq!(projection.paragraphs[0].runs[0].styles, styles);
    Ok(())
  }

  #[test]
  fn editor_insert_text_preserves_paragraph_style_mark() -> Result<()> {
    let source = flowstate_document::document_from_input(
      flowstate_document::flowstate_document_theme(),
      vec![InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![InputRun {
          text: "pocket".to_string(),
          styles: RunStyles::default(),
        }],
      }],
    );
    let doc = flowstate_document::document_to_loro(&source, "Styled")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::InsertText {
        at: flowstate_document::DocumentOffset { paragraph: 0, byte: 3 },
        text: "x".to_string(),
        styles: RunStyles::default(),
      },
    )?;

    let updated = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&updated, 0), "pocxket");
    assert_eq!(updated.paragraphs[0].style, ParagraphStyle::Custom(0));
    Ok(())
  }

  #[test]
  fn split_paragraph_creates_live_paragraph_metadata_and_block_anchor() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 3,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;

    assert_eq!(body_text(runtime.doc()).to_string(), "\nhe\nllo");
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 3]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0, 3]);
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.paragraphs.len(), 2);
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "he");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "llo");
    Ok(())
  }

  #[test]
  fn runtime_repairs_missing_paragraph_style_marks_on_takeover() -> Result<()> {
    let doc = new_loro_document("Malformed")?;
    let body = body_text(&doc);
    body.insert(1, "bad\nnext")?;
    doc.commit();
    assert_eq!(body_paragraph_boundaries_missing_style_mark(&body), vec![4]);

    let runtime = CrdtRuntime::from_doc(doc, None, None)?;

    assert!(body_paragraph_boundaries_missing_style_mark(&body_text(runtime.doc())).is_empty());
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.paragraphs.len(), 2);
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "bad");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "next");
    assert_eq!(projection.paragraphs[1].style, ParagraphStyle::Normal);
    Ok(())
  }

  #[test]
  fn package_open_persists_missing_paragraph_style_mark_repair() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("malformed.db8");
    let doc = new_loro_document("Malformed")?;
    let body = body_text(&doc);
    body.insert(1, "bad\nnext")?;
    doc.commit();
    assert_eq!(body_paragraph_boundaries_missing_style_mark(&body), vec![4]);
    DocumentPackage::from_loro_snapshot(&doc, "Malformed")?.write(&path)?;

    let _runtime = CrdtRuntime::open_package(&path)?;
    let package = DocumentPackage::read(&path)?;
    let loaded = package.load_loro_doc()?;

    assert_eq!(body_text(&loaded).to_string(), "\nbad\nnext");
    assert!(body_paragraph_boundaries_missing_style_mark(&body_text(&loaded)).is_empty());
    // §P2a: opening also runs the projection-repair pipeline, which writes the
    // durable paragraph metadata this malformed doc was missing at boundary 4.
    // That repair persists an additional update segment beyond the style-mark
    // repair, so the count is no longer exactly one.
    assert!(!package.loro_update_segments.is_empty());
    assert_eq!(live_paragraph_metadata_boundaries(&loaded), vec![0, 4]);
    Ok(())
  }

  #[test]
  fn remote_import_repairs_and_publishes_missing_paragraph_style_marks() -> Result<()> {
    let base = new_loro_document("Malformed")?;
    let source = base.fork();
    let from_vv = base.state_vv();
    body_text(&source).insert(1, "bad\nnext")?;
    source.commit();
    let update = source.export(ExportMode::updates(&from_vv))?;

    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let events = target.import_remote_update(&update)?;

    assert!(body_paragraph_boundaries_missing_style_mark(&body_text(target.doc())).is_empty());
    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    Ok(())
  }

  #[test]
  fn join_paragraphs_deletes_boundary_and_prunes_stale_metadata() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 6,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 7,
      text: "world".to_string(),
      styles: RunStyles::default(),
    })?;
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 6]);

    let before_join = runtime.projection_snapshot()?;
    let first = before_join.ids.paragraph_ids[0];
    let second = before_join.ids.paragraph_ids[1];
    let events = runtime.apply_editor_semantic_command(&before_join, &EditorSemanticCommand::JoinParagraphs { first, second })?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhelloworld");
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0]);
    Ok(())
  }

  #[test]
  fn insert_into_empty_paragraph_before_text_preserves_following_paragraph_identity() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "before".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 7,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 8,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 9,
      text: "after".to_string(),
      styles: RunStyles::default(),
    })?;

    let before_insert = runtime.projection_snapshot()?;
    assert_eq!(before_insert.paragraphs.len(), 3);
    assert_eq!(flowstate_document::paragraph_text(&before_insert, 0), "before");
    assert_eq!(flowstate_document::paragraph_text(&before_insert, 1), "");
    assert_eq!(flowstate_document::paragraph_text(&before_insert, 2), "after");
    let following_id = before_insert.ids.paragraph_ids[2];
    let rewrote_following_boundary_cursor = {
      let body = body_text(runtime.doc());
      let snapshot = body.to_string();
      let root = runtime.doc().get_map(ROOT);
      let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
      let live_boundaries = live_boundary_positions(&snapshot);
      let pos_by_id = boundary_cursor_positions(runtime.doc(), &body, &paragraphs, &["boundary_cursor"]);
      let mut rewrote = false;
      for key in map_keys(&paragraphs) {
        let Some(paragraph) = child_map(&paragraphs, &key) else {
          continue;
        };
        if live_cursor_pos(runtime.doc(), &live_boundaries, &pos_by_id, &paragraph, "boundary_cursor") == Some(8)
          && let Some(cursor) = body.get_cursor(8, Side::Right)
        {
          paragraph.insert("boundary_cursor", cursor.encode())?;
          rewrote = true;
        }
      }
      runtime.doc().commit();
      rewrote
    };
    assert!(rewrote_following_boundary_cursor);

    let events = runtime.apply_editor_semantic_command(
      &before_insert,
      &EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: 1, byte: 0 },
        text: "X".to_string(),
        styles: RunStyles::default(),
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    assert_eq!(body_text(runtime.doc()).to_string(), "\nbefore\nX\nafter");
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 7, 9]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0, 7, 9]);
    let after_insert = runtime.projection_snapshot()?;
    assert_eq!(after_insert.ids.paragraph_ids[2], following_id);
    assert_eq!(flowstate_document::paragraph_text(&after_insert, 1), "X");
    assert_eq!(flowstate_document::paragraph_text(&after_insert, 2), "after");
    Ok(())
  }

  #[test]
  fn runtime_persists_local_update_segments() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.db8");
    let doc = flowstate_document::new_loro_document("Runtime")?;
    DocumentPackage::from_loro_snapshot(&doc, "Runtime")?.write(&path)?;
    let mut runtime = CrdtRuntime::open_package(&path)?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "persisted".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 10,
      text: " twice".to_string(),
      styles: RunStyles::default(),
    })?;
    let package = DocumentPackage::read(&path)?;
    assert!(package.loro_update_segments.len() >= 3);
    assert!(package.current_search_units().is_empty());
    let loaded = package.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\npersisted twice");
    Ok(())
  }

  #[test]
  fn semantic_text_commands_mutate_loro_body_flow() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello world".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::DeleteRange {
      unicode_index: 6,
      unicode_len: 1,
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 6,
      inherited_style: flowstate_document::ParagraphStyle::Custom(2),
    })?;
    runtime.command(SemanticCommand::SetRunStyles {
      unicode_range: 1..6,
      styles: flowstate_document::RunStyles {
        semantic: flowstate_document::RunSemanticStyle::Custom(3),
        direct_underline: true,
        strikethrough: false,
        highlight: Some(flowstate_document::HighlightStyle::Custom(4)),
      },
    })?;

    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello\nworld");
    let delta = body_text(runtime.doc()).to_delta();
    assert!(delta.iter().any(|item| matches!(
      item,
      loro::TextDelta::Insert {
        attributes: Some(attributes),
        ..
      } if attributes.get(flowstate_document::MARK_RUN_SEMANTIC_STYLE).is_some()
    )));
    assert!(delta.iter().any(|item| matches!(
      item,
      loro::TextDelta::Insert {
        insert,
        attributes: Some(attributes),
      } if insert == "\n" && attributes.get(flowstate_document::MARK_PARAGRAPH_STYLE).is_some()
    )));
    Ok(())
  }

  #[test]
  fn editor_replace_paragraph_span_preserves_boundaries_and_marks() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
        style: flowstate_document::ParagraphStyle::Normal,
        runs: vec![flowstate_document::InputRun {
          text: "old".to_string(),
          styles: flowstate_document::RunStyles::default(),
        }],
      })],
    );
    let replacement_styles = flowstate_document::RunStyles {
      semantic: flowstate_document::RunSemanticStyle::Custom(3),
      direct_underline: true,
      strikethrough: false,
      highlight: Some(flowstate_document::HighlightStyle::Custom(4)),
    };
    let replacement = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
          style: flowstate_document::ParagraphStyle::Custom(2),
          runs: vec![flowstate_document::InputRun {
            text: "Hello".to_string(),
            styles: replacement_styles,
          }],
        }),
        flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
          style: flowstate_document::ParagraphStyle::Normal,
          runs: vec![flowstate_document::InputRun {
            text: "World".to_string(),
            styles: flowstate_document::RunStyles::default(),
          }],
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Span")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    let events = runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceParagraphSpan {
        start: None,
        before: flowstate_document::capture_document_span(&source, 0..1),
        after: flowstate_document::capture_document_span(&replacement, 0..2),
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    assert_eq!(body_text(runtime.doc()).to_string(), "\nHello\nWorld");
    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "Hello");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "World");
    assert_eq!(projection.paragraphs[0].style, flowstate_document::ParagraphStyle::Custom(2));
    assert_eq!(projection.paragraphs[0].runs[0].styles, replacement_styles);
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 6]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0, 6]);
    Ok(())
  }

  #[test]
  fn editor_replace_paragraph_span_forces_editor_supplied_ids() -> Result<()> {
    // Regression for the optimistic-vs-canonical identity divergence: a span
    // replacement (here a 3->2 paragraph join) must adopt the editor-supplied
    // durable ids canonically instead of letting Loro metadata survival keep a
    // different id, which would strand later pending edits that reference the id
    // the optimistic replay dropped. Boundary 0 keeps its reserved root id, so
    // this asserts the trailing (non-reserved) boundary the join keeps.
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("a")),
        flowstate_document::InputBlock::Paragraph(input_paragraph("b")),
        flowstate_document::InputBlock::Paragraph(input_paragraph("c")),
      ],
    );
    let replacement = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("ab")),
        flowstate_document::InputBlock::Paragraph(input_paragraph("c")),
      ],
    );
    let before = flowstate_document::capture_document_span(&source, 0..3);
    let after = flowstate_document::capture_document_span(&replacement, 0..2);
    // The editor-captured ids for the surviving trailing paragraph; canonical
    // survival would otherwise keep `source`'s third-paragraph id here.
    let expected_paragraph_id = after.paragraph_ids[1];
    let expected_block_id = after.block_ids[1];

    let doc = flowstate_document::document_to_loro(&source, "Span Ids")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceParagraphSpan { start: None, before, after },
    )?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "ab");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "c");
    assert_eq!(projection.ids.paragraph_ids[1], expected_paragraph_id);
    assert_eq!(projection.ids.block_ids[1], expected_block_id);
    Ok(())
  }

  #[test]
  fn editor_replace_paragraph_span_join_at_start_drops_removed_sibling_block() -> Result<()> {
    // Regression for the block-id divergence: a boundary-0 join that removes an
    // empty middle paragraph must not let the removed paragraph's block metadata
    // survive and re-anchor onto the following paragraph's boundary, displacing
    // that paragraph's real block id. `before`/`after` are captured from the
    // runtime's own (canonical) projection so their ids match the Loro records,
    // exactly as in the live editor flow.
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("y")),
        flowstate_document::InputBlock::Paragraph(input_paragraph("")),
        flowstate_document::InputBlock::Paragraph(input_paragraph("z")),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Join Start")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    // The trailing "z" paragraph is OUTSIDE the replaced span; its ids must be
    // exactly what survives at the shifted index after the join.
    let surviving_paragraph_id = projection.ids.paragraph_ids[2];
    let surviving_block_id = projection.ids.block_ids[2];

    // Join "y"+"" -> "y": the span covers paragraphs 0..2, the result keeps "y".
    let before = flowstate_document::capture_document_span(&projection, 0..2);
    let after = flowstate_document::capture_document_span(&projection, 0..1);
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceParagraphSpan {
        start: Some(flowstate_document::DocumentOffset { paragraph: 0, byte: 0 }),
        before,
        after,
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "y");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "z");
    assert_eq!(projection.ids.paragraph_ids[1], surviving_paragraph_id);
    assert_eq!(projection.ids.block_ids[1], surviving_block_id);
    Ok(())
  }

  // FS-170: a caret can rest on either side of a block object; the two sides
  // must resolve to DISTINCT document offsets (the "before" side to the previous
  // paragraph's end, the "after" side to the following paragraph's start).
  // Before the decode fix both collapsed onto the previous paragraph's end,
  // sending remote/undo carets to the wrong side of the object.
  #[test]
  fn object_between_paragraphs_resolves_distinct_caret_sides() -> Result<()> {
    flowstate_fidelity::set_enabled(true);
    let _ = flowstate_fidelity::take_violations();
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("before")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "img".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
        flowstate_document::InputBlock::Paragraph(input_paragraph("after")),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Object sides")?;
    let runtime = CrdtRuntime::from_doc(doc, None, None)?;

    let index = &runtime.projection_index;
    assert_eq!(index.object_placeholder_positions.len(), 1, "exactly one block-object placeholder");
    let position = index.object_placeholder_positions[0];
    let before = index
      .offset_for_body_unicode(&runtime.projection, position)
      .expect("object slot resolves");
    let after = index
      .offset_for_body_unicode(&runtime.projection, position + 1)
      .expect("post-object slot resolves");
    assert_ne!(before, after, "the two sides of a block object must resolve to distinct offsets (FS-170)");
    // "after object" belongs to the following paragraph's start; "before" to the
    // preceding paragraph's end. Paragraph indices skip the object block, so the
    // "after" paragraph is index 1.
    assert_eq!(after.paragraph, 1, "the after-object caret is the following paragraph");
    assert_eq!(after.byte, 0, "at the following paragraph's start");
    assert_eq!(before.paragraph, 0, "the before-object caret is the preceding paragraph");

    let violations = flowstate_fidelity::take_violations();
    flowstate_fidelity::set_enabled(false);
    assert!(
      !violations.iter().any(|violation| violation.contains("object-side-collapse")),
      "no object-side-collapse violation must fire: {violations:?}"
    );
    Ok(())
  }

  #[test]
  fn editor_replace_block_updates_image_metadata() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    let events = runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(source.ids.block_ids[1]),
        block_ix: 1,
        after: flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(9),
          alt_text: "new alt".to_string(),
          caption: Some(input_paragraph("caption")),
          sizing: flowstate_document::InputImageSizing::Fixed {
            width_px: 640,
            height_px: Some(480),
          },
          alignment: flowstate_document::InputBlockAlignment::Right,
        }),
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after ReplaceBlock");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(9));
    assert_eq!(image.alt_text.as_ref(), "new alt");
    assert!(image.caption.is_some());
    assert_eq!(
      image.sizing,
      flowstate_document::ImageSizing::Fixed {
        width_px: 640,
        height_px: Some(480),
      }
    );
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Right);
    Ok(())
  }

  #[test]
  fn editor_replace_block_prefers_projected_loro_id_over_stale_index() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block = projection.ids.block_ids[1];

    let events = runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(image_block),
        block_ix: 99,
        after: flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(9),
          alt_text: "new alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::FitWidth,
          alignment: flowstate_document::InputBlockAlignment::Right,
        }),
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after ReplaceBlock");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(9));
    assert_eq!(image.alt_text.as_ref(), "new alt");
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Right);
    Ok(())
  }

  #[test]
  fn editor_replace_image_alt_text_updates_alt_flow() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Image Alt")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block_id = projection.ids.block_ids[1];
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, image_block_id).expect("image block");
    let before_flow = map_string_opt(&block, "alt_text_flow_id").expect("alt flow id");

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceImageAltText {
        image: image_block_id,
        text: "new alt".to_string(),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after alt text edit");
    };
    assert_eq!(image.alt_text.as_ref(), "new alt");
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, image_block_id).expect("image block");
    assert_eq!(map_string_opt(&block, "alt_text_flow_id").as_deref(), Some(before_flow.as_str()));
    Ok(())
  }

  #[test]
  fn editor_set_image_layout_updates_image_attrs() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Image Layout")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block_id = projection.ids.block_ids[1];

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::SetImageLayout {
        image: image_block_id,
        sizing: flowstate_document::InputImageSizing::Fixed {
          width_px: 444,
          height_px: None,
        },
        alignment: flowstate_document::InputBlockAlignment::Center,
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after layout edit");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(1));
    assert_eq!(image.alt_text.as_ref(), "alt");
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Center);
    assert_eq!(
      image.sizing,
      flowstate_document::ImageSizing::Fixed {
        width_px: 444,
        height_px: None,
      }
    );
    Ok(())
  }

  #[test]
  fn editor_insert_block_creates_loro_object_from_projection_payload() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputBlock::Paragraph(input_paragraph("body"))],
    );
    let target = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(7),
          alt_text: "inserted".to_string(),
          caption: Some(input_paragraph("caption")),
          sizing: flowstate_document::InputImageSizing::FitWidth,
          alignment: flowstate_document::InputBlockAlignment::Center,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Insert Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let image_block = target.ids.block_ids[1];

    let events = runtime.apply_editor_semantic_command(
      &target,
      &EditorSemanticCommand::InsertBlock {
        block: image_block,
        block_ix: 1,
        after: flowstate_document::input_block_from_block(&target.blocks[1]),
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    assert!(
      body_text(runtime.doc())
        .to_string()
        .contains(OBJECT_REPLACEMENT)
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected inserted image block");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(7));
    assert_eq!(image.alt_text.as_ref(), "inserted");
    assert!(image.caption.is_some());
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Center);
    assert_eq!(projection.ids.block_ids[1], image_block);
    Ok(())
  }

  #[test]
  fn editor_delete_block_removes_loro_object_by_projected_id() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Delete Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block = projection.ids.block_ids[1];

    let events = runtime.apply_editor_semantic_command(&projection, &EditorSemanticCommand::DeleteBlock { block: image_block })?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    assert!(
      !body_text(runtime.doc())
        .to_string()
        .contains(OBJECT_REPLACEMENT)
    );
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.blocks.len(), 1);
    assert!(matches!(&projection.blocks[0], flowstate_document::Block::Paragraph(_)));
    Ok(())
  }

  #[test]
  fn editor_delete_object_and_replace_paragraph_span_apply_together() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("alpha")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
        flowstate_document::InputBlock::Paragraph(input_paragraph("omega")),
      ],
    );
    let target = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputBlock::Paragraph(input_paragraph("alega"))],
    );
    let doc = flowstate_document::document_to_loro(&source, "Mixed Delete")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block_id = projection.ids.block_ids[1];
    let before = flowstate_document::capture_document_span(&source, 0..2);
    let after = flowstate_document::capture_document_span(&target, 0..1);

    runtime.apply_editor_semantic_command(&projection, &EditorSemanticCommand::DeleteBlock { block: image_block_id })?;
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceParagraphSpan {
        start: Some(flowstate_document::DocumentOffset { paragraph: 0, byte: 0 }),
        before,
        after,
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.blocks.len(), 1);
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "alega");
    assert!(
      !body_text(runtime.doc())
        .to_string()
        .contains(OBJECT_REPLACEMENT)
    );
    Ok(())
  }

  #[test]
  fn editor_move_block_reorders_loro_object_placeholder_by_projected_id() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "first".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(2),
          alt_text: "second".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Move Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let second_image = projection.ids.block_ids[2];

    let events = runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::MoveBlock {
        block: second_image,
        new_block_ix: 1,
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected moved image at block 1");
    };
    assert_eq!(image.alt_text.as_ref(), "second");
    assert_eq!(projection.ids.block_ids[1], second_image);
    let flowstate_document::Block::Image(image) = &projection.blocks[2] else {
      panic!("expected first image at block 2");
    };
    assert_eq!(image.alt_text.as_ref(), "first");
    Ok(())
  }

  #[test]
  fn editor_replace_block_updates_equation_source() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Equation(flowstate_document::InputEquationBlock {
          source: "x".to_string(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::Display,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Equation")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(source.ids.block_ids[1]),
        block_ix: 1,
        after: flowstate_document::InputBlock::Equation(flowstate_document::InputEquationBlock {
          source: "x+1".to_string(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::InlineLikeParagraph,
        }),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Equation(equation) = &projection.blocks[1] else {
      panic!("expected equation block after ReplaceBlock");
    };
    assert_eq!(equation.source.as_ref(), "x+1");
    assert_eq!(equation.display, flowstate_document::EquationDisplay::InlineLikeParagraph);
    Ok(())
  }

  #[test]
  fn editor_replace_equation_source_range_edits_source_flow() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Equation(flowstate_document::InputEquationBlock {
          source: "x+y".to_string(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::Display,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Edit Equation Source")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let equation_block_id = projection.ids.block_ids[1];
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, equation_block_id).expect("equation block");
    let before_flow = map_string_opt(&block, "source_flow_id").expect("source flow id");

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceEquationSourceRange {
        equation: equation_block_id,
        range: 1..2,
        text: "*".to_string(),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Equation(equation) = &projection.blocks[1] else {
      panic!("expected equation block after source range edit");
    };
    assert_eq!(equation.source.as_ref(), "x*y");
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, equation_block_id).expect("equation block");
    assert_eq!(map_string_opt(&block, "source_flow_id").as_deref(), Some(before_flow.as_str()));
    Ok(())
  }

  #[test]
  fn editor_replace_block_rebuilds_table_structure() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["old"]],
          vec![flowstate_document::InputTableColumnWidth::Auto],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Table")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(source.ids.block_ids[1]),
        block_ix: 1,
        after: flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::FixedPx(90),
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          true,
        )),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table) = &projection.blocks[1] else {
      panic!("expected table block after ReplaceBlock");
    };
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.rows[0].cells.len(), 2);
    assert!(table.style.header_row);
    let column_widths = table
      .columns
      .iter()
      .map(|column| column.width.clone())
      .collect::<Vec<_>>();
    assert!(matches!(
      column_widths.as_slice(),
      [
        flowstate_document::TableColumnWidth::FixedPx(90),
        flowstate_document::TableColumnWidth::Fraction(1)
      ]
    ));
    let flowstate_document::TableCellBlock::Paragraph(cell) = &table.rows[1].cells[0].blocks[0] else {
      panic!("expected paragraph cell after ReplaceBlock");
    };
    assert_eq!(cell.text, "c");
    Ok(())
  }

  #[test]
  fn editor_set_table_column_width_preserves_table_identity() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::Auto,
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Resize Table")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let table_block_id = projection.ids.block_ids[1];
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    let before_rows = movable_list_strings(&child_movable_list(&table, "row_order").expect("row order"));
    let before_columns = movable_list_strings(&child_movable_list(&table, "column_order").expect("column order"));

    let events = runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::SetTableColumnWidth {
        table: table_block_id,
        column_ix: 1,
        width: flowstate_document::InputTableColumnWidth::FixedPx(222),
      },
    )?;

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")),
      before_rows
    );
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "column_order").expect("column order")),
      before_columns
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table) = &projection.blocks[1] else {
      panic!("expected table block after column width command");
    };
    let column_widths = table
      .columns
      .iter()
      .map(|column| column.width.clone())
      .collect::<Vec<_>>();
    assert!(matches!(
      column_widths.as_slice(),
      [
        flowstate_document::TableColumnWidth::Auto,
        flowstate_document::TableColumnWidth::FixedPx(222)
      ]
    ));
    Ok(())
  }

  #[test]
  fn editor_table_structure_commands_mutate_loro_table_incrementally() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::Auto,
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Structure Table")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let mut projection = runtime.projection_snapshot()?;
    let table_block_id = projection.ids.block_ids[1];
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    let initial_rows = movable_list_strings(&child_movable_list(&table, "row_order").expect("row order"));
    let initial_columns = movable_list_strings(&child_movable_list(&table, "column_order").expect("column order"));

    // §P2b: the source table carried deterministic fixture ids, so the imported
    // canonical order lists are `row.1/row.2` and `column.1/column.2`.
    let new_row_id = flowstate_document::RowId(100);
    let inserted_row = flowstate_document::InputTableRow {
      id: new_row_id,
      cells: vec![
        input_table_cell(new_row_id, fixture_column_id(0), "new-a"),
        input_table_cell(new_row_id, fixture_column_id(1), "new-b"),
      ],
    };
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::InsertTableRow {
        table: table_block_id,
        new_row_id,
        after_row: Some(fixture_row_id(0)),
        row: inserted_row,
      },
    )?;
    projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after row insert");
    };
    assert_eq!(table_projection.rows.len(), 3);
    assert_eq!(projected_table_cell_text(table_projection, 1, 0), "new-a");
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "column_order").expect("column order")),
      initial_columns
    );
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")).len(),
      initial_rows.len() + 1
    );

    let new_column_id = flowstate_document::ColumnId(100);
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::InsertTableColumn {
        table: table_block_id,
        new_column_id,
        after_column: Some(fixture_column_id(0)),
        width: flowstate_document::InputTableColumnWidth::FixedPx(88),
        cells: vec![
          input_table_cell(fixture_row_id(0), new_column_id, "x"),
          input_table_cell(new_row_id, new_column_id, "y"),
          input_table_cell(fixture_row_id(1), new_column_id, "z"),
        ],
      },
    )?;
    projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after column insert");
    };
    let column_widths = table_projection
      .columns
      .iter()
      .map(|column| column.width.clone())
      .collect::<Vec<_>>();
    assert_eq!(column_widths.len(), 3);
    assert!(matches!(
      column_widths.as_slice(),
      [
        flowstate_document::TableColumnWidth::Auto,
        flowstate_document::TableColumnWidth::FixedPx(88),
        flowstate_document::TableColumnWidth::Fraction(1)
      ]
    ));
    assert_eq!(projected_table_cell_text(table_projection, 0, 1), "x");
    assert_eq!(projected_table_cell_text(table_projection, 1, 1), "y");
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")).len(),
      initial_rows.len() + 1
    );

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::DeleteTableRow {
        table: table_block_id,
        row_id: new_row_id,
      },
    )?;
    projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after row delete");
    };
    assert_eq!(table_projection.rows.len(), 2);
    assert_eq!(projected_table_cell_text(table_projection, 1, 0), "c");

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::DeleteTableColumn {
        table: table_block_id,
        column_id: new_column_id,
      },
    )?;
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after column delete");
    };
    assert_eq!(table_projection.columns.len(), 2);
    assert_eq!(projected_table_cell_text(table_projection, 0, 1), "b");
    assert_eq!(projected_table_cell_text(table_projection, 1, 1), "d");
    Ok(())
  }

  #[test]
  fn editor_replace_table_cell_preserves_table_structure() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::Auto,
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Cell")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let table_block_id = projection.ids.block_ids[1];
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    let before_rows = movable_list_strings(&child_movable_list(&table, "row_order").expect("row order"));
    let before_columns = movable_list_strings(&child_movable_list(&table, "column_order").expect("column order"));

    // Row index 1 / column index 0 of the fixture map to `RowId(2)` / `ColumnId(1)`.
    let target_row = fixture_row_id(1);
    let target_column = fixture_column_id(0);
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceTableCell {
        table: table_block_id,
        row_id: target_row,
        column_id: target_column,
        cell: input_table_cell(target_row, target_column, "changed"),
      },
    )?;

    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")),
      before_rows
    );
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "column_order").expect("column order")),
      before_columns
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after cell replace");
    };
    assert_eq!(projected_table_cell_text(table_projection, 1, 0), "changed");
    assert_eq!(projected_table_cell_text(table_projection, 1, 1), "d");
    Ok(())
  }

  #[test]
  fn undo_manager_restores_selection_metadata() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let selection = UndoSelectionSnapshot {
      anchor_cursor: vec![1, 2, 3],
      head_cursor: vec![4, 5, 6],
      anchor_affinity: UndoSelectionAffinity::Before,
      head_affinity: UndoSelectionAffinity::After,
      direction: UndoSelectionDirection::Forward,
    };

    runtime.set_pending_undo_selection(Some(selection.clone()))?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "abc".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::Undo)?;

    assert_eq!(runtime.take_restored_undo_selection(), Some(selection.clone()));
    runtime.command(SemanticCommand::Redo)?;
    assert_eq!(runtime.take_restored_undo_selection(), Some(selection));
    Ok(())
  }

  #[test]
  fn semantic_object_commands_project_structured_blocks() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertImage {
      unicode_index: 1,
      asset_id: 7,
      alt_text: "alt".to_string(),
      caption: Some("caption".to_string()),
      sizing: flowstate_document::InputImageSizing::Fixed {
        width_px: 320,
        height_px: Some(180),
      },
      alignment: flowstate_document::InputBlockAlignment::Center,
    })?;
    runtime.command(SemanticCommand::InsertEquation {
      unicode_index: 2,
      source: "x^2".to_string(),
      display: flowstate_document::InputEquationDisplay::InlineLikeParagraph,
    })?;
    runtime.command(SemanticCommand::InsertTable {
      unicode_index: 3,
      rows: 2,
      columns: 2,
      column_widths: vec![
        flowstate_document::InputTableColumnWidth::FixedPx(120),
        flowstate_document::InputTableColumnWidth::Fraction(1),
      ],
      header_row: true,
    })?;

    let projection = runtime.projection_snapshot()?;
    assert!(matches!(
      &projection.blocks[0],
      flowstate_document::Block::Image(image)
        if image.asset_id == flowstate_document::AssetId(7)
          && image.alt_text.as_ref() == "alt"
          && image.caption.is_some()
    ));
    assert!(matches!(
      &projection.blocks[1],
      flowstate_document::Block::Equation(equation)
        if equation.source.as_ref() == "x^2"
          && equation.display == flowstate_document::EquationDisplay::InlineLikeParagraph
    ));
    assert!(matches!(
      &projection.blocks[2],
      flowstate_document::Block::Table(table)
        if table.rows.len() == 2
          && table.rows[0].cells.len() == 2
          && table.style.header_row
          && matches!(table.columns.as_slice(), [
            flowstate_document::TableColumn { width: flowstate_document::TableColumnWidth::FixedPx(120), .. },
            flowstate_document::TableColumn { width: flowstate_document::TableColumnWidth::Fraction(1), .. }
          ])
    ));
    Ok(())
  }

  #[test]
  fn runtime_opens_and_forks_named_revisions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("revisions.db8");
    let doc = flowstate_document::new_loro_document("Runtime")?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Runtime")?;
    let blank_revision = package.create_named_revision(&doc, "Blank", "Blank document", None, None)?;
    body_text(&doc).insert(1, "latest")?;
    doc.commit();
    package.compact_to_named_snapshot(&doc, "Latest", "Latest document", None, None)?;
    package.write(&path)?;

    let mut runtime = CrdtRuntime::open_package(&path)?;
    let opened = runtime.command(SemanticCommand::OpenRevision { revision_id: blank_revision })?;
    assert!(matches!(
      opened.as_slice(),
      [RuntimeEvent::RevisionOpened { document, .. }] if document.paragraphs.first().is_some_and(|paragraph| paragraph.byte_range.is_empty())
    ));

    let forked = runtime.command(SemanticCommand::ForkRevision { revision_id: blank_revision })?;
    let [RuntimeEvent::RevisionForked { document, package, .. }] = forked.as_slice() else {
      panic!("expected fork event");
    };
    assert_eq!(
      document
        .paragraphs
        .first()
        .map(|paragraph| paragraph.byte_range.clone()),
      Some(0..0)
    );
    assert!(!package.loro_snapshots.is_empty());
    Ok(())
  }

  #[test]
  fn remote_text_insert_emits_incremental_paragraph_patch() -> Result<()> {
    let base = flowstate_document::new_loro_document("Shared")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let setup = source.export_updates_for(&target.doc().oplog_vv())?;
    target.import_remote_update(&setup)?;
    let update = local_update_bytes(&source.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?);

    let events = target.import_remote_update(&update)?;
    let patches = events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::ProjectionPatched { batch, .. } => Some(&batch.patches),
        RuntimeEvent::LocalUpdate { .. }
        | RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("remote import should emit a projection patch");

    let [
      ProjectionPatch::ParagraphText {
        row_hint, new, delta_utf8, ..
      },
    ] = patches.as_slice()
    else {
      panic!("expected one paragraph text patch");
    };
    assert_eq!(*row_hint, 0);
    assert_eq!(
      new
        .runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>(),
      "hello"
    );
    assert_eq!(delta_utf8, &[ProjectionTextDelta::Insert("hello".len())]);
    assert!(events.iter().all(|event| !matches!(
      event,
      RuntimeEvent::ProjectionUpdated {
        invalidation: ProjectionInvalidation {
          fallback_reason: Some("remote_update_projection_fallback"),
          ..
        },
        ..
      }
    )));
    Ok(())
  }

  #[test]
  fn remote_text_insert_in_object_document_still_emits_incremental_patch() -> Result<()> {
    let source_projection = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![
        InputBlock::Paragraph(input_paragraph("alpha")),
        InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(7),
          alt_text: "figure".to_string(),
          caption: None,
          sizing: InputImageSizing::Intrinsic,
          alignment: InputBlockAlignment::Center,
        }),
        InputBlock::Paragraph(input_paragraph("omega")),
      ],
    );
    let base = flowstate_document::document_to_loro(&source_projection, "Mixed")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let setup = source.export_updates_for(&target.doc().oplog_vv())?;
    target.import_remote_update(&setup)?;
    let unicode_index = body_text(source.doc()).len_unicode();
    let update = local_update_bytes(&source.command(SemanticCommand::InsertText {
      unicode_index,
      text: "!".to_string(),
      styles: RunStyles::default(),
    })?);

    let events = target.import_remote_update(&update)?;
    let patches = events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::ProjectionPatched { batch, .. } => Some(&batch.patches),
        RuntimeEvent::LocalUpdate { .. }
        | RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("remote import should emit a projection patch");

    let [
      ProjectionPatch::ParagraphText {
        row_hint, new, delta_utf8, ..
      },
    ] = patches.as_slice()
    else {
      panic!("expected one paragraph text patch");
    };
    assert_eq!(*row_hint, 2);
    assert_eq!(
      new
        .runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>(),
      "omega!"
    );
    assert_eq!(
      delta_utf8,
      &[ProjectionTextDelta::Retain("omega".len()), ProjectionTextDelta::Insert("!".len())]
    );
    assert!(
      events
        .iter()
        .all(|event| !matches!(event, RuntimeEvent::ProjectionUpdated { .. }))
    );
    Ok(())
  }

  #[test]
  fn local_text_insert_can_apply_without_projection_snapshot() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let events = runtime
      .try_apply_editor_semantic_command_without_projection(&EditorSemanticCommand::InsertText {
        at: flowstate_document::DocumentOffset { paragraph: 0, byte: 0 },
        text: "hello".to_string(),
        styles: RunStyles::default(),
      })?
      .expect("text insert should use body fast path");

    assert!(
      events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty()))
    );
    assert!(
      events
        .iter()
        .all(|event| !matches!(event, RuntimeEvent::ProjectionUpdated { .. }))
    );
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello");
    Ok(())
  }

  #[test]
  fn imported_runtime_startup_projection_accepts_the_first_editor_command() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![InputBlock::Paragraph(input_paragraph("ready"))],
    );
    let imported = flowstate_document::import_document_projection(source, "Imported startup")?;
    let mut runtime = CrdtRuntime::from_imported_document(imported)?;
    let startup = runtime.projection_snapshot()?;

    runtime.apply_editor_commands(
      1,
      &startup.frontier,
      &[EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: 0, byte: 0 },
        text: "x".to_string(),
        styles: RunStyles::default(),
      }],
      None,
    )?;

    assert_eq!(flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0), "xready");
    Ok(())
  }

  #[test]
  fn editor_commands_accept_projection_frontier_after_metadata_only_commit() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Metadata frontier")?;
    let base_frontier = runtime.projection_snapshot()?.frontier;
    runtime.set_author_identity(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef, Some("Author".to_string()))?;
    assert_ne!(runtime.doc.state_frontiers().encode(), base_frontier);
    assert_eq!(runtime.projection_snapshot()?.frontier, base_frontier);

    runtime.apply_editor_commands(
      1,
      &base_frontier,
      &[EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: 0, byte: 0 },
        text: "x".to_string(),
        styles: RunStyles::default(),
      }],
      None,
    )?;

    assert_eq!(flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0), "x");
    Ok(())
  }

  #[test]
  fn editor_commands_reject_a_stale_projection_frontier() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Stale frontier")?;
    let base_frontier = runtime.projection_snapshot()?.frontier;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "remote".to_string(),
      styles: RunStyles::default(),
    })?;

    let error = runtime
      .apply_editor_commands(
        1,
        &base_frontier,
        &[EditorSemanticCommand::InsertText {
          at: DocumentOffset { paragraph: 0, byte: 0 },
          text: "local".to_string(),
          styles: RunStyles::default(),
        }],
        None,
      )
      .expect_err("stale editor commands must be rejected");

    assert!(error.downcast_ref::<StaleProjectionError>().is_some());
    assert_eq!(body_text(runtime.doc()).to_string(), "\nremote");
    Ok(())
  }

  #[test]
  fn editor_insert_after_object_uses_canonical_body_index() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![
        InputBlock::Paragraph(input_paragraph("before")),
        InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(9),
          alt_text: "figure".to_string(),
          caption: None,
          sizing: InputImageSizing::Intrinsic,
          alignment: InputBlockAlignment::Center,
        }),
        InputBlock::Paragraph(input_paragraph("after")),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Mixed editor offsets")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    runtime.apply_editor_commands(
      1,
      &[],
      &[EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: 1, byte: 2 },
        text: "!".to_string(),
        styles: RunStyles::default(),
      }],
      None,
    )?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "before");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "af!ter");
    Ok(())
  }

  #[test]
  fn remote_import_reports_pending_dependencies() -> Result<()> {
    let source = flowstate_document::new_loro_document("Source")?;
    let empty_vv = VersionVector::default();
    body_text(&source).insert(1, "first")?;
    source.commit();
    let mid_vv = source.state_vv();
    body_text(&source).insert(6, " second")?;
    source.commit();
    let second_only = source.export(ExportMode::updates(&mid_vv))?;

    let mut target = CrdtRuntime::new_empty("Target")?;
    let events = target.import_remote_update(&second_only)?;
    assert!(matches!(events.first(), Some(RuntimeEvent::RemoteUpdateApplied { pending: Some(_), .. })));

    let first_update = source.export(ExportMode::updates(&empty_vv))?;
    let events = target.import_remote_update(&first_update)?;
    assert!(matches!(events.first(), Some(RuntimeEvent::RemoteUpdateApplied { pending: None, .. })));
    Ok(())
  }
  #[test]
  fn projected_document_runtime_starts_without_eager_package_or_reprojection() -> Result<()> {
    let mut source = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![InputBlock::Paragraph(input_paragraph("imported"))],
    );
    source.theme.zoom_factor = 1.75;
    let runtime = CrdtRuntime::from_document_projection(&source, "Imported")?;

    assert!(runtime.package.is_none());
    assert_eq!(runtime.projection.frontier, runtime.doc.state_frontiers().encode());
    assert_eq!(runtime.projection.theme.zoom_factor, 1.75);
    assert_eq!(flowstate_document::paragraph_text(&runtime.projection, 0), "imported");
    Ok(())
  }

  #[test]
  fn projection_index_materializes_paragraph_and_block_anchor_lookups() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 3,
      inherited_style: ParagraphStyle::Normal,
    })?;
    let projection = runtime.projection_snapshot()?;
    let index = ProjectionRuntimeIndex::from_projection(&projection);

    // §24 paragraph metadata index agrees with the linear `paragraph_ids` scan.
    for (expected_ix, id) in projection.ids.paragraph_ids.iter().enumerate() {
      assert_eq!(index.paragraph_metadata_by_id.get(id).copied(), Some(expected_ix));
      assert_eq!(
        index.paragraph_index_for_id(&projection, *id),
        projection
          .ids
          .paragraph_ids
          .iter()
          .position(|candidate| candidate == id),
      );
    }
    // §24 block anchor index agrees with the linear `block_ids` scan.
    for (expected_ix, id) in projection.ids.block_ids.iter().enumerate() {
      assert_eq!(index.block_anchor_by_id.get(id).copied(), Some(expected_ix));
      assert_eq!(
        index.block_index_for_id(&projection, *id),
        projection
          .ids
          .block_ids
          .iter()
          .position(|candidate| candidate == id),
      );
    }
    // Absent ids fall back to `None` through both the index and the scan.
    assert_eq!(index.paragraph_index_for_id(&projection, flowstate_document::new_paragraph_id()), None);
    assert_eq!(index.block_index_for_id(&projection, flowstate_document::new_block_id()), None);

    // One search-unit span per block; one style-interval list per paragraph.
    assert_eq!(index.search_unit_spans.len(), projection.blocks.len());
    assert_eq!(index.style_runs_by_paragraph.len(), projection.paragraphs.len());
    Ok(())
  }

  #[test]
  fn projection_index_maps_changed_ranges_to_search_units() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 3,
      inherited_style: ParagraphStyle::Normal,
    })?;
    let projection = runtime.projection_snapshot()?;
    let index = ProjectionRuntimeIndex::from_projection(&projection);

    // Body text is "\nhe\nllo": unit 0 covers [1,3), unit 1 covers [4,7).
    let first = index.search_units_for_changed_ranges(&[ProjectionTextRange {
      flow_id: ROOT_BODY_FLOW_ID.to_string(),
      unicode_start: 1,
      unicode_len: 1,
    }]);
    assert_eq!(first, vec![0]);
    let second = index.search_units_for_changed_ranges(&[ProjectionTextRange {
      flow_id: ROOT_BODY_FLOW_ID.to_string(),
      unicode_start: 4,
      unicode_len: 1,
    }]);
    assert_eq!(second, vec![1]);
    // A non-body flow never matches a body search-unit span.
    assert!(
      index
        .search_units_for_changed_ranges(&[ProjectionTextRange {
          flow_id: "flow.other".to_string(),
          unicode_start: 1,
          unicode_len: 5,
        }])
        .is_empty()
    );
    Ok(())
  }

  #[test]
  fn projection_index_style_intervals_resolve_run_styles() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let styles = RunStyles {
      semantic: RunSemanticStyle::Custom(2),
      ..RunStyles::default()
    };
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "styled".to_string(),
      styles,
    })?;
    let projection = runtime.projection_snapshot()?;
    let index = ProjectionRuntimeIndex::from_projection(&projection);

    // The style interval covering the paragraph's leading byte carries its run styles.
    assert_eq!(index.run_styles_at(0, 0), Some(styles));
    assert_eq!(index.run_styles_at(0, 0), projection.paragraphs[0].runs.first().map(|run| run.styles),);
    // Bytes beyond the paragraph text have no covering interval.
    assert_eq!(index.run_styles_at(0, 1_000), None);
    Ok(())
  }

  #[test]
  fn projection_index_indexes_image_asset_refs_and_table_cells() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertImage {
      unicode_index: 1,
      asset_id: 7,
      alt_text: "alt".to_string(),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Center,
    })?;
    runtime.command(SemanticCommand::InsertTable {
      unicode_index: 2,
      rows: 2,
      columns: 2,
      column_widths: vec![InputTableColumnWidth::FixedPx(120), InputTableColumnWidth::Fraction(1)],
      header_row: true,
    })?;
    let projection = runtime.projection_snapshot()?;
    let index = ProjectionRuntimeIndex::from_projection(&projection);

    // §24 asset reference index: the image block id is recorded under its asset.
    let image_block_ix = projection
      .blocks
      .iter()
      .position(|block| matches!(block, Block::Image(_)))
      .expect("image block present");
    let image_block_id = projection.ids.block_ids[image_block_ix];
    let asset_refs = index
      .asset_refs_by_id
      .get(&AssetId(7))
      .expect("asset reference indexed");
    assert_eq!(asset_refs.len(), 1);
    assert!(asset_refs.contains(&image_block_id));

    // §24 table row/column/cell index: 2x2 table → 2 rows, 2 columns, 4 cells.
    let table_block_ix = projection
      .blocks
      .iter()
      .position(|block| matches!(block, Block::Table(_)))
      .expect("table block present");
    let table_block_id = projection.ids.block_ids[table_block_ix];
    let entry = index
      .table_cells_by_block
      .get(&table_block_id)
      .expect("table indexed");
    assert_eq!(entry.row_ids.len(), 2);
    assert_eq!(entry.column_ids.len(), 2);
    assert_eq!(entry.cells.len(), 4);
    // §P2b: the index is keyed by the model's durable ids, so every projected
    // cell's `(row_id, column_id)` coordinate resolves to its deterministic
    // `CellId` (the fresh table minted uuid row/column ids, so probe by them).
    let Block::Table(table) = &projection.blocks[table_block_ix] else {
      panic!("expected table block");
    };
    for row in &table.rows {
      for cell in &row.cells {
        assert_eq!(entry.cells.get(&(cell.row_id, cell.column_id)), Some(&cell.id));
      }
    }

    // §24 search unit index: every object block contributes a paragraph-less span.
    let object_blocks = projection
      .blocks
      .iter()
      .filter(|block| !matches!(block, Block::Paragraph(_)))
      .count();
    assert_eq!(
      index
        .search_unit_spans
        .iter()
        .filter(|span| span.paragraph.is_none())
        .count(),
      object_blocks,
    );
    Ok(())
  }

  #[test]
  fn projection_index_cursor_cache_clears_on_incremental_update() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    // Seed a stand-in cache entry, then drive a further incremental edit. Positions
    // can shift on any edit, so the cache must be cleared (rebuilt empty) afterward.
    runtime
      .projection_index
      .cursor_resolution_cache
      .insert(vec![0xde, 0xad], DocumentOffset::default());
    assert!(!runtime.projection_index.cursor_resolution_cache.is_empty());
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "x".to_string(),
      styles: RunStyles::default(),
    })?;
    assert!(runtime.projection_index.cursor_resolution_cache.is_empty());
    Ok(())
  }

  #[test]
  fn presence_after_affinity_sticks_to_following_text_across_inserts() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Presence")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "ab".to_string(),
      styles: RunStyles::default(),
    })?;
    let offset = DocumentOffset { paragraph: 0, byte: 1 };
    let selection = EditorSelection::collapsed_with(offset, gpui_flowtext::SelectionAffinity::After, gpui_flowtext::VisualGravity::Downstream);
    let presence = runtime
      .presence_selection(&selection)
      .expect("presence selection should encode");

    runtime.command(SemanticCommand::InsertText {
      unicode_index: 2,
      text: "X".to_string(),
      styles: RunStyles::default(),
    })?;
    let carets = runtime.resolve_presence_carets(vec![RuntimePresenceCaretRequest {
      selection: presence,
      color_rgb: 0xabcdef,
    }]);

    assert_eq!(carets.carets.len(), 1);
    assert_eq!(carets.carets[0].offset, DocumentOffset { paragraph: 0, byte: 2 });
    assert_eq!(carets.carets[0].visual_gravity, gpui_flowtext::VisualGravity::Downstream);
    Ok(())
  }

  #[test]
  fn presence_before_affinity_sticks_to_preceding_text_across_inserts() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Presence")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "ab".to_string(),
      styles: RunStyles::default(),
    })?;
    let offset = DocumentOffset { paragraph: 0, byte: 1 };
    let selection = EditorSelection::collapsed_with(offset, gpui_flowtext::SelectionAffinity::Before, gpui_flowtext::VisualGravity::Upstream);
    let presence = runtime
      .presence_selection(&selection)
      .expect("presence selection should encode");

    runtime.command(SemanticCommand::InsertText {
      unicode_index: 2,
      text: "X".to_string(),
      styles: RunStyles::default(),
    })?;
    let carets = runtime.resolve_presence_carets(vec![RuntimePresenceCaretRequest {
      selection: presence,
      color_rgb: 0xabcdef,
    }]);

    assert_eq!(carets.carets.len(), 1);
    assert_eq!(carets.carets[0].offset, offset);
    assert_eq!(carets.carets[0].visual_gravity, gpui_flowtext::VisualGravity::Upstream);
    Ok(())
  }
}

#[cfg(test)]
#[path = "crdt_runtime/editor_transaction_tests.rs"]
mod editor_transaction_tests;
#[cfg(test)]
#[path = "crdt_runtime/multi_peer_convergence_tests.rs"]
mod multi_peer_convergence_tests;
#[cfg(test)]
#[path = "crdt_runtime/projection_repair_tests.rs"]
mod projection_repair_tests;
#[cfg(test)]
#[path = "crdt_runtime/table_convergence_tests.rs"]
mod table_convergence_tests;
