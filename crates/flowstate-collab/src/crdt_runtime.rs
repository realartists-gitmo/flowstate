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
  InputTableColumnWidth, MAIN_BODY_BLOCK_ID, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE,
  MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, Paragraph, ParagraphId, ParagraphStyle, ProjectionDefect, ProjectionPatch,
  ProjectionStructuralBlock, ROOT, ROOT_BODY_FLOW_ID, ROOT_FIRST_PARAGRAPH_ID, RowId, RunSemanticStyle, RunStyles, SENTINEL_NEWLINE, SectionId,
  TableBlock, cell_loro_id, cell_loro_id_for, column_loro_id, document_from_loro, document_from_loro_with_defects, import_document_projection,
  loro_import::assets_from_document, loro_schema::body_text, new_loro_document, row_loro_id,
};
use loro::{
  Container, ContainerID, ExportMode, Frontiers, ID, ImportStatus, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, Subscription,
  UndoItemMeta, ValueOrContainer, VersionRange, VersionVector,
  cursor::{Cursor, Side},
  event::{Diff, DiffEvent},
};
// §perf act-three B.1: the vendored `loro-internal` UndoManager (re-exported by
// the wrapper as `InnerUndoManager`) — needed for the external fast-path undo
// APIs (`peek_top_span` / `external_step_*`), which the thin wrapper type does
// not surface. The wrapper's `UndoManager` is a passthrough over this exact
// type, so behavior is otherwise identical.
use flowstate_fidelity::{self as fidelity, FidelityClass};
use loro::InnerUndoManager as UndoManager;
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

/// §P2a: commit origin stamped on canonical projection-repair batches. Excluded
/// from the local undo stack and used by peers/telemetry to identify repairs.
const REPAIR_ORIGIN: &str = "repair";

/// §P2a: maximum canonical-repair attempts per defect `stable_key`. A defect that
/// survives this many repair passes is quarantined (left as the deterministic
/// projection) instead of looping forever.
const PROJECTION_REPAIR_ATTEMPT_CAP: u64 = 3;

#[path = "crdt_runtime/import_delta.rs"]
pub(crate) mod import_delta;
#[path = "crdt_runtime/projection_patch.rs"]
mod projection_patch;
#[path = "crdt_runtime/projection_repair.rs"]
mod projection_repair;
#[path = "crdt_runtime/table_ops.rs"]
pub(crate) mod table_ops;
#[path = "crdt_runtime/types.rs"]
mod types;
use crate::presence::{PresenceSelection, SelectionAffinity, SelectionDirection, SelectionEndpoint, VisualGravity};
use gpui_flowtext::{DocumentOffset, EditorSelection, ExternalCaret, ExternalSelection, ProjectionPatchBatch, apply_projection_patch_batch};
use loro::{ContainerTrait as _, cursor::PosType};
pub(crate) use projection_patch::{
  body_input_paragraph_at, paragraph_projection_patches_ranged, projection_patches_between, remote_nonstructural_projection_patches,
  splice_region_patches, text_delta_between,
};

/// A successful §6-R regional derivation: the splice patch set, the region's
/// defects, and the OLD projection's paragraph-index range the region covered
/// (so the caller can route out-of-region changes through the ranged readback).
struct RegionalPatches {
  patches: Vec<ProjectionPatch>,
  defects: Vec<ProjectionDefect>,
  region_paragraphs: std::ops::Range<usize>,
}

/// How a repair pass surfaces the repaired projection to the editor stream —
/// see [`CrdtRuntime::schedule_projection_repairs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairEmission {
  /// Push the repaired projection onto the ordered editor stream (repairs
  /// running after a patched emission left the editor pre-repair).
  Streamed,
  /// Do not stream: an enclosing full-rebuild emission (or the absence of any
  /// attached editor, at construction) carries the repaired state instead.
  Silent,
}
use types::UndoSelectionState;
pub use types::{
  ProjectionFallbackStats, ProjectionInvalidation, ProjectionTextRange, RuntimeAssetMetadata, RuntimeCommentMessage, RuntimeCommentThread,
  RuntimeDiffSpan, RuntimeEvent, RuntimeFrontierDiff, RuntimePresenceCaretRequest, RuntimePresenceCarets, RuntimeRevisionInfo,
  SemanticCommand, UndoSelectionAffinity,
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
  // Loro-first spec §16 (lazy author identity): identity supplied by the app,
  // registered into `users_by_id`/`replicas_by_id` only when this replica
  // actually commits an edit — a replica that never edits leaves no history.
  pending_author_identity: Option<(u128, Option<String>)>,
  /// §16 always-on commit metrics (ops from Loro's own `ChangeMeta.len`).
  ops_committed_total: Arc<AtomicU64>,
  commits_total: Arc<AtomicU64>,
  _pre_commit_subscription: Subscription,
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
  // §act-twelve A12.1.1: refresh the replica record's `last_seen_at` on the
  // FIRST local edit instead of at open (open commits invalidate the
  // package's frontier-stamped caches).
  pending_replica_touch: bool,
  /// Field fix 2026-07-07: the ORDERED editor-bound projection stream. Every
  /// projection change (local intent patches, remote import patches, full
  /// replaces) is pushed here IN COMMIT ORDER under the write gate; the editor
  /// drains it as its sole projection input. See
  /// `gpui_flowtext::local_intents::ProjectionStreamItem`.
  editor_stream: Vec<gpui_flowtext::ProjectionStreamItem>,
  /// Loro-first publish queue (spec §6): committed local-change events
  /// (`LocalUpdate` bytes and friends) buffered under the write gate for the
  /// I/O service to drain and broadcast. Local intents never block on the
  /// network; the network never reaches into an intent.
  pending_publish: Vec<RuntimeEvent>,
  /// §6-R: the paragraph/block registry container ids, resolved once so the
  /// subscription drain can recognize registry-map events by target and
  /// harvest imported record keys for regional rematerialization.
  paragraph_registry_container_id: String,
  block_registry_container_id: String,
  /// §act-three B.1: single-slot recorded inverse for the last qualifying mass
  /// `DeleteRange` commit. Consumed (ping-ponged) by the undo/redo fast path;
  /// any interleaving commit or import fails its frontier check and the slot
  /// is dropped. See `local_write::recorded_inverse`.
  /// §act-five P3-deep: the recorded-inverse fast path is now a STACK, not a
  /// single slot — so a RUN of consecutive local undos (the Ctrl-Z mash) each
  /// replay `O(change)` instead of only the first, with the rest falling to the
  /// `O(doc/history)` Loro `UndoManager`. `recorded_undo` holds inverses that can
  /// be fast-undone (LIFO); `recorded_redo` holds their opposites after an undo.
  /// A forward edit pushes to `recorded_undo` and clears `recorded_redo`. Both
  /// clear on any remote import / frontier break. FAIL-SAFE: every fast step is
  /// still validated against the Loro `UndoManager`'s actual top span
  /// (`peek_top_span`), so a stale entry bails to the correct slow path.
  recorded_undo: Vec<crate::local_write::recorded_inverse::RecordedInverse>,
  recorded_redo: Vec<crate::local_write::recorded_inverse::RecordedInverse>,
  /// §act-eleven C8: count of batched-import calls (one per gate hold).
  import_batches_served: u64,
  _root_subscription: Subscription,
  _local_update_subscription: Subscription,
}

/// §24 style interval index entry: one run's byte span within a paragraph plus
/// its styles. `start`/`len` are byte offsets into the paragraph text.
#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Debug, Default, PartialEq)]
struct TableIndexEntry {
  row_ids: Vec<RowId>,
  column_ids: Vec<ColumnId>,
  cells: FxHashMap<(RowId, ColumnId), CellId>,
}

/// Loro-first spec §6.3: the composed body delta drained from one
/// subscription-buffer sweep, with its conservatism flags.
#[derive(Debug, Default)]
pub(crate) struct DrainedBodyDelta {
  pub(crate) net: import_delta::NetBodyDelta,
  pub(crate) has_remote_origin: bool,
  pub(crate) checkout_seen: bool,
  /// Spec §6-R: record keys this batch wrote into (or deleted from) the
  /// paragraph registry — the identity candidates a regional
  /// rematerialization needs beyond the old projection's own rows.
  pub(crate) imported_paragraph_record_keys: Vec<String>,
  /// Same, for the block registry (paragraph blocks and objects alike).
  pub(crate) imported_block_record_keys: Vec<String>,
}

/// §24 search unit span: a lightweight body-unicode range for one search unit.
/// `paragraph` is `Some(ix)` for a paragraph unit and `None` for an object
/// placeholder unit. Used to map changed body ranges onto affected search units.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SearchUnitSpan {
  paragraph: Option<usize>,
  unicode_start: usize,
  unicode_len: usize,
}

#[derive(Debug, Default)]
pub(crate) struct ProjectionRuntimeIndex {
  // Original three indexes (§ earlier work): behavior and population unchanged.
  paragraph_body_unicode_starts: Vec<usize>,
  /// One-past-the-end unicode position of the live body flow at index-build
  /// time. Used to clamp degraded (id-less) cursor resolutions, which Loro
  /// resolves to the WHOLE-BODY end (spec §8 / semantics-audit F7).
  body_unicode_end: usize,
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
  /// Paragraph id → paragraph index in the projection this index was built
  /// from. O(1); the identity basis of the local-write resolution law.
  pub(crate) fn paragraph_index(&self, id: ParagraphId) -> Option<usize> {
    self.paragraph_metadata_by_id.get(&id).copied()
  }

  /// Block id → block index in the projection this index was built from.
  pub(crate) fn block_index(&self, id: BlockId) -> Option<usize> {
    self.block_anchor_by_id.get(&id).copied()
  }

  /// Clamp a body-unicode position into the live body range recorded at index
  /// build time (degraded-cursor defense, spec §8).
  pub(crate) fn clamp_body_unicode(&self, pos: usize) -> usize {
    pos.min(self.body_unicode_end)
  }

  /// Sorted pre-state paragraph-boundary positions (spec §6.3 exact
  /// structural-delete detection).
  pub(crate) fn boundary_positions(&self) -> &[usize] {
    &self.paragraph_boundary_positions
  }

  /// Sorted pre-state object placeholder positions.
  pub(crate) fn object_positions(&self) -> &[usize] {
    &self.object_placeholder_positions
  }

  /// Pre-state paragraph start positions (body unicode).
  pub(crate) fn paragraph_starts(&self) -> &[usize] {
    &self.paragraph_body_unicode_starts
  }

  #[hotpath::measure]
  pub(crate) fn from_projection(projection: &DocumentProjection) -> Self {
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
          // Count chars straight off the rope slice — materializing a String
          // per paragraph (the old `paragraph_text` call) allocated the whole
          // body once per index rebuild.
          let byte_range = flowstate_document::paragraph_byte_range(projection, paragraph_ix);
          let char_count = projection.text.byte_slice(byte_range).chars().count();
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

    index.body_unicode_end = body_unicode;
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
    fidelity::event(FidelityClass::Caret, "offset->unicode", || {
      format!("offset={offset:?} byte={byte} -> body_unicode={unicode}")
    });
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
  pub(crate) fn body_unicode_for_offset_in_loro(&self, doc: &LoroDoc, projection: &DocumentProjection, offset: DocumentOffset) -> Option<usize> {
    let paragraph = projection.paragraphs.get(offset.paragraph)?;
    let paragraph_text = flowstate_document::paragraph_text(projection, offset.paragraph);
    let byte = offset
      .byte
      .min(flowstate_document::paragraph_text_len(paragraph));
    if !paragraph_text.is_char_boundary(byte) {
      return None;
    }
    // §perf (measured 2026-07-07): the MAINTAINED start first, content-verified
    // against the live body (same trust law as `paragraph_boundary_from_record`),
    // with the durable-cursor resolution as the fallback. The old order ran
    // `paragraph_body_start_in_loro` — a linear cursor chunk scan, 2.6ms on the
    // reference doc — on EVERY keystroke's anchor resolution; the maintained
    // index is in-sync inside the gate and the verification catches the
    // coalesced-row/rot cases where it is not.
    let maintained = self
      .paragraph_body_unicode_starts
      .get(offset.paragraph)
      .copied()
      .filter(|&start| body_start_matches_paragraph(doc, projection, offset.paragraph, start));
    let paragraph_start = maintained.or_else(|| {
      projection
        .ids
        .paragraph_ids
        .get(offset.paragraph)
        .and_then(|paragraph_id| paragraph_body_start_in_loro(doc, *paragraph_id))
        .or_else(|| {
          self
            .paragraph_body_unicode_starts
            .get(offset.paragraph)
            .copied()
        })
    })?;
    let unicode = paragraph_start + paragraph_text[..byte].chars().count();
    fidelity::event(FidelityClass::Caret, "offset->unicode-loro", || {
      format!("offset={offset:?} byte={byte} -> body_unicode={unicode}")
    });
    Some(unicode)
  }

  pub(crate) fn offset_for_body_unicode(&self, projection: &DocumentProjection, unicode: usize) -> Option<DocumentOffset> {
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
      if let Ok(start_ix) = self
        .paragraph_body_unicode_starts
        .binary_search(&following_start)
      {
        let paragraph_ix = start_ix.min(projection.paragraphs.len().saturating_sub(1));
        let offset = DocumentOffset {
          paragraph: paragraph_ix,
          byte: 0,
        };
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
    fidelity::event(FidelityClass::Caret, "unicode->offset", || {
      format!("body_unicode={unicode} -> offset={offset:?}")
    });
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
        fidelity::check(left != right, FidelityClass::Caret, "object-side-collapse", || {
          format!("U+FFFC object at body-unicode {position} collapses caret sides: both map to {left:?}")
        });
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
      // Widen by one paragraph on each edge: a paragraph-STYLE mark rides the
      // boundary `\n`, which text-start attribution assigns to the PRECEDING
      // paragraph, and mark expand semantics can carry one position past the
      // range end — either way the styled paragraph sits one slot outside the
      // strict span. Over-inclusion is harmless (the ranged readback is exact
      // and produces no patch for untouched paragraphs); under-inclusion left
      // a stale paragraph style after an undo of a multi-paragraph restyle
      // (object-fuzz undo arm).
      if let Some(start) = start {
        touched.insert(start.saturating_sub(1));
        touched.insert(start);
      }
      if let Some(end) = end {
        touched.insert(end);
        touched.insert((end + 1).min(paragraph_count.saturating_sub(1)));
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
    // §act-ten A10.8: search-unit spans TILE the body flow (non-overlapping,
    // ascending in both start and end — paragraph spans cover their ranges
    // with the boundary separators between, object spans are single slots), so
    // each range's overlap window is two binary searches instead of the old
    // full scan per commit.
    let mut touched = Vec::new();
    for range in ranges {
      if range.flow_id != ROOT_BODY_FLOW_ID {
        continue;
      }
      let range_end = range.unicode_start.saturating_add(range.unicode_len.max(1));
      let window_start = self
        .search_unit_spans
        .partition_point(|span| span.unicode_start.saturating_add(span.unicode_len.max(1)) <= range.unicode_start);
      let window_end = self
        .search_unit_spans
        .partition_point(|span| span.unicode_start < range_end);
      touched.extend(window_start..window_end);
    }
    touched.sort_unstable();
    touched.dedup();
    touched
  }

  /// Returns `(rebuild_required, table_ids_to_refresh_post_apply)`.
  fn update_for_patches(&mut self, projection: &DocumentProjection, patches: &[ProjectionPatch]) -> (bool, Vec<flowstate_document::BlockId>) {
    // §24: resolved cursor offsets shift whenever the projection's positions
    // change, so invalidate the memoized cursor cache on every incremental
    // update. (Full rebuilds construct a fresh index, which starts empty.)
    self.cursor_resolution_cache.clear();
    let mut text_deltas = Vec::new();
    let mut structural: Option<&ProjectionPatch> = None;
    let mut table_refresh: Vec<flowstate_document::BlockId> = Vec::new();
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
          // A10.3 oracle catch (pre-existing): the incremental path left the
          // edited paragraph's style intervals stale — refresh them from the
          // patch payload.
          let mut intervals = Vec::with_capacity(new.runs.len());
          let mut start = 0usize;
          for run in &new.runs {
            intervals.push(StyleInterval {
              start,
              len: run.text.len(),
              styles: run.styles,
            });
            start = start.saturating_add(run.text.len());
          }
          self.style_runs_by_paragraph.insert(paragraph_ix, intervals);
        },
        ProjectionPatch::InsertBlocks { .. } | ProjectionPatch::DeleteBlocks { .. } => {
          // §act-ten A10.3: ONE structural patch can splice incrementally (the
          // Enter/join/cross-delete shapes patch synthesis emits). A second
          // structural patch, or any unresolvable shape, falls back to rebuild.
          if structural.replace(patch).is_some() {
            rebuild = true;
            break;
          }
        },
        ProjectionPatch::MoveBlock { .. } => {
          rebuild = true;
          break;
        },
        ProjectionPatch::ParagraphRuns {
          block_id,
          paragraph_id,
          row_hint,
          runs,
        } => {
          let Some(paragraph_ix) = paragraph_index_for_patch(projection, *block_id, *paragraph_id, *row_hint) else {
            rebuild = true;
            break;
          };
          let mut intervals = Vec::with_capacity(runs.len());
          let mut start = 0usize;
          for run in runs {
            intervals.push(StyleInterval {
              start,
              len: run.len,
              styles: run.styles,
            });
            start = start.saturating_add(run.len);
          }
          self.style_runs_by_paragraph.insert(paragraph_ix, intervals);
        },
        ProjectionPatch::ReplaceObjectBlock { block_id, .. } => {
          // A10.3 oracle catches (pre-existing): a replaced TABLE left its
          // `table_cells_by_block` entry stale (wrong cell resolution for the
          // next local table op), and a replaced IMAGE left `asset_refs_by_id`
          // pointing at the old asset. The fresh entries need the POST-apply
          // block, so the caller refreshes them after the batch applies.
          table_refresh.push(*block_id);
        },
        ProjectionPatch::ParagraphStyle { .. } | ProjectionPatch::AssetArrived { .. } => {},
      }
    }
    if rebuild {
      return (true, Vec::new());
    }
    // §11 anti-amplification: ONE merge-walk pass per array instead of one
    // full O(doc) shift pass PER TEXT PATCH — a replace-all/mass-restyle batch
    // carries thousands of ParagraphText patches, which made this
    // O(patches × doc). Deltas come from DISJOINT paragraphs (patch synthesis
    // groups per paragraph), so each position's final shift is the prefix sum
    // of the deltas of paragraphs strictly before it — same result as the
    // sequential per-patch loops this replaces.
    let mut text_deltas: Vec<(usize, isize)> = text_deltas
      .into_iter()
      .filter(|(_, delta)| *delta != 0)
      .collect();
    if text_deltas.is_empty() && structural.is_none() {
      return (false, table_refresh);
    }
    text_deltas.sort_unstable_by_key(|(paragraph_ix, _)| *paragraph_ix);
    // A10.3 oracle catch (pre-existing): the merge-walk shifted every position
    // array but left `body_unicode_end` stale after every text edit — the
    // degraded-cursor clamp was drifting from the real body end.
    let total_delta: isize = text_deltas.iter().map(|(_, delta)| *delta).sum();
    self.body_unicode_end = self.body_unicode_end.saturating_add_signed(total_delta);
    // Pre-shift start values key the value-ordered arrays (placeholders,
    // search spans); index-ordered arrays walk by paragraph index.
    let thresholds: Vec<(usize, isize)> = text_deltas
      .iter()
      .map(|&(paragraph_ix, delta)| {
        (
          self
            .paragraph_body_unicode_starts
            .get(paragraph_ix)
            .copied()
            .unwrap_or_default(),
          delta,
        )
      })
      .collect();
    {
      let mut shift = 0isize;
      let mut next = 0usize;
      for (ix, start) in self.paragraph_body_unicode_starts.iter_mut().enumerate() {
        while next < text_deltas.len() && text_deltas[next].0 < ix {
          shift += text_deltas[next].1;
          next += 1;
        }
        if shift != 0 {
          *start = start.saturating_add_signed(shift);
        }
      }
    }
    {
      let mut shift = 0isize;
      let mut next = 0usize;
      for (ix, boundary) in self.paragraph_boundary_positions.iter_mut().enumerate() {
        while next < text_deltas.len() && text_deltas[next].0 < ix {
          shift += text_deltas[next].1;
          next += 1;
        }
        if shift != 0 {
          *boundary = boundary.saturating_add_signed(shift);
        }
      }
    }
    {
      // `object_placeholder_positions` is ascending by construction; shift
      // each placeholder by the deltas whose (pre-shift) threshold lies AT or
      // before it. A10.3 oracle catch (pre-existing): the old strict `<` (and
      // the per-delta `> threshold` filter before it) missed an object sitting
      // at the SAME position as an empty paragraph's start — typing into that
      // empty paragraph left the object's cached position stale.
      let mut shift = 0isize;
      let mut next = 0usize;
      for position in &mut self.object_placeholder_positions {
        while next < thresholds.len() && thresholds[next].0 <= *position {
          shift += thresholds[next].1;
          next += 1;
        }
        if shift != 0 {
          *position = position.saturating_add_signed(shift);
        }
      }
    }
    {
      let mut delta_by_paragraph: FxHashMap<usize, isize> = FxHashMap::default();
      for &(paragraph_ix, delta) in &text_deltas {
        *delta_by_paragraph.entry(paragraph_ix).or_default() += delta;
      }
      // Spans are ascending by `unicode_start` (built in flow order). An
      // OBJECT span at the same position as an edited empty paragraph's start
      // must shift (same equality case as the placeholder walk above); the
      // edited paragraph's OWN span must not (its start is the threshold — only
      // its length changes, applied via `own_delta` below).
      let mut shift = 0isize;
      let mut next = 0usize;
      for span in &mut self.search_unit_spans {
        let bound = if span.paragraph.is_none() {
          span.unicode_start.saturating_add(1)
        } else {
          span.unicode_start
        };
        while next < thresholds.len() && thresholds[next].0 < bound {
          shift += thresholds[next].1;
          next += 1;
        }
        if shift != 0 {
          span.unicode_start = span.unicode_start.saturating_add_signed(shift);
        }
        if let Some(own_delta) = span
          .paragraph
          .and_then(|paragraph_ix| delta_by_paragraph.get(&paragraph_ix))
        {
          span.unicode_len = span.unicode_len.saturating_add_signed(*own_delta);
        }
      }
    }
    // §act-ten A10.3: the trailing structural patch, spliced incrementally.
    // Runs AFTER the text merge-walk so positions reflect the content patches
    // (patch synthesis touches disjoint paragraphs: e.g. a join edits the
    // surviving paragraph and deletes the removed rows). Every Enter and
    // paragraph-joining Backspace used to pay a full `from_projection` here —
    // O(doc chars) rope counting plus five map rebuilds on the actor thread.
    if let Some(patch) = structural {
      return (!self.try_splice_structural(projection, patch), table_refresh);
    }
    (false, table_refresh)
  }

  /// §act-ten A10.3: refresh object-derived index entries from the POST-apply
  /// projection (`ReplaceObjectBlock` patches). Tables refresh their cell
  /// layout entry; the asset-reference map is rebuilt outright when any object
  /// was replaced — an image replace can change `asset_id`, and the map's
  /// per-asset vectors are block-ordered, so an in-place remove/re-add would
  /// break the ordering invariant the fresh build guarantees. One O(blocks)
  /// filter pass, no rope work; object replaces are rare.
  fn refresh_object_entries(&mut self, projection: &DocumentProjection, replaced_ids: &[flowstate_document::BlockId]) {
    if replaced_ids.is_empty() {
      return;
    }
    for id in replaced_ids {
      let Some(&row) = self.block_anchor_by_id.get(id) else {
        continue;
      };
      if let Some(Block::Table(table)) = projection.blocks.get(row) {
        self
          .table_cells_by_block
          .insert(*id, table_index_entry(table));
      }
    }
    self.asset_refs_by_id.clear();
    for (block_ix, block) in projection.blocks.iter().enumerate() {
      if let Block::Image(image) = block
        && let Some(block_id) = projection.ids.block_ids.get(block_ix)
      {
        self
          .asset_refs_by_id
          .entry(image.asset_id)
          .or_default()
          .push(*block_id);
      }
    }
  }

  /// §act-ten A10.3 oracle: field-by-field equality against a fresh
  /// `from_projection` of the POST-patch projection. Debug builds only —
  /// O(doc), exercised by the convergence/intent fuzz suites.
  #[cfg(debug_assertions)]
  fn debug_assert_matches_fresh(&self, projection: &DocumentProjection) {
    let fresh = Self::from_projection(projection);
    debug_assert_eq!(
      self.paragraph_body_unicode_starts, fresh.paragraph_body_unicode_starts,
      "A10.3 splice: paragraph starts diverged"
    );
    debug_assert_eq!(self.body_unicode_end, fresh.body_unicode_end, "A10.3 splice: body end diverged");
    debug_assert_eq!(
      self.paragraph_boundary_positions, fresh.paragraph_boundary_positions,
      "A10.3 splice: boundaries diverged"
    );
    debug_assert_eq!(
      self.object_placeholder_positions, fresh.object_placeholder_positions,
      "A10.3 splice: object positions diverged"
    );
    debug_assert_eq!(
      self.paragraph_metadata_by_id, fresh.paragraph_metadata_by_id,
      "A10.3 splice: paragraph id index diverged"
    );
    debug_assert_eq!(self.block_anchor_by_id, fresh.block_anchor_by_id, "A10.3 splice: block id index diverged");
    debug_assert_eq!(
      self.table_cells_by_block, fresh.table_cells_by_block,
      "A10.3 splice: table index diverged"
    );
    debug_assert_eq!(
      self.style_runs_by_paragraph, fresh.style_runs_by_paragraph,
      "A10.3 splice: style intervals diverged"
    );
    debug_assert_eq!(
      self.section_anchor_by_id, fresh.section_anchor_by_id,
      "A10.3 splice: section anchors diverged"
    );
    debug_assert_eq!(self.asset_refs_by_id, fresh.asset_refs_by_id, "A10.3 splice: asset refs diverged");
    debug_assert_eq!(self.search_unit_spans, fresh.search_unit_spans, "A10.3 splice: search spans diverged");
  }

  /// §act-ten A10.3: incrementally splice ONE `InsertBlocks`/`DeleteBlocks` of
  /// PARAGRAPH rows into the index (the shapes local split/join/cross-delete
  /// synthesize). Returns `false` (caller rebuilds) for anything outside the
  /// gated shape: object rows, non-contiguous deletes, doc-head edits, or an
  /// object inside the ±2 row window (mirroring the editor's in-place gate —
  /// outside the window the unicode math is unambiguous because neighbors are
  /// paragraphs). `projection` is the PRE-patch state.
  fn try_splice_structural(&mut self, projection: &DocumentProjection, patch: &ProjectionPatch) -> bool {
    const SPLICE_MAX_ROWS: usize = 8;
    let old_paragraph_count = projection.paragraphs.len();
    let old_block_count = projection.blocks.len();
    let object_free_window = |center: usize| {
      let start = center.saturating_sub(2);
      let end = (center + 2).min(old_block_count.saturating_sub(1));
      (start..=end).all(|row| matches!(projection.blocks.get(row), None | Some(Block::Paragraph(_))))
    };
    // Paragraph rank of the first row at-or-after `row` (rows in the window are
    // paragraphs, so the O(log N) tree rank query applies directly).
    let paragraph_rank_at = |row: usize| -> Option<usize> {
      if row == old_block_count {
        return Some(old_paragraph_count);
      }
      projection.blocks.paragraph_ix_for_block_row(row)
    };
    match patch {
      ProjectionPatch::InsertBlocks { before, row_hint, blocks } => {
        if blocks.len() > SPLICE_MAX_ROWS || blocks.is_empty() {
          return false;
        }
        let mut inserted: Vec<(flowstate_document::BlockId, ParagraphId, usize, Vec<StyleInterval>)> = Vec::with_capacity(blocks.len());
        for block in blocks {
          let (InputBlock::Paragraph(paragraph), Some(paragraph_id)) = (&block.block, block.paragraph_id) else {
            return false;
          };
          let char_count: usize = paragraph
            .runs
            .iter()
            .map(|run| run.text.chars().count())
            .sum();
          let mut intervals = Vec::with_capacity(paragraph.runs.len());
          let mut start = 0usize;
          for run in &paragraph.runs {
            intervals.push(StyleInterval {
              start,
              len: run.text.len(),
              styles: run.styles,
            });
            start = start.saturating_add(run.text.len());
          }
          inserted.push((block.block_id, paragraph_id, char_count, intervals));
        }
        let row = match before {
          Some(before_id) => match self.block_anchor_by_id.get(before_id).copied() {
            Some(row) => row,
            None => return false,
          },
          None => old_block_count,
        };
        let _ = row_hint;
        if !object_free_window(row) {
          return false;
        }
        let Some(paragraph_at) = paragraph_rank_at(row) else {
          return false;
        };
        // Doc-head inserts hit the boundary-0 special case — leave to rebuild.
        if paragraph_at == 0 || old_paragraph_count == 0 {
          return false;
        }
        let base = match self.paragraph_body_unicode_starts.get(paragraph_at) {
          Some(start) => *start,
          None => self.body_unicode_end.saturating_add(1),
        };
        let mut acc = 0usize;
        let mut new_starts = Vec::with_capacity(inserted.len());
        let mut new_boundaries = Vec::with_capacity(inserted.len());
        for (_, _, char_count, _) in &inserted {
          new_boundaries.push(base + acc - 1);
          new_starts.push(base + acc);
          acc += char_count + 1;
        }
        let shift = acc as isize;
        let k = inserted.len();
        self
          .paragraph_body_unicode_starts
          .splice(paragraph_at..paragraph_at, new_starts.iter().copied());
        for start in &mut self.paragraph_body_unicode_starts[paragraph_at + k..] {
          *start = start.saturating_add_signed(shift);
        }
        self
          .paragraph_boundary_positions
          .splice(paragraph_at..paragraph_at, new_boundaries.iter().copied());
        for boundary in &mut self.paragraph_boundary_positions[paragraph_at + k..] {
          *boundary = boundary.saturating_add_signed(shift);
        }
        for position in &mut self.object_placeholder_positions {
          if *position >= base - 1 {
            *position = position.saturating_add_signed(shift);
          }
        }
        // Suffix re-index: ids after the splice point move by k (paragraphs)
        // and k rows (blocks). O(suffix) hash updates — no rope work.
        for (offset, id) in projection.ids.paragraph_ids[paragraph_at.min(projection.ids.paragraph_ids.len())..]
          .iter()
          .enumerate()
        {
          self
            .paragraph_metadata_by_id
            .insert(*id, paragraph_at + k + offset);
        }
        for (offset, id) in projection.ids.block_ids[row.min(projection.ids.block_ids.len())..]
          .iter()
          .enumerate()
        {
          self.block_anchor_by_id.insert(*id, row + k + offset);
        }
        for ix in (paragraph_at..old_paragraph_count).rev() {
          if let Some(intervals) = self.style_runs_by_paragraph.remove(&ix) {
            self.style_runs_by_paragraph.insert(ix + k, intervals);
          }
        }
        let span_from = row.min(self.search_unit_spans.len());
        for span in &mut self.search_unit_spans[span_from..] {
          span.unicode_start = span.unicode_start.saturating_add_signed(shift);
          if let Some(paragraph_ix) = &mut span.paragraph {
            *paragraph_ix += k;
          }
        }
        for (i, (block_id, paragraph_id, char_count, intervals)) in inserted.into_iter().enumerate() {
          self
            .paragraph_metadata_by_id
            .insert(paragraph_id, paragraph_at + i);
          self.block_anchor_by_id.insert(block_id, row + i);
          self
            .style_runs_by_paragraph
            .insert(paragraph_at + i, intervals);
          self.search_unit_spans.insert(
            row + i,
            SearchUnitSpan {
              paragraph: Some(paragraph_at + i),
              unicode_start: new_starts[i],
              unicode_len: char_count,
            },
          );
        }
        self.body_unicode_end = self.body_unicode_end.saturating_add_signed(shift);
        true
      },
      ProjectionPatch::DeleteBlocks { block_ids, .. } => {
        let k = block_ids.len();
        if k == 0 || k > SPLICE_MAX_ROWS {
          return false;
        }
        let mut rows: Vec<usize> = Vec::with_capacity(k);
        for id in block_ids {
          match self.block_anchor_by_id.get(id).copied() {
            Some(row) => rows.push(row),
            None => return false,
          }
        }
        rows.sort_unstable();
        let row = rows[0];
        if rows.windows(2).any(|pair| pair[1] != pair[0] + 1) {
          return false;
        }
        if rows
          .iter()
          .any(|row| !matches!(projection.blocks.get(*row), Some(Block::Paragraph(_))))
        {
          return false;
        }
        if !object_free_window(row) || !object_free_window(row + k - 1) {
          return false;
        }
        let Some(paragraph_at) = paragraph_rank_at(row) else {
          return false;
        };
        if paragraph_at == 0 || paragraph_at + k > old_paragraph_count {
          return false;
        }
        let Some(&lower) = self.paragraph_body_unicode_starts.get(paragraph_at) else {
          return false;
        };
        let upper = match self.paragraph_body_unicode_starts.get(paragraph_at + k) {
          Some(start) => *start,
          None => self.body_unicode_end.saturating_add(1),
        };
        let shift = (upper - lower) as isize;
        self
          .paragraph_body_unicode_starts
          .drain(paragraph_at..paragraph_at + k);
        for start in &mut self.paragraph_body_unicode_starts[paragraph_at..] {
          *start = start.saturating_add_signed(-shift);
        }
        self
          .paragraph_boundary_positions
          .drain(paragraph_at..paragraph_at + k);
        for boundary in &mut self.paragraph_boundary_positions[paragraph_at..] {
          *boundary = boundary.saturating_add_signed(-shift);
        }
        for position in &mut self.object_placeholder_positions {
          if *position >= lower {
            *position = position.saturating_add_signed(-shift);
          }
        }
        for id in &projection.ids.paragraph_ids[paragraph_at..(paragraph_at + k).min(projection.ids.paragraph_ids.len())] {
          self.paragraph_metadata_by_id.remove(id);
        }
        for (offset, id) in projection.ids.paragraph_ids[(paragraph_at + k).min(projection.ids.paragraph_ids.len())..]
          .iter()
          .enumerate()
        {
          self
            .paragraph_metadata_by_id
            .insert(*id, paragraph_at + offset);
        }
        for id in block_ids {
          self.block_anchor_by_id.remove(id);
        }
        for (offset, id) in projection.ids.block_ids[(row + k).min(projection.ids.block_ids.len())..]
          .iter()
          .enumerate()
        {
          self.block_anchor_by_id.insert(*id, row + offset);
        }
        for ix in paragraph_at..paragraph_at + k {
          self.style_runs_by_paragraph.remove(&ix);
        }
        for ix in paragraph_at + k..old_paragraph_count {
          if let Some(intervals) = self.style_runs_by_paragraph.remove(&ix) {
            self.style_runs_by_paragraph.insert(ix - k, intervals);
          }
        }
        self
          .search_unit_spans
          .drain(row..(row + k).min(self.search_unit_spans.len()));
        let span_from = row.min(self.search_unit_spans.len());
        for span in &mut self.search_unit_spans[span_from..] {
          span.unicode_start = span.unicode_start.saturating_add_signed(-shift);
          if let Some(paragraph_ix) = &mut span.paragraph {
            *paragraph_ix -= k;
          }
        }
        self.body_unicode_end = self.body_unicode_end.saturating_add_signed(-shift);
        true
      },
      _ => false,
    }
  }
}

fn paragraph_index_for_block_row(projection: &DocumentProjection, row: usize) -> Option<usize> {
  // §act-ten A10.8: O(log N) tree rank (was an O(row) block walk per patch).
  projection.blocks.paragraph_ix_for_block_row(row)
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
    // §act-twelve A12.1.0 (`FLOWSTATE_OPEN_PROBE=1`): stage timings for the
    // cold-open pipeline — where the seconds actually sit, not where they
    // sat before patch #18/#24.
    static OPEN_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let probe = *OPEN_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_OPEN_PROBE").is_some());
    let path = path.as_ref();
    let t0 = std::time::Instant::now();
    let package = DocumentPackage::read(path).with_context(|| format!("reading Flowstate package {}", path.display()))?;
    let t_read = t0.elapsed();
    let t1 = std::time::Instant::now();
    let projection = package
      .current_projection_document()
      .context("reading frontier-matched package projection cache")?;
    let t_cache = t1.elapsed();
    let cache_hit = projection.is_some();
    // `read` just validated the package; the plain `load_loro_doc` would run
    // a SECOND full validate (re-hashing every segment + asset) on every open.
    let t2 = std::time::Instant::now();
    // §act-twelve A12.1.3 (default ON): decode the shallow accelerator chunk
    // instead of the full history when the package carries one. Every
    // anomaly — no chunk, stale chunk, a segment the shallow state cannot
    // accept — falls back to the full decode below.
    static SHALLOW_OPEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let shallow_armed = *SHALLOW_OPEN.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_SHALLOW_OPEN").is_none());
    let mut opened_shallow = false;
    let shallow_doc = if shallow_armed {
      package.load_loro_doc_shallow().unwrap_or_else(|error| {
        eprintln!("[flowstate-shallow-open] shallow decode failed; falling back to full history: {error}");
        None
      })
    } else {
      None
    };
    let doc = match shallow_doc {
      Some(doc) => {
        opened_shallow = true;
        doc
      },
      None => package
        .load_loro_doc_from_validated()
        .context("loading Loro document from package")?,
    };
    let t_doc = t2.elapsed();
    let t3 = std::time::Instant::now();
    let mut runtime = Self::from_doc_with_projection(doc, Some(package), Some(path.to_path_buf()), projection)?;
    runtime.package_journal_prepared = true;
    if probe {
      eprintln!(
        "[flowstate-open-probe] read={t_read:?} projection_cache={t_cache:?} (hit={cache_hit}) loro_doc={t_doc:?} (shallow={opened_shallow}) runtime_construct={:?} total={:?}",
        t3.elapsed(),
        t0.elapsed()
      );
    }
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
    static OPEN_PROBE2: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let open_probe = *OPEN_PROBE2.get_or_init(|| std::env::var_os("FLOWSTATE_OPEN_PROBE").is_some());
    let probe_t0 = std::time::Instant::now();
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
    let probe_t_repair = probe_t0.elapsed();
    let probe_t1 = std::time::Instant::now();
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
    let probe_t_projection = probe_t1.elapsed();
    if open_probe {
      eprintln!("[flowstate-open-probe]   construct: repair_scan={probe_t_repair:?} projection_decide={probe_t_projection:?}");
    }
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
      static DERIVE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
      if *DERIVE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some()) {
        eprintln!(
          "  subscription: origin {} trigger {} epoch {} changes {}",
          summary.origin,
          summary.triggered_by,
          summary.epoch,
          summary.changes.len()
        );
      }
      if let Ok(mut events) = subscription_events_for_callback.lock() {
        events.push(summary);
      }
    }));
    // Loro-first spec §16: source-of-truth ops-per-commit tap. `ChangeMeta.len`
    // is measured by Loro itself at commit time — immune to instrumentation
    // drift. Callback discipline per I-9b: copy the numbers out, touch nothing.
    let ops_committed_total = Arc::new(AtomicU64::new(0));
    let commits_total = Arc::new(AtomicU64::new(0));
    let ops_for_callback = Arc::clone(&ops_committed_total);
    let commits_for_callback = Arc::clone(&commits_total);
    let pre_commit_subscription = doc.subscribe_pre_commit(Box::new(move |payload| {
      ops_for_callback.fetch_add(payload.change_meta.len as u64, AtomicOrdering::Relaxed);
      commits_for_callback.fetch_add(1, AtomicOrdering::Relaxed);
      true
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
    let undo = UndoManager::new(doc.inner());
    // Loro-first spec §10 (D6): time-based merge OFF — undo units are explicit
    // group_start/group_end boundaries driven by input semantics.
    undo.set_merge_interval(0);
    undo.set_max_undo_steps(300);
    undo.add_exclude_origin_prefix("remote");
    // §P2a: canonical repair commits carry the `repair` origin and must never
    // enter the local undo stack (they are convergent housekeeping, not edits).
    undo.add_exclude_origin_prefix("repair");
    // §act-ten A10.2 (vendor patch #14): metadata/revision commits (autosave
    // checkpoints) are INERT for undo — they must not create undo steps, and
    // must not mark the stacks dirty either (the plain excluded-origin path
    // composes them as remote-like diffs, flipping `peek_top_span`'s `clean`
    // flag and forcing the O(doc) slow path after every autosave). Sound
    // because "meta" commits write only meta/revisions containers, never
    // content an undo item references.
    undo.add_inert_origin_prefix("meta");
    let undo_selection = Arc::new(Mutex::new(UndoSelectionState::default()));
    install_undo_selection_callbacks(&undo, &undo_selection);
    let projection_index = ProjectionRuntimeIndex::from_projection(&projection);
    // §6-R: registry container ids, read-only (no `ensure_*` — construction
    // must not leave uncommitted marker writes in the pending txn).
    let registry_root = doc.get_map(ROOT);
    let paragraph_registry_container_id = child_map(&registry_root, PARAGRAPHS_BY_ID)
      .map(|map| map.id().to_string())
      .unwrap_or_default();
    let block_registry_container_id = child_map(&registry_root, BLOCKS_BY_ID)
      .map(|map| map.id().to_string())
      .unwrap_or_default();
    let mut runtime = Self {
      doc,
      projection,
      projection_index,
      undo,
      defer_undo_checkpoints: false,
      undo_checkpoint_pending: false,
      author_user_id: None,
      pending_author_identity: None,
      ops_committed_total,
      commits_total,
      _pre_commit_subscription: pre_commit_subscription,
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
      pending_replica_touch: true,
      editor_stream: Vec::new(),
      pending_publish: Vec::new(),
      paragraph_registry_container_id,
      block_registry_container_id,
      recorded_undo: Vec::new(),
      recorded_redo: Vec::new(),
      import_batches_served: 0,
      _root_subscription: root_subscription,
      _local_update_subscription: local_update_subscription,
    };
    // §P2a: repair any malformed canonical state the initial projection reported.
    // At construction there is no peer channel to emit onto, but the repair is
    // committed + persisted, so peers receive it via the update segment / next
    // anti-entropy pull. Errors are logged, never fatal to opening the document.
    if !startup_defects.is_empty()
      && let Err(error) = runtime.schedule_projection_repairs(startup_defects, RepairEmission::Silent)
    {
      tracing::error!(%error, "scheduling projection repairs during runtime construction failed");
    }
    Ok(runtime)
  }

  /// The live doc. Callers must hold the write gate for any barrier-capable
  /// use (spec I-9a); integration tests access it through their gate guards.
  pub fn doc(&self) -> &LoroDoc {
    &self.doc
  }

  /// §16 commit metrics: (total commits, total Loro ops) recorded at the
  /// pre-commit tap since this core was constructed.
  #[must_use]
  pub fn commit_metrics(&self) -> (u64, u64) {
    (
      self.commits_total.load(AtomicOrdering::Relaxed),
      self.ops_committed_total.load(AtomicOrdering::Relaxed),
    )
  }

  // ---- Loro-first local-write core surface (spec §4/§6) --------------------

  pub(crate) fn projection_ref(&self) -> &DocumentProjection {
    &self.projection
  }

  pub(crate) fn projection_index_ref(&self) -> &ProjectionRuntimeIndex {
    &self.projection_index
  }

  pub(crate) fn set_projection_frontier(&mut self, frontier: Vec<u8>) {
    self.projection.frontier = frontier;
  }

  pub(crate) fn undo_manager_mut(&mut self) -> &mut UndoManager {
    &mut self.undo
  }

  /// §act-three B.1 / §act-five P3-deep: the recorded-inverse fast-path stacks
  /// (see `local_write::recorded_inverse`).
  pub(crate) fn recorded_undo_stack(&mut self) -> &mut Vec<crate::local_write::recorded_inverse::RecordedInverse> {
    &mut self.recorded_undo
  }

  pub(crate) fn recorded_redo_stack(&mut self) -> &mut Vec<crate::local_write::recorded_inverse::RecordedInverse> {
    &mut self.recorded_redo
  }

  /// §act-eleven C8: batched-import calls served (one per gate hold). Test
  /// observability for the `doc_io` coalescing bound.
  #[must_use]
  pub fn import_batches_served(&self) -> u64 {
    self.import_batches_served
  }

  /// Drop the whole fast-path cache — a frontier break (remote import, checkout,
  /// or a slow-path step) invalidates every recorded inverse at once.
  pub(crate) fn clear_recorded_inverse(&mut self) {
    self.recorded_undo.clear();
    self.recorded_redo.clear();
  }

  /// §oom-leads #9 (mass-op collab): decide the recorded fast-path stacks' fate
  /// after an APPLYING remote import — rebase them through hull-disjoint remote
  /// changes instead of the old unconditional clear (which made the O(change)
  /// undo fast path effectively dead under live collab: any partner keystroke
  /// between an edit and Ctrl-Z forced the O(doc/history) slow path — measured
  /// seconds-to-minutes on large docs). Clears for anything the disjointness
  /// proof cannot cover: checkouts, structural churn, unclassifiable diffs,
  /// section-map changes, or remote body changes at/after the recorded hull.
  fn reconcile_recorded_inverse_after_remote(&mut self, invalidation: &ProjectionInvalidation, drained: &DrainedBodyDelta) {
    if self.recorded_undo.is_empty() && self.recorded_redo.is_empty() {
      return;
    }
    let unrebasable = drained.checkout_seen
      || drained.net.structural_churn
      || !invalidation.changed_sections.is_empty()
      || matches!(
        invalidation.fallback_reason,
        Some("checkout_trigger_projection_rebuild" | "unknown_loro_subscription_diff")
      );
    if unrebasable {
      self.clear_recorded_inverse();
      return;
    }
    let body_ranges_post: Vec<(usize, usize)> = invalidation
      .changed_text_ranges
      .iter()
      .filter(|range| range.flow_id == ROOT_BODY_FLOW_ID)
      .map(|range| (range.unicode_start, range.unicode_len))
      .collect();
    // Same structure law the derive ladder classifies by: the import neither
    // inserted a boundary/placeholder nor deleted a pre-existing boundary or
    // object placeholder (any of those changes the projection's ROW set, which
    // pre-recorded patches index into).
    let structure_neutral = !drained.net.inserts_structure()
      && !drained
        .net
        .deletes_any_position(self.projection_index.boundary_positions())
      && !drained
        .net
        .deletes_any_position(self.projection_index.object_positions());
    let net = drained.net.clone();
    if !crate::local_write::recorded_inverse::rebase_recorded_inverse_through_remote(self, &net, &body_ranges_post, structure_neutral) {
      tracing::debug!("recorded-inverse stacks cleared by remote import (rebase declined)");
    }
  }

  /// Push an editor-bound projection change onto the ordered stream (gate-held
  /// callers only — ordering is the whole point).
  pub(crate) fn push_editor_stream(&mut self, item: gpui_flowtext::ProjectionStreamItem) {
    self.editor_stream.push(item);
  }

  /// Drain the ordered projection stream for the editor (single consumer).
  pub(crate) fn take_editor_stream(&mut self) -> Vec<gpui_flowtext::ProjectionStreamItem> {
    std::mem::take(&mut self.editor_stream)
  }

  /// Queue committed-change events for the I/O service to drain (spec §6).
  pub(crate) fn queue_publish(&mut self, events: Vec<RuntimeEvent>) {
    self.pending_publish.extend(events);
  }

  /// Drain queued publish events. Called by the I/O service under the gate.
  pub(crate) fn take_pending_publish(&mut self) -> Vec<RuntimeEvent> {
    std::mem::take(&mut self.pending_publish)
  }

  /// Spec §7 audit law: the patch-applied projection must equal an independent
  /// full materialization (modulo the documented whitelist, which is embodied
  /// by `projections_semantically_equal`). On mismatch: loud metric + snapshot
  /// repair by installing the full rebuild.
  #[hotpath::measure]
  pub(crate) fn audit_projection_against_full_rebuild(&mut self, class: &str) -> Result<()> {
    let full = document_from_loro(&self.doc).context("audit: full materialization failed")?;
    if projections_semantically_equal(&self.projection, &full) {
      return Ok(());
    }
    let detail = first_projection_divergence(&self.projection, &full);
    tracing::error!(
      class,
      detail,
      "audit-mismatch: patch-applied projection != full rebuild; installing snapshot repair"
    );
    if let Ok(mut counts) = self.projection_fallback_counts.lock() {
      *counts.entry("audit-mismatch".to_string()).or_default() += 1;
    }
    let frontier = self.projection.frontier.clone();
    self.projection = full;
    self.projection.frontier = frontier;
    self.projection_index = ProjectionRuntimeIndex::from_projection(&self.projection);
    anyhow::bail!("projection audit mismatch for intent class {class}: {detail}")
  }

  /// §15: bind a stable durable author identity to this live replica.
  ///
  /// Registers (or refreshes) the user record in `users_by_id`, links the current
  /// Loro replica to that user, and stores the id so later revisions record it as
  /// their author. Until this is called, `author_user_id` stays `None` and
  /// authorship is left unset (behavior unchanged).
  pub fn set_author_identity(&mut self, user_id: u128, display_name: Option<String>) -> Result<Vec<RuntimeEvent>> {
    // Loro-first spec §16: registration is LAZY — deferred to this replica's
    // first actual edit so an open-and-look session commits no identity noise.
    self.author_user_id = Some(user_id);
    self.pending_author_identity = Some((user_id, display_name));
    Ok(Vec::new())
  }

  /// Register the deferred author identity (spec §16). Called by the local
  /// write path right after this replica's first committed edit; the identity
  /// commit rides the excluded `repair` origin so it never enters undo stacks.
  pub(crate) fn register_pending_author_identity(&mut self) {
    let identity = self.pending_author_identity.take();
    // §act-twelve A12.1.1: the replica `last_seen_at` touch is DEFERRED from
    // open to the first real edit (an open-time commit invalidated the
    // package's frontier-stamped caches for every later open).
    let replica_touch = std::mem::take(&mut self.pending_replica_touch);
    if identity.is_none() && !replica_touch {
      return;
    }
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    // §act-twelve A12.1.1: the INERT "meta" origin (vendor #14), not the
    // excluded "repair" one — an excluded-origin commit composes into the
    // Loro undo stacks as a remote-like diff and flips `clean` for the whole
    // session, which silently killed the recorded-inverse fast path on every
    // doc once the replica touch moved here (fast undo died after the FIRST
    // edit). Identity/replica writes only touch meta-class containers, so
    // inert is sound; the recorded stacks' frontier tops are re-armed below,
    // mirroring the autosave checkpoint (A10.2).
    self.doc.set_next_commit_origin("meta");
    self.doc.set_next_commit_message("author-identity");
    if replica_touch && let Err(error) = flowstate_document::register_replica(&self.doc, None) {
      tracing::warn!(%error, "deferred replica registration failed");
    }
    if let Some((user_id, display_name)) = identity
      && let Err(error) = flowstate_document::register_user(&self.doc, user_id, display_name.as_deref())
    {
      tracing::warn!(%error, "registering deferred author identity failed");
      return;
    }
    let frontier_before_meta = self.projection.frontier.clone();
    match self.events_after_local_change(from_frontier, from_vv, ProjectionInvalidation::default(), false) {
      Ok(events) => self.queue_publish(events),
      Err(error) => tracing::warn!(%error, "publishing deferred author-identity update failed; anti-entropy recovers it"),
    }
    // The meta commit advanced the frontier: (a) keep the recorded fast-path
    // tops armed across it (A10.2 law); (b) advance the maintained projection
    // frontier and stream an EMPTY patch batch so the editor's base-frontier
    // chain stays gapless (the ordered-stream field bug class — the #40
    // neutral-repair emission, same law).
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    self.projection.frontier = self.doc.state_frontiers().encode();
    let invalidation = ProjectionInvalidation {
      frontier_before: frontier_before_meta,
      frontier_after: self.projection.frontier.clone(),
      ..Default::default()
    };
    let empty: std::sync::Arc<[ProjectionPatch]> = std::sync::Arc::from(Vec::new());
    let event = self.projection_patched_event(empty, invalidation);
    self.queue_publish(vec![event]);
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

  /// Re-park a restored-selection snapshot that could not resolve against the
  /// current projection yet (mirrors the slow undo path's deferral).
  pub(crate) fn restore_undo_selection_later(&mut self, snapshot: UndoSelectionSnapshot) {
    if let Ok(mut state) = self.undo_selection.lock() {
      state.restored_selection = Some(snapshot);
    }
  }

  #[hotpath::measure]
  pub(crate) fn record_undo_checkpoint(&mut self) -> Result<()> {
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
    let root = flowstate_document::loro_schema::root_map(&self.doc);
    let users = root.get(flowstate_document::loro_schema::USERS_BY_ID);
    self
      .package
      .as_ref()
      .map(|package| {
        package
          .revisions
          .iter()
          .rev()
          .map(|revision| {
            let author_display_name = revision.author_user_id.and_then(|user_id| {
              let ValueOrContainer::Container(Container::Map(users)) = users.as_ref()? else {
                return None;
              };
              let Some(ValueOrContainer::Container(Container::Map(user))) = users.get(&user_id.to_string()) else {
                return None;
              };
              map_string_opt(&user, "display_name")
            });
            RuntimeRevisionInfo {
              revision_id: revision.revision_id,
              title: revision.title.clone(),
              summary: revision.summary.clone(),
              kind: revision.kind,
              frontier: revision.frontier.clone(),
              created_at_unix_secs: revision.created_at_unix_secs,
              author_user_id: revision.author_user_id,
              author_display_name,
              replica_id: revision.replica_id,
            }
          })
          .collect()
      })
      .unwrap_or_default()
  }

  /// Materialize durable comment threads and resolve their CRDT anchors
  /// against the current document. A deleted anchor remains discoverable with
  /// `anchor: None` and its quoted-text fallback intact.
  pub fn comments(&self) -> Vec<RuntimeCommentThread> {
    let root = flowstate_document::loro_schema::root_map(&self.doc);
    let Some(ValueOrContainer::Container(Container::Map(comments))) = root.get(flowstate_document::COMMENTS_BY_ID) else {
      return Vec::new();
    };
    let mut threads = Vec::new();
    comments.for_each(|key, value| {
      let ValueOrContainer::Container(Container::Map(thread)) = value else {
        return;
      };
      let Some(comment_id) = key.parse::<u128>().ok() else {
        return;
      };
      let head_cursor = map_binary_opt(&thread, "head_cursor");
      let anchor_cursor = map_binary_opt(&thread, "anchor_cursor");
      let anchor = head_cursor
        .zip(anchor_cursor)
        .and_then(|(head, anchor)| self.resolve_selection_anchor(&head, &anchor))
        .map(|(head, anchor)| if anchor <= head { (anchor, head) } else { (head, anchor) })
        .filter(|(start, end)| start != end);
      let mut messages = Vec::new();
      if let Some(ValueOrContainer::Container(Container::Map(message_map))) = thread.get("messages_by_id") {
        message_map.for_each(|message_key, value| {
          let ValueOrContainer::Container(Container::Map(message)) = value else {
            return;
          };
          let Some(message_id) = message_key.parse().ok() else {
            return;
          };
          let Some(author_user_id) = map_string_opt(&message, "author_user_id").and_then(|id| id.parse().ok()) else {
            return;
          };
          messages.push(RuntimeCommentMessage {
            message_id,
            author_user_id,
            author_display_name: map_string_opt(&message, "author_display_name").unwrap_or_else(|| "Unknown author".into()),
            body: map_string_opt(&message, "body").unwrap_or_default(),
            created_at_unix_secs: map_i64_opt(&message, "created_at").unwrap_or_default(),
            updated_at_unix_secs: map_i64_opt(&message, "updated_at").unwrap_or_default(),
            deleted: map_bool_opt(&message, "deleted").unwrap_or(false),
          });
        });
      }
      messages.sort_by_key(|message| (message.created_at_unix_secs, message.message_id));
      threads.push(RuntimeCommentThread {
        comment_id,
        author_user_id: comment_thread_author(&thread),
        quoted_text: map_string_opt(&thread, "quoted_text").unwrap_or_default(),
        resolved: map_bool_opt(&thread, "resolved").unwrap_or(false),
        general: map_bool_opt(&thread, "general").unwrap_or(false),
        created_at_unix_secs: map_i64_opt(&thread, "created_at").unwrap_or_default(),
        updated_at_unix_secs: map_i64_opt(&thread, "updated_at").unwrap_or_default(),
        created_frontier: map_binary_opt(&thread, "created_frontier"),
        anchor,
        messages,
      });
    });
    threads.sort_by_key(|thread| (thread.resolved, thread.created_at_unix_secs, thread.comment_id));
    threads
  }

  /// C-S1: `selection: None` creates a GENERAL comment — feedback with no
  /// location (the F3 decision) — stored with no cursors and a `general`
  /// marker so it never reads as an orphan. Every thread records the
  /// frontier it was born at, which is what history-jump (C-S6) checks out.
  pub fn create_comment(
    &mut self,
    selection: Option<&EditorSelection>,
    body: &str,
    author_user_id: u128,
    author_display_name: &str,
  ) -> Result<u128> {
    let body = validated_comment_body(body)?;
    let anchored = match selection {
      Some(selection) => {
        anyhow::ensure!(selection.anchor != selection.head, "Select text before adding an anchored comment");
        let (head_cursor, anchor_cursor) = self
          .encode_selection_anchor(selection)
          .context("The selected text cannot be anchored in this document")?;
        let start = gpui_flowtext::global_byte(&self.projection, selection.anchor.min(selection.head));
        let end = gpui_flowtext::global_byte(&self.projection, selection.anchor.max(selection.head));
        let quoted_text = gpui_flowtext::document_text_slice(&self.projection, start..end);
        Some((head_cursor, anchor_cursor, quoted_text))
      },
      None => None,
    };
    self.register_pending_author_identity();
    let comment_id = Uuid::new_v4().as_u128();
    let message_id = Uuid::new_v4().as_u128();
    let now = unix_time_secs();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    flowstate_document::touch_document_metadata(&self.doc)?;
    let comments = flowstate_document::loro_schema::root_map(&self.doc).ensure_mergeable_map(flowstate_document::COMMENTS_BY_ID)?;
    let thread = comments.ensure_mergeable_map(&comment_id.to_string())?;
    thread.insert("id", comment_id.to_string())?;
    thread.insert("author_user_id", author_user_id.to_string())?;
    match &anchored {
      Some((head_cursor, anchor_cursor, quoted_text)) => {
        thread.insert("head_cursor", LoroValue::Binary(head_cursor.clone().into()))?;
        thread.insert("anchor_cursor", LoroValue::Binary(anchor_cursor.clone().into()))?;
        thread.insert("quoted_text", quoted_text.as_str())?;
      },
      None => {
        thread.insert("general", true)?;
        thread.insert("quoted_text", "")?;
      },
    }
    thread.insert("created_frontier", LoroValue::Binary(from_frontier.encode().into()))?;
    thread.insert("resolved", false)?;
    thread.insert("created_at", now)?;
    thread.insert("updated_at", now)?;
    let messages = thread.ensure_mergeable_map("messages_by_id")?;
    write_comment_message(&messages, message_id, &body, author_user_id, author_display_name, now)?;
    self.finish_comment_mutation(from_frontier, from_vv, "comment-create")?;
    Ok(comment_id)
  }

  /// C-S1: author-gated per-message delete leaving a tombstone — the thread
  /// keeps its shape ("message deleted") instead of replies losing referents.
  pub fn delete_comment_message(&mut self, comment_id: u128, message_id: u128, actor_user_id: u128) -> Result<()> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let thread = existing_comment_thread(&self.doc, comment_id)?;
    let messages = existing_child_map(&thread, "messages_by_id")?;
    let message = existing_child_map(&messages, &message_id.to_string())?;
    anyhow::ensure!(
      map_string_opt(&message, "author_user_id").and_then(|id| id.parse().ok()) == Some(actor_user_id),
      "Only the message author can delete it"
    );
    message.insert("deleted", true)?;
    message.insert("body", "")?;
    message.insert("updated_at", unix_time_secs())?;
    thread.insert("updated_at", unix_time_secs())?;
    self.finish_comment_mutation(from_frontier, from_vv, "comment-message-delete")
  }

  pub fn reply_to_comment(&mut self, comment_id: u128, body: &str, author_user_id: u128, author_display_name: &str) -> Result<u128> {
    let body = validated_comment_body(body)?;
    self.register_pending_author_identity();
    let message_id = Uuid::new_v4().as_u128();
    let now = unix_time_secs();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let thread = existing_comment_thread(&self.doc, comment_id)?;
    let messages = thread.ensure_mergeable_map("messages_by_id")?;
    write_comment_message(&messages, message_id, &body, author_user_id, author_display_name, now)?;
    thread.insert("updated_at", now)?;
    self.finish_comment_mutation(from_frontier, from_vv, "comment-reply")?;
    Ok(message_id)
  }

  pub fn set_comment_resolved(&mut self, comment_id: u128, resolved: bool) -> Result<()> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let thread = existing_comment_thread(&self.doc, comment_id)?;
    thread.insert("resolved", resolved)?;
    thread.insert("updated_at", unix_time_secs())?;
    self.finish_comment_mutation(from_frontier, from_vv, if resolved { "comment-resolve" } else { "comment-reopen" })
  }

  /// C-S6: give an orphaned thread a new live anchor. The quoted text follows
  /// the new target (the card must read as what it now points at), and the
  /// original `created_frontier` is deliberately left alone — history-jump
  /// still shows the thread's TRUE original context.
  pub fn reanchor_comment(&mut self, comment_id: u128, selection: &EditorSelection) -> Result<()> {
    anyhow::ensure!(selection.anchor != selection.head, "Select text before re-anchoring a comment");
    let (head_cursor, anchor_cursor) = self
      .encode_selection_anchor(selection)
      .context("The selected text cannot be anchored in this document")?;
    let start = gpui_flowtext::global_byte(&self.projection, selection.anchor.min(selection.head));
    let end = gpui_flowtext::global_byte(&self.projection, selection.anchor.max(selection.head));
    let quoted_text = gpui_flowtext::document_text_slice(&self.projection, start..end);
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let thread = existing_comment_thread(&self.doc, comment_id)?;
    thread.insert("head_cursor", LoroValue::Binary(head_cursor.into()))?;
    thread.insert("anchor_cursor", LoroValue::Binary(anchor_cursor.into()))?;
    thread.insert("quoted_text", quoted_text.as_str())?;
    // A re-anchored thread is no general note (it may have been rendered as
    // one while orphaned).
    thread.insert("general", false)?;
    thread.insert("updated_at", unix_time_secs())?;
    self.finish_comment_mutation(from_frontier, from_vv, "comment-reanchor")
  }

  /// H-S5: two-frontier diff + blame. Removed-since spans land in the BASE
  /// (historical) projection's coordinates; each span is attributed by the
  /// op id of its midpoint character (a span may cover several authors — the
  /// midpoint author wins, which is honest at paint granularity). Authorship
  /// resolves op peer → `replicas_by_id` → `users_by_id` (the schema's blame
  /// chain).
  pub fn frontier_diff_vs(&self, base_frontier: &[u8], newer_frontier: Option<&[u8]>) -> Result<RuntimeFrontierDiff> {
    let from = Frontiers::decode(base_frontier).context("decoding base frontier for diff")?;
    let to = match newer_frontier {
      Some(frontier) => Frontiers::decode(frontier).context("decoding newer frontier for diff")?,
      None => self.doc.state_frontiers(),
    };
    let batch = self
      .doc
      .diff(&from, &to)
      .map_err(|error| anyhow::anyhow!(error.to_string()))
      .context("computing two-frontier diff")?;
    let fork = self
      .doc
      .fork_at(&from)
      .context("forking document at the diff base frontier")?;
    let base_projection = document_from_loro(&fork).context("projecting the diff base")?;
    let index = ProjectionRuntimeIndex::from_projection(&base_projection);
    let text = body_text(&fork);
    let text_id = text.id();

    let mut diff = RuntimeFrontierDiff::default();
    for (container, container_diff) in batch.iter() {
      if *container != text_id {
        continue;
      }
      let loro::event::Diff::Text(deltas) = container_diff else {
        continue;
      };
      let mut base_pos = 0usize;
      for delta in deltas {
        match delta {
          loro::TextDelta::Retain { retain, .. } => base_pos += retain,
          loro::TextDelta::Insert { insert, .. } => diff.inserted_chars += insert.chars().count(),
          loro::TextDelta::Delete { delete } => {
            let start_unicode = base_pos;
            let end_unicode = base_pos + delete;
            base_pos = end_unicode;
            diff.removed_chars += delete;
            let author_peer = text
              .get_cursor(start_unicode + delete / 2, loro::cursor::Side::Middle)
              .and_then(|cursor| cursor.id)
              .map(|id| id.peer);
            let (author_user_id, author_display_name) = self.resolve_blame_author(author_peer);
            if let (Some(start), Some(end)) = (
              index.offset_for_body_unicode(&base_projection, start_unicode),
              index.offset_for_body_unicode(&base_projection, end_unicode),
            ) {
              diff.removed_since.push(RuntimeDiffSpan {
                start,
                end,
                author_user_id,
                author_display_name,
              });
            }
          },
        }
      }
    }
    Ok(diff)
  }

  /// The schema's blame chain: op peer → `replicas_by_id[peer].user_id` →
  /// `users_by_id[user].display_name`.
  fn resolve_blame_author(&self, peer: Option<u64>) -> (Option<u128>, Option<String>) {
    let Some(peer) = peer else { return (None, None) };
    let root = flowstate_document::loro_schema::root_map(&self.doc);
    let Some(ValueOrContainer::Container(Container::Map(replicas))) = root.get(flowstate_document::loro_schema::REPLICAS_BY_ID) else {
      return (None, None);
    };
    let Some(ValueOrContainer::Container(Container::Map(replica))) = replicas.get(&peer.to_string()) else {
      return (None, None);
    };
    let Some(user_id) = map_string_opt(&replica, "user_id").and_then(|id| id.parse::<u128>().ok()) else {
      return (None, None);
    };
    let display_name = match root.get(flowstate_document::loro_schema::USERS_BY_ID) {
      Some(ValueOrContainer::Container(Container::Map(users))) => match users.get(&user_id.to_string()) {
        Some(ValueOrContainer::Container(Container::Map(user))) => map_string_opt(&user, "display_name"),
        _ => None,
      },
      _ => None,
    };
    (Some(user_id), display_name)
  }

  /// C-S6 history-jump: the document as the comment author saw it — a
  /// read-only projection at the thread's `created_frontier`, plus the
  /// thread's anchor resolved AT that frontier (cursor bytes are stable ids,
  /// so the live thread's cursors resolve against the historical fork).
  pub fn frontier_comment_context(
    &self,
    frontier: &[u8],
    comment_id: u128,
  ) -> Result<(DocumentProjection, Option<(DocumentOffset, DocumentOffset)>)> {
    let frontiers = Frontiers::decode(frontier).context("decoding frontier blob for comment context")?;
    let historical_doc = self
      .doc
      .fork_at(&frontiers)
      .context("forking document at the comment's birth frontier")?;
    let mut document = document_from_loro(&historical_doc).context("projecting historical comment context")?;
    if let Some(package) = &self.package {
      attach_package_assets(&mut document, package);
    }
    let anchor = (|| {
      let thread = existing_comment_thread(&self.doc, comment_id).ok()?;
      let head_bytes = map_binary_opt(&thread, "head_cursor")?;
      let anchor_bytes = map_binary_opt(&thread, "anchor_cursor")?;
      let index = ProjectionRuntimeIndex::from_projection(&document);
      let text = body_text(&historical_doc);
      let resolve = |encoded: &[u8]| -> Option<DocumentOffset> {
        let cursor = Cursor::decode(encoded).ok()?;
        if cursor.container != text.id() {
          return None;
        }
        let resolved = historical_doc.get_cursor_pos(&cursor).ok()?;
        let unicode = resolved_cursor_boundary_unicode(&text, &resolved)?;
        index.offset_for_body_unicode(&document, unicode)
      };
      let (head, anchor) = (resolve(&head_bytes)?, resolve(&anchor_bytes)?);
      Some(if anchor <= head { (anchor, head) } else { (head, anchor) })
    })();
    Ok((document, anchor))
  }

  pub fn edit_comment_message(&mut self, comment_id: u128, message_id: u128, body: &str, actor_user_id: u128) -> Result<()> {
    let body = validated_comment_body(body)?;
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let thread = existing_comment_thread(&self.doc, comment_id)?;
    let messages = existing_child_map(&thread, "messages_by_id")?;
    let message = existing_child_map(&messages, &message_id.to_string())?;
    anyhow::ensure!(
      map_string_opt(&message, "author_user_id").and_then(|id| id.parse().ok()) == Some(actor_user_id),
      "Only the message author can edit it"
    );
    message.insert("body", body.as_str())?;
    message.insert("updated_at", unix_time_secs())?;
    thread.insert("updated_at", unix_time_secs())?;
    self.finish_comment_mutation(from_frontier, from_vv, "comment-edit")
  }

  pub fn delete_comment(&mut self, comment_id: u128, actor_user_id: u128) -> Result<()> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let root = flowstate_document::loro_schema::root_map(&self.doc);
    let comments = existing_child_map(&root, flowstate_document::COMMENTS_BY_ID)?;
    let thread = existing_child_map(&comments, &comment_id.to_string())?;
    // C-S1: ONE authority for thread authorship — the same resolution the
    // materializer exposes to the UI, so visibility and enforcement can
    // never diverge again.
    anyhow::ensure!(
      comment_thread_author(&thread) == Some(actor_user_id),
      "Only the thread author can delete it"
    );
    comments.delete(&comment_id.to_string())?;
    self.finish_comment_mutation(from_frontier, from_vv, "comment-delete")
  }

  fn finish_comment_mutation(&mut self, from_frontier: Frontiers, from_vv: VersionVector, message: &str) -> Result<()> {
    flowstate_document::touch_document_metadata(&self.doc)?;
    self.doc.set_next_commit_origin("comment");
    self.doc.set_next_commit_message(message);
    self.doc.commit();
    let frontier_before = self.projection.frontier.clone();
    let mut invalidation = ProjectionInvalidation::default();
    self.merge_subscription_invalidation(&mut invalidation);
    let events = self.events_after_local_change(from_frontier, from_vv, invalidation, false)?;
    self.queue_publish(events);
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    self.projection.frontier = self.doc.state_frontiers().encode();
    let empty: Arc<[ProjectionPatch]> = Arc::from(Vec::new());
    let _ = self.projection_patched_event(
      empty,
      ProjectionInvalidation {
        frontier_before,
        frontier_after: self.projection.frontier.clone(),
        ..Default::default()
      },
    );
    Ok(())
  }

  /// §caret-anchor: re-anchor an editor selection across a remote import using
  /// Loro cursors, so a concurrent insert/delete BEFORE the caret repositions it
  /// instead of stranding it at a stale projection offset (the concurrent-typing
  /// interleave bug). The caret is projection-space for rendering/navigation, but
  /// its STABILITY across remote edits comes from the CRDT — exactly as paragraph
  /// boundaries and object anchors already do.
  ///
  /// `before` is the editor's PRE-patch projection and `base_frontier` its
  /// pre-patch frontier. We reconstruct the pre-patch body (`fork_at`), anchor a
  /// cursor at each endpoint there, then resolve it in the CURRENT canonical doc.
  /// Returns `None` (empty/edge body, unresolvable cursor, structural mismatch) so
  /// the caller can fall back to clamping — never a silent wrong caret.
  pub fn rebase_selection_across_import(
    &self,
    selection: &EditorSelection,
    before: &DocumentProjection,
    base_frontier: &[u8],
  ) -> Option<EditorSelection> {
    // Instrument the O(doc) fork so a test can assert the fast path (stored
    // cursors) is taken in the common case rather than silently regressing to a
    // fork on every remote batch.
    CARET_REBASE_FORKS.with(|count| count.set(count.get().wrapping_add(1)));
    let base = Frontiers::decode(base_frontier).ok()?;
    let old_doc = self.doc.fork_at(&base).ok()?;
    let old_text = body_text(&old_doc);
    let cur_text = body_text(&self.doc);
    let before_index = ProjectionRuntimeIndex::from_projection(before);
    let rebase = |offset: DocumentOffset, affinity: SelectionAffinity| -> Option<DocumentOffset> {
      let old_unicode = before_index.body_unicode_for_offset(before, offset)?;
      let cursor = cursor_for_boundary(&old_text, old_unicode, affinity)?;
      let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
      let new_unicode = resolved_cursor_boundary_unicode(&cur_text, &resolved)?;
      self
        .projection_index
        .offset_for_body_unicode(&self.projection, new_unicode)
    };
    let head = rebase(selection.head, SelectionAffinity::from(selection.head_affinity))?;
    let anchor = rebase(selection.anchor, SelectionAffinity::from(selection.anchor_affinity))?;
    Some(EditorSelection {
      anchor,
      head,
      ..selection.clone()
    })
  }

  /// §caret-anchor FAST path: encode a selection's endpoints as Loro cursors
  /// against the CURRENT (synced) body. The editor stores these at synced moments
  /// (after a sync/local write) and, when a remote import arrives while the caret
  /// has NOT moved, resolves them with [`Self::resolve_selection_anchor`] to
  /// reposition the caret in O(log n) — avoiding the O(doc) `fork_at` of the
  /// general [`Self::rebase_selection_across_import`] fallback. Reuses the presence
  /// cursor machinery, so both stay bit-consistent.
  pub fn encode_selection_anchor(&self, selection: &EditorSelection) -> Option<(Vec<u8>, Vec<u8>)> {
    let head = self.presence_endpoint(
      selection.head,
      SelectionAffinity::from(selection.head_affinity),
      VisualGravity::from(selection.head_gravity),
    )?;
    let anchor = self.presence_endpoint(
      selection.anchor,
      SelectionAffinity::from(selection.anchor_affinity),
      VisualGravity::from(selection.anchor_gravity),
    )?;
    Some((head.cursor, anchor.cursor))
  }

  /// Resolve encoded caret cursors (from [`Self::encode_selection_anchor`]) to
  /// offsets in the CURRENT canonical state. `None` if either endpoint no longer
  /// resolves (e.g. its container was removed) so the caller can fall back.
  pub fn resolve_selection_anchor(&self, head_cursor: &[u8], anchor_cursor: &[u8]) -> Option<(DocumentOffset, DocumentOffset)> {
    Some((
      self.resolve_body_cursor_offset(head_cursor)?,
      self.resolve_body_cursor_offset(anchor_cursor)?,
    ))
  }

  fn resolve_body_cursor_offset(&self, encoded: &[u8]) -> Option<DocumentOffset> {
    let text = body_text(&self.doc);
    let cursor = Cursor::decode(encoded).ok()?;
    if cursor.container != text.id() {
      return None;
    }
    let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
    let unicode = resolved_cursor_boundary_unicode(&text, &resolved)?;
    self
      .projection_index
      .offset_for_body_unicode(&self.projection, unicode)
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
    // Resolve one presence endpoint's encoded cursor to a projection-space
    // DocumentOffset in the CURRENT canonical state (`None` if the cursor's
    // container is gone or no longer resolves).
    let resolve_endpoint = |cursor_bytes: &[u8]| -> Option<DocumentOffset> {
      let cursor = Cursor::decode(cursor_bytes).ok()?;
      if cursor.container != text.id() {
        return None;
      }
      let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
      let unicode = resolved_cursor_boundary_unicode(&text, &resolved)?;
      self
        .projection_index
        .offset_for_body_unicode(&self.projection, unicode)
    };
    let mut carets = Vec::with_capacity(requests.len());
    let mut selections = Vec::new();
    for request in requests {
      let Some(head_offset) = resolve_endpoint(&request.selection.head.cursor) else {
        continue;
      };
      carets.push(ExternalCaret {
        offset: head_offset,
        visual_gravity: gpui_gravity_from_presence(request.selection.head.visual_gravity),
        color_rgb: request.color_rgb,
      });
      // A non-collapsed selection also paints a highlighted span behind the
      // text. Skip it when the anchor no longer resolves or coincides with the
      // head (a bare caret) — the caret above already covers that case.
      if let Some(anchor_offset) = resolve_endpoint(&request.selection.anchor.cursor)
        && anchor_offset != head_offset
      {
        selections.push(ExternalSelection {
          selection: EditorSelection {
            anchor: anchor_offset,
            head: head_offset,
            ..EditorSelection::default()
          },
          color_rgb: request.color_rgb,
        });
      }
    }
    RuntimePresenceCarets { carets, selections }
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

  /// Record asset bytes/metadata into canonical state (Loro-first spec §6):
  /// additive `assets_by_id` metadata + the maintained projection's asset
  /// store, published like any local change and patched to editors as
  /// `AssetArrived`. Asset records are content-addressed sideband — never
  /// document text authority.
  pub fn merge_asset_records(&mut self, records: Vec<AssetRecord>) -> Result<Vec<RuntimeEvent>> {
    if records.is_empty() {
      return Ok(Vec::new());
    }
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let root = self.doc.get_map(ROOT);
    let assets = root
      .ensure_mergeable_map(flowstate_document::loro_schema::ASSETS_BY_ID)
      .context("opening canonical assets map")?;
    for record in &records {
      let asset_id = record.id.0.to_string();
      let asset_map = assets
        .ensure_mergeable_map(&asset_id)
        .context("opening canonical asset record")?;
      let hash = blake3::hash(&record.bytes);
      asset_map
        .insert("asset_id", asset_id.as_str())
        .context("writing asset id")?;
      asset_map
        .insert("content_hash", hash.to_hex().as_str())
        .context("writing asset content hash")?;
      asset_map
        .insert("mime_type", record.mime_type.as_ref())
        .context("writing asset mime type")?;
      asset_map
        .insert("byte_length", i64::try_from(record.bytes.len()).unwrap_or(i64::MAX))
        .context("writing asset byte length")?;
      if let Some(original_name) = &record.original_name {
        asset_map
          .insert("original_name", original_name.as_ref())
          .context("writing asset original name")?;
      }
    }
    refresh_image_asset_metadata(&self.doc).context("refreshing image asset metadata")?;
    // Asset records ride the repair origin: convergent housekeeping, never an
    // undo unit.
    self.doc.set_next_commit_origin(REPAIR_ORIGIN);
    self.doc.set_next_commit_message("asset-records");
    self.doc.commit();

    merge_asset_records_into_projection(&mut self.projection, &records);
    self.projection.frontier = self.doc.state_frontiers().encode();

    let mut invalidation = ProjectionInvalidation {
      frontier_before: from_frontier.encode(),
      frontier_after: self.doc.state_frontiers().encode(),
      changed_assets: records
        .iter()
        .map(|record| record.id.0.to_string())
        .collect(),
      ..ProjectionInvalidation::default()
    };
    self.merge_subscription_invalidation(&mut invalidation);
    let mut events = self.events_after_local_change(from_frontier, from_vv, invalidation.clone(), false)?;
    let patches: Vec<_> = records
      .into_iter()
      .map(|record| flowstate_document::ProjectionPatch::AssetArrived { id: record.id, record })
      .collect();
    events.push(self.projection_patched_event(patches.into(), invalidation));
    Ok(events)
  }

  pub fn command(&mut self, command: SemanticCommand) -> Result<Vec<RuntimeEvent>> {
    let restore_undo_selection = matches!(&command, SemanticCommand::Undo | SemanticCommand::Redo);
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
      // §act-three B.1: any other mutation invalidates the recorded inverse
      // (the frontier check would catch it; this frees the capture eagerly).
      self.clear_recorded_inverse();
    }
    #[allow(
      clippy::needless_late_init,
      reason = "assigned across match arms that interleave with diverging early-return arms"
    )]
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
      SemanticCommand::OpenFrontier { frontier } => {
        let document = self.frontier_projection(&frontier)?;
        return Ok(vec![RuntimeEvent::FrontierViewOpened {
          frontier,
          document: Box::new(document),
        }]);
      },
      SemanticCommand::Undo => {
        // §act-three B.1: replay the recorded inverse when the lineage checks
        // hold — no checkout, no diff calc, no rebuild. Falls through to the
        // UndoManager slow path on any mismatch.
        if let Some(events) = hotpath::measure_block!(
          "recorded_inverse_undo",
          crate::local_write::recorded_inverse::try_fast_step(self, loro::UndoOrRedo::Undo)?
        ) {
          return Ok(events);
        }
        let applied = hotpath::measure_block!("loro_undo_manager_undo", self.undo.undo().context("applying Loro undo")?);
        // §fidelity: record the undo's frontier transition and assert it only
        // introduced local-peer ops (remote-origin ops are excluded from undo).
        if fidelity::enabled() {
          fidelity::event(FidelityClass::Undo, "undo", || {
            format!(
              "applied={applied} frontier {:?} -> {:?}",
              from_frontier.encode(),
              self.doc.state_frontiers().encode()
            )
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
        // §act-three B.1: symmetric fast path — replays the recorded delete.
        if let Some(events) = hotpath::measure_block!(
          "recorded_inverse_redo",
          crate::local_write::recorded_inverse::try_fast_step(self, loro::UndoOrRedo::Redo)?
        ) {
          return Ok(events);
        }
        let applied = hotpath::measure_block!("loro_undo_manager_redo", self.undo.redo().context("applying Loro redo")?);
        // §fidelity: record the redo's frontier transition and assert it only
        // introduced local-peer ops (remote-origin ops are excluded from redo).
        if fidelity::enabled() {
          fidelity::event(FidelityClass::Undo, "redo", || {
            format!(
              "applied={applied} frontier {:?} -> {:?}",
              from_frontier.encode(),
              self.doc.state_frontiers().encode()
            )
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
    let drained = self.merge_subscription_invalidation(&mut projection_invalidation);
    let mut events = self.events_after_local_change(from_frontier, from_vv, projection_invalidation.clone(), false)?;
    // §6-R: semantic commands (undo/redo, object inserts) derive their
    // projection through THE ladder — the same one remote imports use. No
    // whole-body string diff, no O(doc) before/after captures.
    let context = if restore_undo_selection { "undo_redo" } else { "semantic_command" };
    self.derive_body_projection_events(projection_invalidation, &drained, context, &mut events)?;
    // §act-four M5: the semantic-command path (undo/redo, object inserts, direct
    // text edits) commits and derives here — append its root to the version log,
    // same as the intent path's `apply_local_intent` hooks.
    if restore_undo_selection && let Some(snapshot) = self.take_restored_undo_selection() {
      if let Some(selection) = self.resolve_undo_selection(&snapshot) {
        events.push(RuntimeEvent::SelectionRestored { selection });
      } else if let Ok(mut state) = self.undo_selection.lock() {
        state.restored_selection = Some(snapshot);
      }
    }
    Ok(events)
  }

  pub(crate) fn resolve_undo_selection(&mut self, snapshot: &UndoSelectionSnapshot) -> Option<EditorSelection> {
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

  /// H-K0 keystone: project the document as it stood at `frontier` (an
  /// encoded [`Frontiers`] blob). Works on a fork — the live doc, its undo
  /// stacks, and its subscriptions are untouched. Unlike
  /// [`Self::revision_projection`] this needs no named revision and no
  /// package: any frontier in the doc's causal past is viewable, which is
  /// what history preview/tape/restore and comment orphan-jump all consume.
  pub fn frontier_projection(&self, frontier: &[u8]) -> Result<DocumentProjection> {
    let frontiers = Frontiers::decode(frontier).context("decoding frontier blob for historical view")?;
    let historical_doc = self
      .doc
      .fork_at(&frontiers)
      .context("forking document at historical frontier")?;
    let mut document = document_from_loro(&historical_doc).context("projecting historical document")?;
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

  /// Spec §6-R: derive exact structural patches for a remote import by
  /// rematerializing ONLY the changed region under the same materializer law
  /// as the full rebuild. `None` = take the full-rebuild fallback (loud,
  /// counted by the caller). Returns the patches plus the region's projection
  /// defects for canonical repair scheduling.
  #[hotpath::measure]
  /// Loro-first spec §6.3 + §6-R: THE post-commit projection derivation
  /// ladder, shared by remote imports and local semantic commands (undo/redo,
  /// object inserts). Structural analysis is EXACT, from the composed net
  /// delta (pre-change → post-change): structure changed iff (a) an inserted
  /// run carries \n/U+FFFC, (b) a deleted pre-change range covers a paragraph
  /// boundary or object placeholder, or (c) a structural insert was churned
  /// away within the batch (per-op flags can't prove neutrality). Checkout
  /// events stay conservative. The ladder: projection-neutral → nothing to
  /// derive (the 19:28 churn class ends as a no-op, spec §13.13);
  /// non-structural → ranged paragraph patches; structural → regional
  /// rematerialization; any bail → full rebuild, loudly, with its cause.
  pub(crate) fn derive_body_projection_events(
    &mut self,
    mut invalidation: ProjectionInvalidation,
    drained: &DrainedBodyDelta,
    context: &'static str,
    events: &mut Vec<RuntimeEvent>,
  ) -> Result<()> {
    invalidation.has_remote_origin |= drained.has_remote_origin;
    debug_assert!(
      !drained.checkout_seen || invalidation.rebuild_required,
      "checkout drains must force a rebuild"
    );
    let net = &drained.net;
    let body_neutral = net.is_empty();
    let structural = !body_neutral
      && (net.structural_churn
        || net.inserts_structure()
        || net.deletes_any_position(self.projection_index.boundary_positions())
        || net.deletes_any_position(self.projection_index.object_positions()));
    // Diagnostic affordance (FLOWSTATE_DERIVE_DEBUG=1): one line per ladder
    // decision — which branch a change batch takes and why. Costs one cached
    // atomic load when off.
    static DERIVE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DERIVE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some()) {
      eprintln!(
        "derive[{context}]: neutral={body_neutral} structural={structural} rebuild={} fallback={:?} ranges={:?} net={:?}",
        invalidation.rebuild_required, invalidation.fallback_reason, invalidation.changed_text_ranges, net
      );
    }
    if structural {
      invalidation.rebuild_required = true;
      if invalidation.fallback_reason.is_none() {
        invalidation.fallback_reason = Some("structural_body_text_change");
      }
    } else if invalidation.fallback_reason == Some("structural_body_text_change") {
      // The union heuristics inside the drain were conservative; the exact
      // net-delta analysis supersedes them.
      invalidation.rebuild_required = false;
      invalidation.fallback_reason = None;
    }
    // §6-R: structural changes take the REGIONAL rematerializer — the same
    // materializer law as the full rebuild, applied to the changed region
    // only. Any bail falls through to the rebuild, loudly, with its cause.
    let regional = if structural {
      let result = self.remote_structural_projection_patches(&invalidation, drained);
      if let Err(bail) = &result {
        // Mirror of `full-rebuild-after-local-write`: regional bails must be
        // visible, not ambient.
        tracing::warn!(bail, context, "full-rebuild-after-structural-change");
      }
      result.ok()
    } else {
      None
    };
    // §6-R composition: the regional walk covers the structural neighborhood;
    // a mixed batch can ALSO carry changes outside it (e.g. an undo restoring
    // a split while a distant paragraph is restyled in the same exchange).
    // Out-of-region touched paragraphs take the same ranged readback the
    // non-structural path uses — same law, region scale + paragraph scale. A
    // readback bail falls to the rebuild exactly like a regional bail.
    let regional = regional.and_then(|regional| {
      let RegionalPatches {
        mut patches,
        defects,
        region_paragraphs,
      } = regional;
      let live_starts = net.shift_positions(self.projection_index.paragraph_starts());
      let touched =
        self
          .projection_index
          .paragraphs_for_changed_ranges(&invalidation.changed_text_ranges, self.projection.paragraphs.len(), &live_starts);
      let outside: Vec<usize> = touched
        .into_iter()
        .filter(|ix| !region_paragraphs.contains(ix))
        .collect();
      if !outside.is_empty() {
        match paragraph_projection_patches_ranged(&self.projection, &self.doc, &live_starts, outside) {
          Some(extra) => patches.extend(extra),
          None => {
            tracing::warn!(bail = "outside-region-readback", context, "full-rebuild-after-structural-change");
            return None;
          },
        }
      }
      // Same composition for OBJECT state: a batch can pair a structural body
      // change with e.g. a table column reorder on a table outside the region
      // (the table fuzz caught the swallowed reorder). The canonical object
      // readback covers all object rows; in-region duplicates are benign
      // (same derived values).
      if !invalidation.changed_blocks.is_empty()
        || !invalidation.changed_tables.is_empty()
        || invalidation
          .changed_flows
          .iter()
          .any(|flow| flow != ROOT_BODY_FLOW_ID)
      {
        match projection_patch::remote_object_projection_patches_scoped_or_full(&self.projection, &self.doc, &invalidation) {
          Some(extra) => patches.extend(extra),
          None => {
            tracing::warn!(bail = "outside-region-object-readback", context, "full-rebuild-after-structural-change");
            return None;
          },
        }
      }
      Some((patches, defects))
    });
    if let Some((patches, defects)) = regional {
      self.apply_projection_patch_set(&patches);
      self.projection.frontier = self.doc.state_frontiers().encode();
      let mut patched_invalidation = invalidation;
      patched_invalidation.rebuild_required = false;
      patched_invalidation.fallback_reason = None;
      events.push(self.projection_patched_event(patches.into(), patched_invalidation));
      match self.schedule_projection_repairs(defects, RepairEmission::Streamed) {
        Ok(repair_events) => events.extend(repair_events),
        Err(error) => tracing::error!(%error, context, "scheduling regional projection repairs after structural change failed"),
      }
    } else if invalidation.rebuild_required {
      let before_projection = self.projection.clone();
      self.refresh_projection()?;
      events.push(self.projection_change_event(&before_projection, invalidation)?);
    } else {
      // Non-structural: post-change paragraph starts are the pre-change
      // starts shifted through the net delta — O(P + ops) arithmetic, no
      // body scan.
      let live_starts = net.shift_positions(self.projection_index.paragraph_starts());
      let touched_paragraphs =
        self
          .projection_index
          .paragraphs_for_changed_ranges(&invalidation.changed_text_ranges, self.projection.paragraphs.len(), &live_starts);
      let nonstructural = remote_nonstructural_projection_patches(&self.projection, &self.doc, &invalidation, &touched_paragraphs, &live_starts);
      static DERIVE_DEBUG2: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
      if *DERIVE_DEBUG2.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some()) {
        eprintln!(
          "  nonstructural: touched={touched_paragraphs:?} patches={:?}",
          nonstructural.as_ref().map(|patches| patches
            .iter()
            .map(|p| format!("{p:?}").chars().take(90).collect::<String>())
            .collect::<Vec<_>>())
        );
      }
      if let Some(patches) = nonstructural {
        self.apply_projection_patch_set(&patches);
        self.projection.frontier = self.doc.state_frontiers().encode();
        events.push(self.projection_patched_event(patches.into(), invalidation));
      } else {
        let before_projection = self.projection.clone();
        self.refresh_projection()?;
        events.push(self.projection_change_event(&before_projection, invalidation)?);
      }
    }
    Ok(())
  }

  fn remote_structural_projection_patches(
    &self,
    invalidation: &ProjectionInvalidation,
    drained: &DrainedBodyDelta,
  ) -> Result<RegionalPatches, &'static str> {
    // Rebuild-only classes: time travel, unclassifiable diffs, section-map
    // changes, and non-body flow content (table cells) mixed into the chunk.
    // `metadata_record_id_change` is EXPECTED here (splits write records) and
    // is exactly what the regional re-derivation handles.
    if drained.checkout_seen
      || !invalidation.changed_sections.is_empty()
      || !invalidation.changed_tables.is_empty()
      || invalidation
        .changed_flows
        .iter()
        .any(|flow| flow != ROOT_BODY_FLOW_ID)
      || matches!(
        invalidation.fallback_reason,
        Some("checkout_trigger_projection_rebuild" | "unknown_loro_subscription_diff")
      )
    {
      return Err("rebuild-only-invalidation");
    }
    let (pre_lo, pre_hi) = drained.net.pre_change_span().ok_or("empty-net-span")?;

    // ---- Old affected rows (search spans are block-aligned, PRE space). ----
    let spans = &self.projection_index.search_unit_spans;
    if spans.len() != self.projection.blocks.len() {
      return Err("search-index-misaligned");
    }
    let mut blk_lo = None;
    let mut blk_hi = None;
    for (ix, span) in spans.iter().enumerate() {
      // A row's territory includes its leading boundary sentinel.
      let lo = span.unicode_start.saturating_sub(1);
      let hi = span.unicode_start + span.unicode_len;
      if hi >= pre_lo && lo <= pre_hi {
        blk_lo.get_or_insert(ix);
        blk_hi = Some(ix);
      }
    }
    let (blk_lo, blk_hi) = (blk_lo.ok_or("no-affected-rows")?, blk_hi.ok_or("no-affected-rows")?);

    // ---- Expand to whole rows + resolve POST-space region bounds. ----------
    // The predecessor paragraph provides the walker's object-coalescing
    // context; the successor paragraph's leading sentinel is the exclusive
    // region end. Both are unaffected rows, so their durable records resolve
    // on the live fast path.
    let is_paragraph_row = |ix: usize| matches!(self.projection.blocks.get(ix), Some(flowstate_document::Block::Paragraph(_)));
    // §act-ten A10.8: O(log N) tree rank per lookup (was an O(row) block walk,
    // called ~4x per regional derivation).
    let paragraph_ix_of_row = |row: usize| self.projection.blocks.paragraphs_before_row(row);
    let body = body_text(&self.doc);
    static REGION_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let region_debug = *REGION_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some());
    let (region_lo, region_sentinel) = match (0..blk_lo).rev().find(|ix| is_paragraph_row(*ix)) {
      Some(row) => {
        let id = self
          .projection
          .ids
          .paragraph_ids
          .get(paragraph_ix_of_row(row))
          .copied()
          .ok_or("edge-paragraph-id-missing")?;
        let start = paragraph_body_start_in_loro(&self.doc, id);
        if start.is_none() && region_debug {
          eprintln!("derive[region]: start unresolved — predecessor row {row} paragraph id {id:?} has no resolvable record");
        }
        let start = start.ok_or("region-start-unresolved")?;
        (row, start.checked_sub(1).ok_or("region-start-underflow")?)
      },
      None => (0, 0),
    };
    let (region_hi, region_end) = match ((blk_hi + 1)..self.projection.blocks.len()).find(|ix| is_paragraph_row(*ix)) {
      Some(row) => {
        let id = self
          .projection
          .ids
          .paragraph_ids
          .get(paragraph_ix_of_row(row))
          .copied()
          .ok_or("edge-paragraph-id-missing")?;
        let start = paragraph_body_start_in_loro(&self.doc, id).ok_or("region-end-unresolved")?;
        (row, start.checked_sub(1).ok_or("region-end-underflow")?)
      },
      None => (self.projection.blocks.len(), body.len_unicode()),
    };

    // ---- Identity candidates (§6-R step 2): the old projection's region rows
    // plus every registry record this import touched. Canonical keys make each
    // probe O(1); a candidate that no longer resolves is simply dead. ---------
    let root = self.doc.get_map(ROOT);
    let paragraphs_registry = child_map(&root, PARAGRAPHS_BY_ID).ok_or("paragraph-registry-missing")?;
    let blocks_registry = child_map(&root, BLOCKS_BY_ID).ok_or("block-registry-missing")?;
    let probe = |registry: &LoroMap, canonical: String, special: &str, id: u128| -> Option<(String, LoroMap)> {
      if let Some(record) = child_map(registry, &canonical) {
        return Some((canonical, record));
      }
      if loro_id_u128(special) == id
        && let Some(record) = child_map(registry, special)
      {
        return Some((special.to_string(), record));
      }
      None
    };

    let mut paragraph_candidates: Vec<(String, LoroMap)> = Vec::new();
    let mut pblock_candidates: Vec<(String, LoroMap)> = Vec::new();
    let mut object_candidates: Vec<LoroMap> = Vec::new();
    let mut paragraph_pointer = paragraph_ix_of_row(region_lo);
    for row in region_lo..region_hi {
      let block_id = self
        .projection
        .ids
        .block_ids
        .get(row)
        .copied()
        .ok_or("row-block-id-missing")?;
      match self.projection.blocks.get(row).ok_or("row-missing")? {
        flowstate_document::Block::Paragraph(_) => {
          let id = self
            .projection
            .ids
            .paragraph_ids
            .get(paragraph_pointer)
            .copied()
            .ok_or("row-paragraph-id-missing")?;
          paragraph_pointer += 1;
          // A missing record is not a bail: the boundary fabricates the same
          // stable id the repair writer mints, and defect repair converges it
          // (accepted-risk class, spec 6-R).
          if let Some(candidate) = probe(&paragraphs_registry, format!("paragraph.{}", id.0), ROOT_FIRST_PARAGRAPH_ID, id.0) {
            paragraph_candidates.push(candidate);
          }
          if let Some(candidate) = probe(
            &blocks_registry,
            format!("paragraph_block.{}", block_id.0),
            MAIN_BODY_BLOCK_ID,
            block_id.0,
          ) {
            pblock_candidates.push(candidate);
          }
        },
        block => {
          let kind = match block {
            flowstate_document::Block::Image(_) => "image",
            flowstate_document::Block::Equation(_) => "equation",
            flowstate_document::Block::Table(_) => "table",
            flowstate_document::Block::Paragraph(_) => unreachable!(),
          };
          object_candidates.push(child_map(&blocks_registry, &format!("{kind}.{}", block_id.0)).ok_or("object-record-missing")?);
        },
      }
    }
    for key in &drained.imported_paragraph_record_keys {
      if let Some(record) = child_map(&paragraphs_registry, key) {
        paragraph_candidates.push((key.clone(), record));
      }
    }
    for key in &drained.imported_block_record_keys {
      let Some(record) = child_map(&blocks_registry, key) else {
        continue;
      };
      match map_string_opt(&record, "kind").as_deref() {
        Some("paragraph") => pblock_candidates.push((key.clone(), record)),
        Some(_) => object_candidates.push(record),
        None => {},
      }
    }

    // ---- ONE batch resolution for every candidate anchor (dead anchors are
    // simply absent — never per-id history walks; the ctrl-A lesson). --------
    let container = body.id();
    let decode_cursor_id = |record: &LoroMap, field: &str| -> Option<ID> {
      let bytes = map_binary_opt(record, field)?;
      let cursor = Cursor::decode(&bytes).ok()?;
      (cursor.container == container)
        .then_some(cursor.id)
        .flatten()
    };
    let mut query_ids: Vec<ID> = Vec::new();
    let paragraph_cursor_ids: Vec<(Option<ID>, Option<ID>)> = paragraph_candidates
      .iter()
      .map(|(_, record)| (decode_cursor_id(record, "boundary_cursor"), decode_cursor_id(record, "start_cursor")))
      .collect();
    let pblock_cursor_ids: Vec<Option<ID>> = pblock_candidates
      .iter()
      .map(|(_, record)| decode_cursor_id(record, "anchor_cursor"))
      .collect();
    let object_cursor_ids: Vec<Option<ID>> = object_candidates
      .iter()
      .map(|record| decode_cursor_id(record, "anchor_cursor"))
      .collect();
    for id in paragraph_cursor_ids
      .iter()
      .flat_map(|(boundary, start)| [boundary, start])
      .chain(pblock_cursor_ids.iter())
      .chain(object_cursor_ids.iter())
      .flatten()
    {
      query_ids.push(*id);
    }
    let mut pos_by_id: FxHashMap<ID, usize> = FxHashMap::default();
    if !query_ids.is_empty() {
      for (id, pos) in query_ids.iter().copied().zip(
        self
          .doc
          .inner()
          .query_text_id_positions(&container, &query_ids),
      ) {
        if let Some(pos) = pos {
          pos_by_id.insert(id, pos);
        }
      }
    }

    // ---- Boundary→key maps, mirroring the full materializer's prefer rule
    // (lexicographically smallest key wins; special ids win boundary 0). -----
    let live_newline = |pos: usize| body.char_at(pos) == Ok('\n');
    let mut paragraph_map: FxHashMap<usize, String> = FxHashMap::default();
    let mut paragraph_resolved: Vec<(String, usize)> = paragraph_candidates
      .iter()
      .zip(&paragraph_cursor_ids)
      .filter_map(|((key, _), (boundary, start))| {
        let pos = boundary
          .and_then(|id| pos_by_id.get(&id).copied())
          .or_else(|| start.and_then(|id| pos_by_id.get(&id).copied()))?;
        live_newline(pos).then(|| (key.clone(), pos))
      })
      .collect();
    paragraph_resolved.sort();
    for (key, pos) in &paragraph_resolved {
      if *pos == 0 && key == ROOT_FIRST_PARAGRAPH_ID {
        paragraph_map.insert(0, key.clone());
      } else {
        paragraph_map.entry(*pos).or_insert_with(|| key.clone());
      }
    }
    let mut pblock_map: FxHashMap<usize, String> = FxHashMap::default();
    let mut pblock_resolved: Vec<(String, usize)> = pblock_candidates
      .iter()
      .zip(&pblock_cursor_ids)
      .filter_map(|((key, _), anchor)| {
        let pos = anchor.and_then(|id| pos_by_id.get(&id).copied())?;
        live_newline(pos).then(|| (key.clone(), pos))
      })
      .collect();
    pblock_resolved.sort();
    for (key, pos) in &pblock_resolved {
      if *pos == 0 && key == MAIN_BODY_BLOCK_ID {
        pblock_map.insert(0, key.clone());
      } else {
        pblock_map.entry(*pos).or_insert_with(|| key.clone());
      }
    }
    let mut object_map: BTreeMap<usize, LoroMap> = BTreeMap::new();
    for (record, anchor) in object_candidates.into_iter().zip(&object_cursor_ids) {
      let Some(pos) = anchor.and_then(|id| pos_by_id.get(&id).copied()) else {
        continue;
      };
      if body.char_at(pos) == Ok(flowstate_document::OBJECT_REPLACEMENT) {
        object_map.entry(pos).or_insert(record);
      }
    }

    // §task #40 (BARE heal): repair-HEALED records are ANCHOR-keyed
    // (`paragraph.anchor.op-…`, minted by `stable_boundary_metadata_keys` when
    // a record-less boundary is repaired), and the row-id probes above cannot
    // reconstruct those keys from the projection's hashed ids (one-way).
    // Re-derive each still-unresolved region boundary's anchor keys from the
    // live body — the exact derivation the repair writer used — and probe
    // them, so healed boundaries resolve regionally instead of re-reporting
    // (and re-repairing, and re-projecting) their defects on every later
    // structural import touching the region. Agreement with the full law:
    // any record the full materializer would prefer over the anchor record
    // (cursor-resolved, lexicographically smaller key) already claimed the
    // boundary in the maps above, so this only fills genuine holes.
    {
      let region_text = body
        .slice(region_sentinel, region_end)
        .map_err(|_| "region-slice-failed")?;
      for (offset, ch) in region_text.chars().enumerate() {
        if ch != '\n' {
          continue;
        }
        let boundary = region_sentinel + offset;
        let paragraph_missing = !paragraph_map.contains_key(&boundary);
        let pblock_missing = !pblock_map.contains_key(&boundary);
        if !paragraph_missing && !pblock_missing {
          continue;
        }
        let Some((paragraph_key, block_key)) = flowstate_document::loro_projection::stable_boundary_metadata_keys(&body, boundary) else {
          continue;
        };
        if paragraph_missing && child_map(&paragraphs_registry, &paragraph_key).is_some() {
          paragraph_map.insert(boundary, paragraph_key);
        }
        if pblock_missing && child_map(&blocks_registry, &block_key).is_some() {
          pblock_map.insert(boundary, block_key);
        }
      }
    }

    // ---- Rematerialize the region under the shared law + splice. -----------
    static DERIVE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let derive_debug = *DERIVE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some());
    let materialize_start = std::time::Instant::now();
    let region = flowstate_document::materialize_body_region(&self.doc, region_sentinel, region_end, &paragraph_map, &pblock_map, &object_map)
      .map_err(|_| "region-materialize-failed")?;
    if derive_debug {
      eprintln!(
        "derive[region]: span={}..{} rows={}..{} objects={} materialize={:?}",
        region_sentinel,
        region_end,
        region_lo,
        region_hi,
        object_map.len(),
        materialize_start.elapsed()
      );
    }
    let patches = splice_region_patches(&self.projection, region_lo, region_hi, &region).ok_or("splice-bailed")?;
    // The region's OLD-projection paragraph-index range, so the caller can
    // derive out-of-region changes (a mixed batch's distant mark edits)
    // through the ranged readback rather than losing them.
    let region_paragraphs = paragraph_ix_of_row(region_lo)..paragraph_ix_of_row(region_hi.min(self.projection.blocks.len()));
    Ok(RegionalPatches {
      patches,
      defects: region.defects,
      region_paragraphs,
    })
  }

  #[hotpath::measure]
  pub fn import_remote_update(&mut self, bytes: &[u8]) -> Result<Vec<RuntimeEvent>> {
    let mut replies = self.import_remote_updates(&[bytes])?;
    Ok(replies.pop().unwrap_or_default())
  }

  /// Batched sibling of [`Self::import_remote_update`] (§6.4): import EVERY
  /// coalesced blob first, then derive projection events ONCE over the
  /// composed frontier delta. The former shape ran one full derive (regional
  /// walk / readback / editor patch batch) per blob — 41.6% of a receiving
  /// peer's runtime in the 2026-07-07 field profile. Returns one event vec
  /// per input blob (each carries its own `pending` status for the session's
  /// anti-entropy trigger); the composed projection events ride on the LAST
  /// blob's vec, preserving commit-order delivery through the ordered stream.
  #[hotpath::measure]
  pub fn import_remote_updates(&mut self, chunks: &[&[u8]]) -> Result<Vec<Vec<RuntimeEvent>>> {
    if chunks.is_empty() {
      return Ok(Vec::new());
    }
    // §act-eleven C8 observability: batches served (each = one gate hold + one
    // derive over N coalesced blobs). The doc_io coalescing bound is asserted
    // against this — without it the §6.4 batching could silently degrade to
    // one-derive-per-blob (41.6% of a receiving peer's runtime in the field).
    self.import_batches_served += 1;
    let from_frontier = self.doc.state_frontiers();
    // §fidelity: capture the pre-import version only when tracing so a disabled
    // build pays nothing; used to assert the import advanced (never regressed) the
    // canonical frontier below.
    let fidelity_before_vv = fidelity::enabled().then(|| self.doc.state_vv());
    static IMPORT_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let import_probe = *IMPORT_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_IMPORT_PROBE").is_some());
    let probe_t = std::time::Instant::now();
    let mut statuses = Vec::with_capacity(chunks.len());
    let mut below_root_chunks: Vec<&[u8]> = Vec::new();
    for bytes in chunks {
      let status = hotpath::measure_block!("loro_import_with", {
        match self.doc.import_with(bytes, "remote") {
          Ok(status) => status,
          // §A12.1.3 slice 4: this shallow session cannot hold ops that
          // depend on pre-root history (Loro rolled the import back
          // cleanly). Absorb them durably into the PACKAGE below instead of
          // failing the import.
          Err(loro::LoroError::ImportUpdatesThatDependsOnOutdatedVersion) if self.doc.is_shallow() => {
            below_root_chunks.push(bytes);
            ImportStatus {
              success: VersionRange::default(),
              pending: None,
            }
          },
          Err(error) => return Err(error).context("importing remote Loro update"),
        }
      });
      statuses.push(status);
    }
    if let Some(before_vv) = &fidelity_before_vv {
      self.fidelity_frontier_transition("import", &from_frontier, before_vv);
    }
    // §6-R: unmarked paragraph boundaries introduced by a merge surface as
    // `MissingParagraphStyleMark` defects from whichever projection derivation
    // runs below (regional walk or full rebuild) and are repaired canonically
    // through `schedule_projection_repairs` — the former pre-import whole-flow
    // style scan (O(doc) `to_delta` per structural chunk) is gone.
    let frontier_after = self.doc.state_frontiers().encode();
    let version_vector = self.doc.state_vv().encode();
    // §act-ten A10.2: a remote import invalidates the recorded inverse — but
    // ONLY if it applied anything (no-op drips leave the frontier — hence the
    // doc — untouched). §oom-leads #9 refined this further: an APPLYING import
    // no longer clears eagerly either; once the drained net delta is available
    // below, `reconcile_recorded_inverse_after_remote` REBASES the stacks
    // through hull-disjoint remote changes and only clears when the import
    // actually threatens the recorded coordinates.
    let probe_loro = probe_t.elapsed();
    let probe_t = std::time::Instant::now();
    let import_applied_changes = frontier_after != from_frontier.encode();
    // §22: when an import is missing dependencies, surface the pending version
    // range so the UI session can trigger immediate update pull/anti-entropy
    // rather than waiting for the periodic digest. The range is both logged here
    // and carried on that blob's `RemoteUpdateApplied { pending }` below.
    let mut replies: Vec<Vec<RuntimeEvent>> = statuses
      .iter()
      .map(|status| {
        if let Some(missing) = Self::missing_dependency_request(status) {
          tracing::debug!(?missing, "remote Loro import is missing dependencies; requesting anti-entropy pull");
        }
        vec![RuntimeEvent::RemoteUpdateApplied {
          pending: status.pending.clone(),
          frontier: frontier_after.clone(),
          version_vector: version_vector.clone(),
        }]
      })
      .collect();
    // A blob with unresolved dependencies may have been satisfied by a LATER
    // blob in the same chunk (Loro buffers and auto-applies), but the recorded
    // per-import status cannot see that — treat any pending status as the
    // conservative full-rebuild fallback, exactly once for the whole chunk
    // (the former shape paid one full rebuild PER pending blob).
    let any_pending = statuses.iter().any(|status| status.pending.is_some());
    let frontier_before = from_frontier.encode();
    let last = replies.last_mut().expect("chunks is non-empty");
    if !any_pending {
      let mut invalidation = ProjectionInvalidation {
        frontier_before,
        frontier_after: frontier_after.clone(),
        changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
        ..ProjectionInvalidation::default()
      };
      let drained = self.merge_subscription_invalidation(&mut invalidation);
      let probe_drain = probe_t.elapsed();
      let probe_t2 = std::time::Instant::now();
      if import_applied_changes {
        self.reconcile_recorded_inverse_after_remote(&invalidation, &drained);
      }
      let probe_reconcile = probe_t2.elapsed();
      let probe_t2 = std::time::Instant::now();
      self.derive_body_projection_events(invalidation, &drained, "remote_import", last)?;
      if import_probe {
        eprintln!(
          "[flowstate-import-probe] loro={probe_loro:?} drain={probe_drain:?} reconcile={probe_reconcile:?} derive={:?}",
          probe_t2.elapsed()
        );
      }
    } else {
      if import_applied_changes {
        self.clear_recorded_inverse();
      }
      let mut invalidation = ProjectionInvalidation::full_rebuild(
        frontier_before.clone(),
        frontier_after.clone(),
        "remote_update_pending_projection_fallback",
      );
      // Drain the subscription regardless so no stale events linger.
      self.merge_subscription_invalidation(&mut invalidation);
      // §act-five P4: a fully-pending import APPLIED NOTHING — Loro buffered the
      // out-of-order updates and the canonical frontier is unchanged, so the
      // projection is byte-identical (same frontier ⇒ same Loro state). Skip the
      // O(doc) full rebuild + event; the pending version range already rode the
      // `RemoteUpdateApplied` reply above, which drives the anti-entropy pull.
      if frontier_after != frontier_before {
        self.refresh_projection()?;
        last.push(self.projection_event(invalidation)?);
      }
    }
    if !below_root_chunks.is_empty() {
      let event = self.absorb_below_root_import(&below_root_chunks)?;
      replies.last_mut().expect("chunks is non-empty").push(event);
    }
    // §act-four §8 fork-and-share: a remote import advances the canonical
    // frontier and forks a new read-model version. Because `blocks`/`paragraphs`
    // are persistent trees, the forked root SHARES every subtree the import did
    // not touch — the fork costs `O(change)`, not `O(document)`.
    if !any_pending {
      // The remote updates have already merged into the canonical Loro doc above;
      // durability (revision sync + update-segment persistence) is a SECONDARY
      // concern and MUST NOT be able to discard a successful merge. Propagating a
      // persist error here (`?`) previously made the caller drop the whole import
      // (session_io) so the peer never projected the remote edits/presence — a
      // one-directional-sync failure. Log and keep the merge in memory instead;
      // the segment persistence self-heals (re-snapshots) in `persist_update_segment`.
      // One revision sync + one segment persist covers the WHOLE chunk.
      let probe_tail = std::time::Instant::now();
      if let Some(package) = &mut self.package
        && let Err(error) = package.sync_revisions_from_loro(&self.doc)
      {
        tracing::error!(%error, "syncing revisions after remote import failed; kept the merged update in memory");
      }
      let probe_revisions = probe_tail.elapsed();
      let probe_tail = std::time::Instant::now();
      if let Err(error) = self.persist_update_from_last_frontier() {
        tracing::error!(%error, "persisting merged remote update failed; kept the merge in memory (durability degraded until the next successful save)");
        fidelity::event(FidelityClass::Persistence, "remote-persist-failed", || format!("{error:#}"));
      }
      if import_probe {
        eprintln!(
          "[flowstate-import-probe]   tail: revisions={probe_revisions:?} persist={:?}",
          probe_tail.elapsed()
        );
      }
    }
    Ok(replies)
  }

  fn projection_event(&mut self, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
    self.record_projection_fallback(&invalidation);
    let document = Box::new(self.projection_snapshot()?);
    // Ordered stream (field fix): the editor consumes this replace in commit
    // order, never through a racing side channel.
    self.push_editor_stream(gpui_flowtext::ProjectionStreamItem::Replace(document.clone()));
    Ok(RuntimeEvent::ProjectionUpdated {
      document,
      invalidation,
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    })
  }

  pub fn export_updates_for(&self, remote_vv: &VersionVector) -> Result<Vec<u8>> {
    // §A12.1.3 slice 4: a shallow live doc's updates export SILENTLY
    // truncates below its root — a far-behind puller would import nothing
    // usable and spin on its pending-retry loop forever. Detect the
    // below-root request and serve it from the reconstructed full-history
    // doc (load-serve-drop sidecar) instead.
    if self.doc.is_shallow() {
      let root = self.doc.shallow_since_vv();
      let below_root = root
        .iter()
        .any(|(peer, counter)| remote_vv.get(peer).copied().unwrap_or(0) < *counter);
      if below_root {
        let package = self
          .package
          .as_ref()
          .context("a shallow doc without its package cannot serve below-root anti-entropy")?;
        let full = package
          .reconstruct_full_doc(&self.doc)
          .context("reconstructing full history for below-root anti-entropy")?;
        return full
          .export(ExportMode::updates(remote_vv))
          .context("exporting full-history Loro updates for anti-entropy");
      }
    }
    self
      .doc
      .export(ExportMode::updates(remote_vv))
      .context("exporting Loro updates for anti-entropy")
  }

  /// §A12.1.3 slice 4 ("rebase reopen", design §5): merge remote ops that
  /// depend on pre-root history into the PACKAGE via the reconstructed
  /// full-history doc, persist, and tell the session a reopen is required.
  /// The in-memory shallow doc is left untouched — it cannot hold these ops.
  fn absorb_below_root_import(&mut self, chunks: &[&[u8]]) -> Result<RuntimeEvent> {
    let package = self
      .package
      .as_mut()
      .context("a shallow doc without its package cannot absorb below-root history")?;
    let full = package
      .reconstruct_full_doc(&self.doc)
      .context("reconstructing full history to absorb a below-root import")?;
    for bytes in chunks {
      full
        .import_with(bytes, "remote")
        .context("importing below-root remote update into the full-history doc")?;
    }
    package
      .compact_to_snapshot(&full)
      .map_err(|error| anyhow::anyhow!("persisting the below-root merge: {error}"))?;
    if let Some(path) = self.package_path.clone() {
      package
        .write(&path)
        .map_err(|error| anyhow::anyhow!("writing the below-root merge to disk: {error}"))?;
      self.package_journal_prepared = true;
    }
    let merged_frontier = full.state_frontiers().encode();
    tracing::warn!("below-root remote history merged into the package; the session must reopen the document to see it");
    fidelity::event(FidelityClass::Persistence, "below-root-absorb", || {
      format!("chunks={} merged_frontier_len={}", chunks.len(), merged_frontier.len())
    });
    Ok(RuntimeEvent::HistoryRebaseRequired { merged_frontier })
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

  pub(crate) fn projection_change_event(&mut self, before: &DocumentProjection, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
    if let Some(patches) = projection_patches_between(before, &self.projection) {
      self.record_projection_fallback(&invalidation);
      let batch = ProjectionPatchBatch {
        transaction_id: uuid::Uuid::new_v4().as_u128(),
        base_frontier: before.frontier.clone(),
        new_frontier: self.doc.state_frontiers().encode(),
        patches: patches.into(),
      };
      self.push_editor_stream(gpui_flowtext::ProjectionStreamItem::Patches(batch.clone()));
      return Ok(RuntimeEvent::ProjectionPatched {
        batch,
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

  pub(crate) fn projection_patched_event(
    &mut self,
    patches: std::sync::Arc<[flowstate_document::ProjectionPatch]>,
    invalidation: ProjectionInvalidation,
  ) -> RuntimeEvent {
    // §fidelity: the single choke point for every incrementally-patched projection
    // emission (local semantic commands + remote non-structural imports). Verify
    // the maintained projection still matches a fresh full rebuild.
    self.fidelity_verify_incremental_projection("projection-patched");
    let batch = ProjectionPatchBatch {
      transaction_id: uuid::Uuid::new_v4().as_u128(),
      base_frontier: invalidation.frontier_before.clone(),
      new_frontier: self.doc.state_frontiers().encode(),
      patches,
    };
    // Ordered stream (field fix): the editor consumes this batch in commit
    // order, never through a racing side channel.
    self.push_editor_stream(gpui_flowtext::ProjectionStreamItem::Patches(batch.clone()));
    RuntimeEvent::ProjectionPatched {
      batch,
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
      matches!(
        before_vv.partial_cmp(&after_vv),
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
      ),
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
    fidelity::check(foreign.is_empty(), FidelityClass::Undo, "remote-origin-op-in-undo", || {
      format!("{op} advanced non-local peers {foreign:?} (local_peer={local_peer}); remote-origin ops must be excluded from undo")
    });
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
  /// `emission` decides whether the repaired projection is ALSO pushed onto
  /// the ordered editor stream: `Streamed` when the repair pass runs after a
  /// patched emission (the editor is at the pre-repair frontier and must be
  /// advanced), `Silent` when an enclosing full-rebuild emission will carry
  /// the repaired state itself (a stream item here would be out of order).
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
  #[hotpath::measure]
  pub fn schedule_projection_repairs(&mut self, defects: Vec<ProjectionDefect>, emission: RepairEmission) -> Result<Vec<RuntimeEvent>> {
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
    let streamed = matches!(emission, RepairEmission::Streamed);
    self.repairing_projection_defects = true;
    let mut applied = 0_usize;
    for defect in &actionable {
      match projection_repair::apply_projection_repair(&self.doc, defect) {
        Ok(true) => applied += 1,
        Ok(false) => {},
        Err(error) => {
          tracing::error!(%error, stable_key = %defect.stable_key(), class = defect.class(), "applying projection defect repair failed");
        },
      }
    }
    if applied == 0 {
      self.repairing_projection_defects = false;
      return Ok(Vec::new());
    }

    // §registry hygiene (field, 2026-07-07 act two): a MASS repair pass — an
    // undo restoring a whole-document deletion fabricates a fresh
    // anchor-keyed record per boundary — leaves the previous generation of
    // records with permanently dead cursors in the registry. They project as
    // nothing but every registry scan (`boundary_cursor_positions`, the
    // fallback key scans) pays for them FOREVER: repeated mass undos
    // compounded the registry to hundreds of thousands of records and pushed
    // scan P95 to seconds. Fold the existing convergent stale/duplicate prune
    // into the same repair-origin commit whenever a pass writes many records.
    const REPAIR_PRUNE_THRESHOLD: usize = 32;
    if applied >= REPAIR_PRUNE_THRESHOLD {
      let body = body_text(&self.doc);
      match prune_stale_paragraph_metadata(&self.doc, &body) {
        Ok(pruned) if pruned.changed() => tracing::info!(
          stale_paragraphs = pruned.stale_paragraphs,
          duplicate_paragraphs = pruned.duplicate_paragraphs,
          stale_blocks = pruned.stale_blocks,
          duplicate_blocks = pruned.duplicate_blocks,
          "pruned stale metadata records alongside a mass repair pass"
        ),
        Ok(_) => {},
        Err(error) => tracing::error!(%error, "pruning stale metadata during repair pass failed"),
      }
    }

    // Commit the whole repair batch atomically under the dedicated origin so it is
    // excluded from undo and identifiable by peers.
    self.doc.set_next_commit_origin(REPAIR_ORIGIN);
    self.doc.commit();

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
      fidelity::check(remaining <= scheduled, FidelityClass::Structure, "repair-not-converging", || {
        format!("repair pass scheduled {scheduled} defect(s) but {remaining} remain after re-projection (cap={PROJECTION_REPAIR_ATTEMPT_CAP})")
      });
    }

    // Encode the pre-repair frontier before `from_frontier` is consumed by
    // `persist_update_segment` below.
    let repair_frontier_before = from_frontier.encode();
    let mut events = Vec::new();
    match self.local_update_bytes(&from_vv) {
      Ok(update) if !update.is_empty() => {
        if let Err(error) = self.persist_update_segment_lazy(from_frontier, from_vv, update.clone()) {
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
    // §task #40 (BARE heal): `Missing{ParagraphMetadata,ParagraphBlock,
    // ParagraphStyleMark}` repairs are PROJECTION-NEUTRAL BY CONSTRUCTION —
    // the repaired record carries exactly the id the projection already
    // fabricated (the shared `stable_boundary_metadata_keys` +
    // `loro_id_u128` law) and the repaired style mark writes the same
    // `Normal` the projector already defaulted to, so re-deriving the
    // projection reproduces it bit-identically. Skip the whole-doc refresh
    // for such batches: retire the repair commit's own subscription summary
    // via the same epoch bump a refresh performs, advance the maintained
    // projection's frontier, and stream a frontier-advancing (empty-diff)
    // batch so the editor never falls behind the authority. Every
    // content-touching class (anchor/placeholder deletions, table topology,
    // asset ids) and every mass pass (prune threshold) keeps the flat
    // rebuild below.
    // Boundary-0 Missing* repairs are the ONE non-neutral case in the family:
    // the shared key law special-cases boundary 0 to `paragraph.initial`, so
    // when a remote sentinel lands at the doc head the repair REASSIGNS the
    // existing record there — displacing whichever boundary it anchored
    // before (healed by the follow-up pass below). Anchor-keyed repairs can
    // never displace (the key derives from the boundary's own char op).
    let neutral_batch = streamed
      && applied < REPAIR_PRUNE_THRESHOLD
      && actionable.iter().all(|defect| match defect {
        ProjectionDefect::MissingParagraphMetadata { boundary_unicode, .. }
        | ProjectionDefect::MissingParagraphBlock { boundary_unicode, .. } => *boundary_unicode != Some(0),
        ProjectionDefect::MissingParagraphStyleMark { .. } => true,
        _ => false,
      });
    if neutral_batch {
      self.bump_runtime_epoch();
      self.repairing_projection_defects = false;
      self.projection.frontier = self.doc.state_frontiers().encode();
      let invalidation = ProjectionInvalidation {
        frontier_before: repair_frontier_before,
        frontier_after: self.projection.frontier.clone(),
        ..Default::default()
      };
      // An EMPTY patch batch: content unchanged, base→new frontier advanced.
      // (`projection_patched_event`'s fidelity hook re-verifies maintained ==
      // full rebuild when tracing is on — the neutrality claim's own net.)
      let empty: std::sync::Arc<[ProjectionPatch]> = std::sync::Arc::from(Vec::new());
      events.push(self.projection_patched_event(empty, invalidation));
      // §act-twelve: the repair commit advanced the frontier AFTER the import
      // reconcile re-armed the recorded fast-path tops — re-arm again or the
      // next undo clears a perfectly valid stack (the bench tail undo fell to
      // the slow path through every BARE heal). Repairs are position-neutral
      // to recorded coordinates (registry records + boundary marks); the
      // excluded-origin compose is covered by `survives_pending_remote` +
      // vendor #22's buffered-diff transform.
      crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
      return Ok(events);
    }
    // Re-project wholesale onto the repaired canonical state. (Measured: a
    // ladder-based derivation here LOST to the flat rebuild on large repair
    // batches — a mass-undo repair pass went 26s → 35s — and repair passes on
    // small batches are rare enough that the ~300ms rebuild is acceptable.)
    let before_projection = streamed.then(|| self.projection.clone());
    let refresh_result = self.refresh_projection();
    self.repairing_projection_defects = false;
    let follow_up_defects = refresh_result?;
    let invalidation =
      ProjectionInvalidation::full_rebuild(repair_frontier_before, self.doc.state_frontiers().encode(), "projection_defect_repair");
    match before_projection {
      // Ordered stream: after a PATCHED emission the repair commit advanced
      // the canonical frontier past what the editor received, so the editor
      // MUST get the repaired projection through the stream — constructing
      // the event without streaming it leaves the editor one repair-commit
      // behind the authority (frontier mismatch class).
      Some(before_projection) => events.push(self.projection_change_event(&before_projection, invalidation)?),
      // Silent: the caller (a full rebuild in flight, or runtime construction
      // before any editor attaches) emits AFTER this repair pass with the
      // final frontier, so a stream item here would land out of causal order
      // (StaleFrontier class). The repair is still committed + persisted.
      None => events.push(RuntimeEvent::ProjectionUpdated {
        document: Box::new(self.projection.clone()),
        invalidation,
        frontier: self.projection.frontier.clone(),
        version_vector: self.doc.state_vv().encode(),
      }),
    }
    // §task #40: a repair can DISPLACE a record (the boundary-0
    // `paragraph.initial` reassignment orphans the boundary it anchored
    // before), and the refresh above is where the displaced boundary's defect
    // surfaces. The old re-entrancy guard silently DROPPED it — the boundary
    // stayed un-healed until an arbitrarily-later import, and every peer that
    // imported the stranded state then paid its OWN heal commit (unsynced
    // repair ops that stall dripped deltas behind Loro's pending buffer until
    // anti-entropy). Follow up in the same call instead; termination is
    // guaranteed by the per-key attempt cap (a persistent defect quarantines
    // rather than looping).
    if !follow_up_defects.is_empty() {
      match self.schedule_projection_repairs(follow_up_defects, emission) {
        Ok(more) => events.extend(more),
        Err(error) => tracing::error!(%error, "follow-up projection repair pass failed"),
      }
    }
    // Same re-arm as the neutral-skip exit (see above): the repair commits
    // advanced the frontier past the import reconcile's re-arm.
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    Ok(events)
  }

  /// Returns the defects the rebuild surfaced WHEN a repair pass is in flight
  /// (the re-entrancy guard suppresses scheduling them here; the repair pass
  /// follows them up itself — §task #40); otherwise schedules them internally
  /// and returns empty.
  #[hotpath::measure]
  pub(crate) fn refresh_projection(&mut self) -> Result<Vec<ProjectionDefect>> {
    let current_assets = self.projection.assets.clone();
    // §P2a: a full rebuild is where malformed canonical state surfaces; collect
    // the projection defects so we can schedule their canonical repair.
    let (mut projection, defects) = document_from_loro_with_defects(&self.doc).context("refreshing projection from canonical Loro state")?;
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
    // pass — those defects are RETURNED for the repair pass's follow-up
    // instead (§task #40). Repairs committed here are persisted (durable +
    // anti-entropy), so peers converge even though this low-level helper
    // cannot surface the repair's `LocalUpdate` event to the caller.
    if self.repairing_projection_defects {
      return Ok(defects);
    }
    if !defects.is_empty()
      && let Err(error) = self.schedule_projection_repairs(defects, RepairEmission::Silent)
    {
      tracing::error!(%error, "scheduling projection repairs after projection refresh failed");
    }
    Ok(Vec::new())
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

  #[hotpath::measure]
  pub(crate) fn apply_projection_patch_set(&mut self, patches: &[ProjectionPatch]) {
    let (rebuild_index, table_refresh) = self
      .projection_index
      .update_for_patches(&self.projection, patches);
    let batch = ProjectionPatchBatch {
      transaction_id: uuid::Uuid::new_v4().as_u128(),
      base_frontier: self.projection.frontier.clone(),
      new_frontier: self.doc.state_frontiers().encode(),
      patches: std::sync::Arc::from(patches),
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
    } else {
      self
        .projection_index
        .refresh_object_entries(&self.projection, &table_refresh);
      // §act-ten A10.3 oracle (debug builds only): the incrementally-updated
      // index must equal a fresh full rebuild — the fuzz suites run this on
      // every spliced Enter/join/delete shape.
      #[cfg(debug_assertions)]
      self
        .projection_index
        .debug_assert_matches_fresh(&self.projection);
    }
  }

  pub fn save_package_to(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
    self.package_path = Some(path.as_ref().to_path_buf());
    self.package_journal_prepared = false;
    self.save_package()
  }

  /// Field fix 2026-07-07: phase 1 of a split checkpoint — everything that
  /// must see the LIVE doc (metadata + revision commit, update export/persist)
  /// plus a fork and the package CHECKOUT, all under one short gate hold. The
  /// heavy assembly then runs off-gate/off-loop via [`CheckpointJob::run`],
  /// and [`Self::finish_checkpoint`] restores the package. Returns `None` when
  /// no package exists yet (first save) — callers fall back to the in-place
  /// path, which is rare and user-initiated.
  ///
  /// While the package is checked out, import-side revision syncs and segment
  /// persistence skip + self-heal on the next pass (their existing behavior
  /// for package-less runtimes).
  pub fn begin_checkpoint(
    &mut self,
    title: &str,
    path: Option<PathBuf>,
    stamp: &flowstate_document::RevisionStamp,
  ) -> io::Result<Option<(CheckpointJob, Vec<RuntimeEvent>)>> {
    if self.package.is_none() {
      return Ok(None);
    }
    let revision_id = Uuid::new_v4().as_u128();
    let revision_frontiers = self.doc.state_frontiers();
    let revision_frontier = revision_frontiers.encode();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    // §A13.4.1: was the maintained projection current BEFORE this function's
    // own commits? The commits below (metadata touch + revision record) are
    // meta-only — they never touch body flows — so a projection current here
    // is still current at the post-commit frontier (restamped at job build).
    let projection_was_current = self.projection.frontier == from_frontier.encode();
    static BEGIN_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let begin_probe = *BEGIN_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_CHECKPOINT_PROBE").is_some());
    let probe_t = std::time::Instant::now();
    flowstate_document::touch_document_metadata(&self.doc).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    flowstate_document::record_revision(
      &self.doc,
      revision_id,
      revision_frontier,
      stamp.display_title(),
      stamp.kind,
      self.author_user_id,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    // H-S1: autosave grain gets thinned as it ages (named/session exempt);
    // thinning BEFORE the update export below lets it ride this segment.
    if stamp.kind == flowstate_document::RevisionKind::Auto
      && let Err(error) = self.thin_auto_revisions()
    {
      tracing::warn!(error = %format_args!("{error:#}"), "thinning auto revisions failed");
    }
    // The metadata/revision commit is undo-inert ("meta" origin): keep the
    // recorded fast-undo timelines alive across it (§act-ten A10.2).
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    let mut revision_invalidation = ProjectionInvalidation::default();
    self.merge_subscription_invalidation(&mut revision_invalidation);
    let probe_commits = probe_t.elapsed();
    let probe_t = std::time::Instant::now();
    let update = self
      .local_update_bytes(&from_vv)
      .map_err(|error| io::Error::other(error.to_string()))?;
    let probe_export = probe_t.elapsed();
    let probe_t = std::time::Instant::now();
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
    let probe_persist = probe_t.elapsed();
    if let Some(path) = path {
      self.package_path = Some(path);
      self.package_journal_prepared = false;
    }
    let package = self.package.take().expect("checked above");
    let mut projection = self.projection.clone();
    if projection_was_current {
      // Meta-only commits above: the projection content is unchanged, so it
      // is current at the post-commit frontier too (§A13.4.1).
      projection.frontier = self.doc.state_frontiers().encode();
    }
    if begin_probe {
      eprintln!(
        "[flowstate-begin-probe] commits={probe_commits:?} export={probe_export:?} persist={probe_persist:?} fork=skipped(package-derived)"
      );
    }
    let job = CheckpointJob {
      // §A13.4.4: no gate-held fork — the job reconstructs from the package.
      fork: None,
      projection,
      package,
      title: title.to_string(),
      revision_stamp: stamp.clone(),
      revision_id,
      revision_frontiers,
      author_user_id: self.author_user_id,
      peer_id: self.doc.peer_id(),
      write_path: self.package_path.clone(),
      record_revision: true,
    };
    Ok(Some((job, events)))
  }

  /// H-S4: mint a named pin at the CURRENT frontier without touching disk —
  /// the restore safety checkpoint. Thinning-exempt by kind.
  pub fn mint_named_pin_now(&mut self, title: &str) -> Result<u128> {
    let revision_id = Uuid::new_v4().as_u128();
    let frontiers = self.doc.state_frontiers();
    match &mut self.package {
      Some(package) => {
        // Writes the doc-side record, persists it as a journaled segment, and
        // adds the manifest entry + load accelerator in one motion.
        package.create_named_revision_at_with_id(
          &self.doc,
          revision_id,
          &frontiers,
          title,
          "",
          flowstate_document::RevisionKind::Named,
          self.author_user_id,
          Some(self.doc.peer_id() as u128),
        )?;
      },
      None => {
        // Package-less (never-saved) documents still get the doc-side record;
        // it syncs into the package at first save.
        flowstate_document::record_revision(
          &self.doc,
          revision_id,
          frontiers.encode(),
          title,
          flowstate_document::RevisionKind::Named,
          self.author_user_id,
        )
        .map_err(|error| anyhow::anyhow!(error))?;
      },
    }
    // Meta-origin commits: keep the recorded fast-undo timelines alive.
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    Ok(revision_id)
  }

  /// H-S4 restore-in-place, as a FORWARD operation. LAW: every restore is
  /// preceded by a safety checkpoint of the present (a named pin), so a
  /// restore can never lose the now. `revert_to` generates ordinary local
  /// inverse ops — undoable, and collab peers see a normal edit (convergent).
  pub fn restore_frontier(&mut self, frontier: &[u8]) -> Result<Vec<RuntimeEvent>> {
    let target = Frontiers::decode(frontier).context("decoding frontier for restore")?;
    let current = self.doc.state_frontiers();
    if current == target {
      return Ok(Vec::new());
    }
    self.mint_named_pin_now("Before restore")?;
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    flowstate_document::touch_document_metadata(&self.doc).context("updating document metadata for restore")?;
    // A whole-doc mutation invalidates the recorded inverse eagerly (the
    // frontier check would catch it; this frees the capture).
    self.clear_recorded_inverse();
    self
      .doc
      .revert_to(&target)
      .map_err(|error| anyhow::anyhow!(error.to_string()))
      .context("reverting document to historical frontier")?;
    let mut projection_invalidation = ProjectionInvalidation {
      frontier_before: from_frontier.encode(),
      frontier_after: self.doc.state_frontiers().encode(),
      changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
      // A restore can move anything (styles, objects, tables, comments):
      // force the full rebuild instead of the regional ladder.
      rebuild_required: true,
      ..ProjectionInvalidation::default()
    };
    let drained = self.merge_subscription_invalidation(&mut projection_invalidation);
    let mut events = self.events_after_local_change(from_frontier, from_vv, projection_invalidation.clone(), false)?;
    self.derive_body_projection_events(projection_invalidation, &drained, "restore", &mut events)?;
    Ok(events)
  }

  /// H-S1 thinning: prune AUTO-grain revision records as they age. Retention:
  /// every auto < 1h old; one per hour to 24h; one per day beyond. Named pins
  /// and session saves are exempt. Prunes the doc-side records (convergent —
  /// peers thinning concurrently delete the same elements) and the manifest
  /// entries; snapshot chunks stay (compaction owns their lifecycle).
  pub fn thin_auto_revisions(&mut self) -> Result<usize> {
    let Some(package) = &self.package else { return Ok(0) };
    let now = unix_time_secs();
    let mut claimed_hours: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut claimed_days: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut autos: Vec<(u128, i64)> = package
      .revisions
      .iter()
      .filter(|revision| revision.kind == flowstate_document::RevisionKind::Auto)
      .map(|revision| (revision.revision_id, revision.created_at_unix_secs))
      .collect();
    autos.sort_by_key(|(_, at)| *at);
    // Newest survivor per bucket: walk newest-first; the first record to claim
    // an hour/day bucket survives, every older record in it is doomed.
    let mut doomed: std::collections::HashSet<u128> = std::collections::HashSet::new();
    for (revision_id, created_at) in autos.iter().rev() {
      let age = now - created_at;
      if age < 3_600 {
        continue;
      }
      let already_claimed = if age < 86_400 {
        !claimed_hours.insert(created_at / 3_600)
      } else {
        !claimed_days.insert(created_at / 86_400)
      };
      if already_claimed {
        doomed.insert(*revision_id);
      }
    }
    if doomed.is_empty() {
      return Ok(0);
    }
    flowstate_document::loro_schema::remove_revision_records(&self.doc, &doomed).context("removing thinned revision records")?;
    let package = self.package.as_mut().expect("checked above");
    let removed = package.remove_revisions(&doomed);
    tracing::debug!(removed, "thinned auto-grain revisions");
    Ok(removed)
  }

  /// H-S1: rename a revision record — naming a moment PINS it (kind becomes
  /// `named`, exempting it from thinning). Persists the doc-side rename as a
  /// journaled segment so it survives without waiting for the next save.
  pub fn rename_revision(&mut self, revision_id: u128, title: &str) -> Result<()> {
    let title = title.trim();
    anyhow::ensure!(!title.is_empty(), "A checkpoint name cannot be empty");
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let renamed =
      flowstate_document::loro_schema::rename_revision_record(&self.doc, revision_id, title).context("renaming revision record")?;
    anyhow::ensure!(renamed, "That checkpoint no longer exists");
    if let Some(package) = &mut self.package {
      package.rename_revision(revision_id, title);
    }
    let update = self.local_update_bytes(&from_vv)?;
    if !update.is_empty() {
      self.persist_update_segment(from_frontier, from_vv, update)?;
    }
    // Meta-origin commit: keep the recorded fast-undo timelines alive.
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    Ok(())
  }

  /// §P3 (act two): a revision-LESS split checkpoint for document close/idle.
  /// Rebuilds the projection + search caches and compacts the package
  /// off-gate so the NEXT preview/open of this file takes the cache fast path
  /// instead of a multi-second full Loro materialization
  /// (`append_update_segment` nulls the cache on every edit, so an edited
  /// document otherwise never previews fast again until an explicit save).
  /// Returns `None` when there is no package or the cache is already
  /// frontier-current.
  pub fn begin_cache_flush(&mut self) -> Option<CheckpointJob> {
    let package = self.package.as_ref()?;
    let latest = self.doc.state_frontiers().encode();
    if package.manifest.projection_cache_frontier.as_deref() == Some(latest.as_slice()) && package.manifest.latest_frontier == latest {
      return None;
    }
    let package = self.package.take().expect("checked above");
    Some(CheckpointJob {
      // The cache flush persists no trailing segment, so the package may lag
      // the doc — a gate-held fork stays correct here (close/idle path).
      fork: Some(self.doc.fork()),
      projection: self.projection.clone(),
      package,
      title: String::new(),
      revision_stamp: flowstate_document::RevisionStamp::default(),
      revision_id: 0,
      revision_frontiers: self.doc.state_frontiers(),
      author_user_id: self.author_user_id,
      peer_id: self.doc.peer_id(),
      write_path: self.package_path.clone(),
      record_revision: false,
    })
  }

  /// Phase 3 of a split checkpoint: restore the (now compacted + saved)
  /// package under the gate.
  pub fn finish_checkpoint(&mut self, package: DocumentPackage, wrote_to_disk: bool) {
    self.package = Some(package);
    if wrote_to_disk {
      self.package_journal_prepared = true;
      self.last_persisted_frontier = self.doc.state_frontiers();
      self.last_persisted_vv = self.doc.state_vv();
    }
  }

  pub fn checkpoint_package(
    &mut self,
    title: &str,
    path: Option<PathBuf>,
    stamp: &flowstate_document::RevisionStamp,
  ) -> io::Result<Vec<RuntimeEvent>> {
    let revision_id = Uuid::new_v4().as_u128();
    let revision_frontiers = self.doc.state_frontiers();
    let revision_frontier = revision_frontiers.encode();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    flowstate_document::touch_document_metadata(&self.doc).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    flowstate_document::record_revision(
      &self.doc,
      revision_id,
      revision_frontier,
      stamp.display_title(),
      stamp.kind,
      self.author_user_id,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stamp.kind == flowstate_document::RevisionKind::Auto
      && let Err(error) = self.thin_auto_revisions()
    {
      tracing::warn!(error = %format_args!("{error:#}"), "thinning auto revisions failed");
    }
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
      let mut created = DocumentPackage::from_loro_snapshot_with_assets(&self.doc, title, assets_from_document(&self.projection))?;
      // §act-twelve A12.1.1: the runtime maintains paragraph-style marks, so
      // a package IT creates is marks-clean at its own frontier — later opens
      // skip the whole-corpus verification scan (the raw
      // `from_loro_snapshot*` constructors deliberately do not stamp).
      created.manifest.style_marks_clean_frontier = Some(created.manifest.latest_frontier.clone());
      self.package = Some(created);
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
    // §act-ten A10.5: ONE materialization feeds both the projection cache and
    // the search units (the separate rebuild calls each ran a full projection
    // walk — the checkpoint path got this fused in §act-five P9, this in-place
    // save path kept paying double).
    package.rebuild_caches_from_loro(&self.doc)?;
    package.compact_to_snapshot_any(&self.doc)?;
    package.create_named_revision_at_with_id(
      &self.doc,
      revision_id,
      &revision_frontiers,
      stamp.display_title(),
      "",
      stamp.kind,
      self.author_user_id,
      Some(self.doc.peer_id() as u128),
    )?;
    if let Some(path) = path {
      self.package_path = Some(path);
      self.package_journal_prepared = false;
    }
    self.save_package()?;
    // ALL the metadata/revision commits above ("meta" origin — the explicit
    // record_revision plus the ones package creation and the named-revision
    // write perform) are undo-inert: re-arm the recorded fast-undo timelines
    // to the final frontier so the next Ctrl-Z stays fast (§act-ten A10.2,
    // vendor patch #14). Must run LAST — any later meta commit would re-break
    // the expected-frontier equality.
    crate::local_write::recorded_inverse::rearm_recorded_inverse_frontier(self);
    Ok(events)
  }

  /// Field fix 2026-07-07 (I-9a completion): capture everything a package
  /// assembly needs under a SHORT gate hold — a doc fork plus a shallow
  /// projection clone. Assembly then runs off-gate and off the I/O loop; the
  /// 18-second `package-bytes` gate holds in the field logs (typing frozen,
  /// imports starved, peer pulls timing out) were the whole-assembly-under-
  /// gate shape.
  /// Read access to the runtime's package (diagnostics + gate tests).
  pub fn package(&self) -> Option<&DocumentPackage> {
    self.package.as_ref()
  }

  pub fn package_export_context(&self) -> (LoroDoc, DocumentProjection) {
    (self.doc.fork(), self.projection.clone())
  }

  /// Legacy in-place variant retained for gate-held callers that already own
  /// the core exclusively (tests, seeding). Production package exports go
  /// through `package_export_context` + `assemble_package_bytes`.
  pub fn package_bytes(&mut self, title: &str) -> io::Result<Vec<u8>> {
    let (fork, projection) = self.package_export_context();
    assemble_package_bytes(&fork, &projection, title)
  }

  #[hotpath::measure]
  pub(crate) fn events_after_local_change(
    &mut self,
    from_frontier: Frontiers,
    from_vv: VersionVector,
    invalidation: ProjectionInvalidation,
    emit_projection: bool,
  ) -> Result<Vec<RuntimeEvent>> {
    let update = self.local_update_bytes(&from_vv)?;
    let mut events = Vec::new();
    if !update.is_empty() {
      self.persist_update_segment(from_frontier, from_vv.clone(), update.clone())?;
      let frontier = self.doc.state_frontiers().encode();
      let version_vector = self.doc.state_vv().encode();
      for bytes in self.chunk_local_update(&from_vv, update) {
        events.push(RuntimeEvent::LocalUpdate {
          bytes,
          frontier: frontier.clone(),
          version_vector: version_vector.clone(),
        });
      }
    }
    if emit_projection {
      events.push(self.projection_event(invalidation)?);
    }
    Ok(events)
  }

  /// §A14.1.1: split a MASS local update into bounded sub-updates so a
  /// receiver imports them as SEPARATE gate holds — the A13.2 priority lane
  /// admits its user's keystrokes between slices instead of stalling ~240ms
  /// behind one mass hold (the alpha complaint). Each slice is a
  /// counter-window of THIS peer's new ops (`updates_in_range`), so slices
  /// arriving in emission order are causally self-contained. Updates that
  /// carry OTHER peers' ops (re-broadcast merges — interleavable causality)
  /// and small updates pass through unsplit. `FLOWSTATE_DISABLE_UPDATE_
  /// CHUNKING` reverts.
  fn chunk_local_update(&self, from_vv: &VersionVector, update: Vec<u8>) -> Vec<Vec<u8>> {
    /// Op atoms per slice: a restyle is ~2 atoms/paragraph, a keystroke 1 —
    /// 2048 keeps a slice's apply+derive in the low single-digit ms.
    const CHUNK_ATOMS: i32 = 4096;
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_UPDATE_CHUNKING").is_some()) {
      return vec![update];
    }
    let now_vv = self.doc.oplog_vv();
    let peer = self.doc.peer_id();
    let start = from_vv.get(&peer).copied().unwrap_or(0);
    let end = now_vv.get(&peer).copied().unwrap_or(start);
    let only_our_ops = now_vv
      .iter()
      .all(|(other, counter)| *other == peer || from_vv.get(other).copied().unwrap_or(0) >= *counter);
    if !only_our_ops || end.saturating_sub(start) <= CHUNK_ATOMS {
      return vec![update];
    }
    let mut chunks = Vec::new();
    let mut window_start = start;
    while window_start < end {
      let window_end = (window_start + CHUNK_ATOMS).min(end);
      match self
        .doc
        .export(ExportMode::updates_in_range(&[loro::IdSpan::new(peer, window_start, window_end)]))
      {
        Ok(bytes) if !bytes.is_empty() => chunks.push(bytes),
        Ok(_) => {},
        Err(error) => {
          // Fail SAFE: one slice failing means publish the original whole
          // update instead of a gappy sequence.
          tracing::warn!(%error, "chunking a mass local update failed; publishing unsplit");
          return vec![update];
        },
      }
      window_start = window_end;
    }
    if chunks.is_empty() { vec![update] } else { chunks }
  }

  #[hotpath::measure]
  pub(crate) fn local_update_bytes(&self, from_vv: &VersionVector) -> Result<Vec<u8>> {
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
  #[hotpath::measure]
  pub(crate) fn merge_subscription_invalidation(&self, invalidation: &mut ProjectionInvalidation) -> DrainedBodyDelta {
    let summaries = self
      .subscription_events
      .lock()
      .map(|mut events| std::mem::take(&mut *events))
      .unwrap_or_default();
    let body_target = body_text(&self.doc).id().to_string();
    let current_epoch = self.runtime_epoch.load(AtomicOrdering::SeqCst);
    // Loro-first spec §6.3: harvest per-summary body deltas (copied-out data,
    // I-9b) so the import path can compose them into ONE exact pre→post
    // delta instead of relying on the lossy union heuristics below.
    let mut body_batches: Vec<Vec<import_delta::NetOp>> = Vec::new();
    let mut checkout_seen = false;
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
    let mut imported_paragraph_record_keys: Vec<String> = Vec::new();
    let mut imported_block_record_keys: Vec<String> = Vec::new();
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
        static DERIVE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DERIVE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some()) {
          eprintln!(
            "  drain: DISCARDED stale-epoch summary (epoch {} != {current_epoch}, origin {}, trigger {}, changes {})",
            summary.epoch,
            summary.origin,
            summary.triggered_by,
            summary.changes.len()
          );
        }
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
        static DERIVE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DERIVE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some()) {
          eprintln!(
            "  drain: DEFERRED ahead-of-frontier summary (origin {}, trigger {}, changes {})",
            summary.origin,
            summary.triggered_by,
            summary.changes.len()
          );
        }
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
        checkout_seen = true;
        invalidation.rebuild_required = true;
        invalidation.fallback_reason = Some("checkout_trigger_projection_rebuild");
        tracing::debug!(origin = %summary.origin, "Flowstate forcing projection rebuild for checkout-triggered Loro event");
      }
      static DERIVE_DEBUG_PROCESS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
      if *DERIVE_DEBUG_PROCESS.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some()) {
        eprintln!(
          "  drain: PROCESSING summary (origin {}, trigger {}, changes {:?})",
          summary.origin,
          summary.triggered_by,
          summary
            .changes
            .iter()
            .map(|change| format!("{change:?}"))
            .collect::<Vec<_>>()
        );
      }
      let mut batch_ops: Vec<import_delta::NetOp> = Vec::new();
      let mut implied_cursor = 0usize;
      for change in &summary.changes {
        if let SubscriptionChange::Text {
          target,
          kind,
          unicode_start,
          unicode_len,
          deleted_len,
          inserted_structure,
        } = change
          && *target == body_target
        {
          if *unicode_start > implied_cursor {
            batch_ops.push(import_delta::NetOp::Retain(unicode_start - implied_cursor));
            implied_cursor = *unicode_start;
          }
          match kind {
            TextChangeKind::StyledRetain => {
              batch_ops.push(import_delta::NetOp::Retain(*unicode_len));
              implied_cursor += unicode_len;
            },
            TextChangeKind::Insert => {
              batch_ops.push(import_delta::NetOp::Insert {
                len: *unicode_len,
                structure: *inserted_structure,
              });
              implied_cursor += unicode_len;
            },
            TextChangeKind::Delete => {
              batch_ops.push(import_delta::NetOp::Delete(*deleted_len));
            },
          }
        }
      }
      if !batch_ops.is_empty() {
        body_batches.push(batch_ops);
      }
      for change in summary.changes {
        match change {
          SubscriptionChange::Text {
            target,
            unicode_start,
            unicode_len,
            deleted_len,
            inserted_structure,
            ..
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
          SubscriptionChange::Map { target, keys } => {
            // §6-R: harvest the registry record keys this batch touched — the
            // regional rematerializer's identity candidates beyond the old
            // projection's own rows.
            if target == self.paragraph_registry_container_id || target == self.block_registry_container_id {
              if target == self.paragraph_registry_container_id {
                imported_paragraph_record_keys.extend(keys.iter().cloned());
              } else {
                imported_block_record_keys.extend(keys.iter().cloned());
              }
              // A key change ON the registry parent is a record ADDED or
              // DELETED — the projection's row SET derives from these records,
              // so this is structural even with zero body-text ops. The class
              // that slipped through: an undo whose text component transforms
              // away (the char was deleted and re-inserted with fresh ids in
              // between) but whose record deletion survives — canonical drops
              // the row (orphan placeholder) while a neutral drain left the
              // maintained row standing (object-fuzz undo arm). Imports still
              // take the regional path (this reason is not in its rebuild-only
              // guard); local undo/redo takes the rebuild.
              invalidation.rebuild_required = true;
              if invalidation.fallback_reason.is_none() {
                invalidation.fallback_reason = Some("registry_record_set_change");
              }
            }
            classify_map_invalidation(invalidation, &target, &keys);
            // Every Map change may sit inside an object record's subtree
            // (attrs, caption/alt/source flow records, nested table maps) —
            // including opaque dynamically-created container ids that no path
            // rule can recognize. Marking the batch object-dirty makes the
            // ladder run the canonical object readback
            // (`remote_object_projection_patches`: O(objects), batched, pure
            // read, empty when nothing actually changed). The class that
            // slipped through: undo of an image alignment change was
            // body-neutral and left the maintained object row stale
            // (object-fuzz undo arm).
            invalidation.changed_blocks.push(target);
          },
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
    DrainedBodyDelta {
      net: import_delta::compose_batches(&body_batches),
      has_remote_origin,
      checkout_seen,
      imported_paragraph_record_keys,
      imported_block_record_keys,
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

  #[hotpath::measure]
  pub(crate) fn persist_update_segment(&mut self, from_frontier: Frontiers, from_vv: VersionVector, update: Vec<u8>) -> Result<()> {
    self.persist_update_segment_with_durability(from_frontier, from_vv, update, true)
  }

  /// §act-twelve A12.5.1: repair persists take the LAZY journal lane (no
  /// per-transaction fsync) — repairs are re-derivable, torn tails drop at
  /// replay, and any later synced append covers them. Everything else keeps
  /// the synchronous lane.
  pub(crate) fn persist_update_segment_lazy(&mut self, from_frontier: Frontiers, from_vv: VersionVector, update: Vec<u8>) -> Result<()> {
    self.persist_update_segment_with_durability(from_frontier, from_vv, update, false)
  }

  fn persist_update_segment_with_durability(
    &mut self,
    from_frontier: Frontiers,
    from_vv: VersionVector,
    update: Vec<u8>,
    sync: bool,
  ) -> Result<()> {
    if let Some(package) = &mut self.package {
      match package.append_update_segment(&from_frontier, &from_vv, &self.doc.state_frontiers(), &self.doc.state_vv(), update) {
        Ok(_) => {
          // §act-four M2: refresh the tiny preview header from the live
          // projection (O(header)) so cold preview of this edited-but-unflushed
          // document stays O(viewport). The header rides the journal delta to
          // disk with the segment. A failure here must not fail the persist.
          if let Err(error) = package.refresh_preview_header(&self.projection) {
            tracing::warn!(%error, "refreshing preview header after edit failed; cold preview may fall back to the full read");
          }
          // §act-eleven A11.3: inline compaction is a BACKSTOP, not the routine
          // path — the debounced autosave checkpoint compacts OFF-thread every
          // cycle (`CheckpointJob::run`), so paying a full O(doc+history)
          // snapshot export inside an interactive op (keystroke/undo commit)
          // is only justified when segments have grown far past the point a
          // checkpoint should have handled (headless handles with no autosave
          // wiring). 4x the routine threshold keeps the durability invariant
          // with a bound while removing the periodic latency spike from the
          // measured op (the recorded_inverse rows' hidden component).
          let compacted = package.compact_update_segments_if_needed(&self.doc, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD * 4)?;
          if let Some(path) = &self.package_path {
            if compacted.is_some() {
              package.write(path)?;
              self.package_journal_prepared = true;
            } else if self.package_journal_prepared {
              if sync {
                package.append_latest_update_to_prepared_path(path)?;
              } else {
                package.append_latest_update_to_prepared_path_lazy(path)?;
              }
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
          package.compact_to_snapshot_any(&self.doc)?;
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
                  kind: TextChangeKind::StyledRetain,
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
                kind: TextChangeKind::Insert,
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
                kind: TextChangeKind::Delete,
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
  // Exemption: user/replica IDENTITY records also write an `"id"` key but never
  // feed positional id resolution — rebuilding on them put a full O(doc) rebuild
  // on every fresh peer's first update (field: "second peer's first op stalled").
  // Identity records are recognizable by their exclusive key set.
  let identity_record = keys
    .iter()
    .any(|key| matches!(key.as_str(), "user_id" | "display_name" | "last_seen_at" | "created_at"));
  if !identity_record
    && keys
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
  if keys.iter().any(|key| {
    matches!(
      key.as_str(),
      "kind" | "flow_id" | "anchor_cursor" | "attrs" | "nested_refs" | "external_url"
    )
  }) {
    invalidation.changed_blocks.push(target.to_string());
  }
  if keys
    .iter()
    .any(|key| key == "section_id" || key == "sections_by_id")
  {
    invalidation.changed_sections.push(target.to_string());
  }
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
pub(crate) fn sentinel_protected_delete_range(start: usize, len: usize) -> Option<(usize, usize)> {
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
#[hotpath::measure]
pub(crate) fn prune_orphaned_body_object_blocks(doc: &LoroDoc, body: &loro::LoroText) -> loro::LoroResult<usize> {
  let body_snapshot = body.to_string();
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(0);
  };
  // ONE batched resolver pass for every anchor cursor. A DELETED anchor simply
  // fails to resolve (absent from the map) — which is exactly the orphan
  // signal. The previous per-record `doc.get_cursor_pos` walked update history
  // for each dead anchor, turning a select-all delete into an
  // O(objects × history) multi-minute freeze (the 2026-07-07 ctrl-A field bug).
  let anchor_pos = boundary_cursor_positions(doc, body, &blocks, &["anchor_cursor"]);
  let placeholder_positions: Vec<usize> = body_snapshot
    .chars()
    .enumerate()
    .filter_map(|(pos, ch)| (ch == OBJECT_REPLACEMENT).then_some(pos))
    .collect();
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
    let live = map_binary_opt(&block, "anchor_cursor")
      .and_then(|bytes| Cursor::decode(&bytes).ok())
      .and_then(|cursor| cursor.id)
      .and_then(|id| anchor_pos.get(&id).copied())
      .is_some_and(|pos| placeholder_positions.binary_search(&pos).is_ok());
    if !live {
      blocks.delete(&key)?;
      pruned += 1;
    }
  }
  Ok(pruned)
}

/// §fidelity: boolean mirror of the test helper `assert_semantic_projection_eq`
/// — whether two projections are
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
  for (ix, (l, r)) in left
    .ids
    .paragraph_ids
    .iter()
    .zip(right.ids.paragraph_ids.iter())
    .enumerate()
  {
    if l != r {
      return format!(
        "paragraph_ids[{ix}] incremental={} full={} text={:?}",
        l.0,
        r.0,
        flowstate_document::paragraph_text(right, ix)
      );
    }
  }
  if left.ids.block_ids.len() != right.ids.block_ids.len() {
    return format!("block_ids len {} != {}", left.ids.block_ids.len(), right.ids.block_ids.len());
  }
  for (ix, (l, r)) in left
    .ids
    .block_ids
    .iter()
    .zip(right.ids.block_ids.iter())
    .enumerate()
  {
    if l != r {
      return format!("block_ids[{ix}] incremental={} full={}", l.0, r.0);
    }
  }
  if left.sections != right.sections {
    return "sections differ".to_string();
  }
  for ix in 0..left.paragraphs.len().min(right.paragraphs.len()) {
    if flowstate_document::paragraph_text(left, ix) != flowstate_document::paragraph_text(right, ix) {
      return format!(
        "paragraph[{ix}] text incremental={:?} full={:?}",
        flowstate_document::paragraph_text(left, ix),
        flowstate_document::paragraph_text(right, ix)
      );
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextChangeKind {
  /// Style-only retain (mark/unmark over existing text).
  StyledRetain,
  Insert,
  Delete,
}

#[derive(Clone, Debug)]
enum SubscriptionChange {
  Text {
    target: String,
    kind: TextChangeKind,
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

pub(crate) fn object_replacement_patch(projection: &DocumentProjection, block_ix: usize, block: InputBlock) -> Option<Vec<ProjectionPatch>> {
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

pub(crate) fn projection_text_delta(
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

pub(crate) fn insert_projection_object_block(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  block_id: flowstate_document::BlockId,
  block_ix: usize,
  input: &InputBlock,
) -> Result<bool> {
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
  let unicode_index = projection_block_lead_pos_in_loro(doc, projection, &body, block_ix);
  insert_input_object_block(doc, unicode_index, block_id, input)?;
  Ok(true)
}

pub(crate) fn insert_input_object_block(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  input: &InputBlock,
) -> Result<()> {
  match input {
    InputBlock::Image(image) => insert_image_block_with_id(doc, unicode_index, block_id, image),
    InputBlock::Equation(equation) => insert_equation_block_with_id(doc, unicode_index, block_id, equation),
    InputBlock::Table(table) => insert_table_block_with_id(doc, unicode_index, block_id, table),
    InputBlock::Paragraph(_) => Ok(()),
  }
}

/// §act-three B.1: container-only sibling of [`insert_input_object_block`] for
/// the recorded-inverse undo restore. The U+FFFC placeholder is already back
/// in the body (it rode the verbatim bulk-text restore), so only the block
/// registry record + nested containers are recreated — keyed by the ORIGINAL
/// block id, which reproduces the exact durable key the delete pruned.
/// Tables are gated out of the fast path (their durable row/column/cell ids
/// cannot be re-minted losslessly here).
pub(crate) fn restore_input_object_block_containers(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  input: &InputBlock,
) -> Result<()> {
  let body = body_text(doc);
  match input {
    InputBlock::Image(image) => {
      let block = ensure_block_with_id(
        doc,
        &object_block_key("image", block_id),
        "image",
        ROOT_BODY_FLOW_ID,
        &body,
        unicode_index,
      )?;
      replace_image_block_from_input(doc, &block, image)
    },
    InputBlock::Equation(equation) => {
      let block = ensure_block_with_id(
        doc,
        &object_block_key("equation", block_id),
        "equation",
        ROOT_BODY_FLOW_ID,
        &body,
        unicode_index,
      )?;
      replace_equation_block_from_input(doc, &block, equation)
    },
    InputBlock::Table(_) => anyhow::bail!("tables are excluded from the recorded-inverse fast path"),
    InputBlock::Paragraph(_) => Ok(()),
  }
}

pub(crate) fn replace_projection_object_block(
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

pub(crate) fn replace_projection_equation_source_range(
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

pub(crate) fn replace_projection_image_alt_text(doc: &LoroDoc, image_block_id: flowstate_document::BlockId, text: &str) -> Result<bool> {
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

pub(crate) fn replace_projection_image_caption(
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

pub(crate) fn set_projection_image_layout(
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
pub(crate) fn empty_input_table_cell(row_id: RowId, column_id: ColumnId) -> InputTableCell {
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

pub(crate) fn delete_projection_object_block(doc: &LoroDoc, block_id: flowstate_document::BlockId) -> Result<bool> {
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

pub(crate) fn move_projection_object_block(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  block_id: flowstate_document::BlockId,
  new_block_ix: usize,
) -> Result<bool> {
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
  // `new_block_ix` is the target index in the POST-removal projection block list (the
  // incremental replay removes the source, then inserts at `new_block_ix`). Map it back
  // to the pre-removal block whose lead position it lands before, skipping the source's
  // own slot, then resolve that lead position on the post-delete body via durable cursors
  // (which survive the char deletion). A target at/after the tail appends.
  let source_ix = projection
    .ids
    .block_ids
    .iter()
    .position(|id| *id == block_id);
  body
    .delete(anchor_pos, 1)
    .context("deleting object placeholder before move")?;
  let remaining = projection.blocks.len().saturating_sub(1);
  let insert_pos = if new_block_ix >= remaining {
    body.len_unicode()
  } else {
    let original_ix = match source_ix {
      Some(source_ix) if new_block_ix >= source_ix => new_block_ix + 1,
      _ => new_block_ix,
    };
    projection_block_lead_pos_in_loro(doc, projection, &body, original_ix)
  };
  body
    .insert(insert_pos, &OBJECT_REPLACEMENT.to_string())
    .context("reinserting object placeholder after move")?;
  if let Some(cursor) = body.get_cursor(insert_pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  Ok(true)
}

/// Body-unicode position at which projection block `block_ix` begins — inserting an
/// `OBJECT_REPLACEMENT` char here makes it the new block `block_ix` (append when
/// `block_ix >= blocks.len()`). Coalescing-aware BY CONSTRUCTION: it resolves the lead
/// position from the projection's own (already-coalesced) block list plus each block's
/// durable cursor — an object's `anchor_cursor`, or a paragraph's boundary `\n` — so it
/// never miscounts the record-less phantom empties a raw body walk stumbled over (the
/// off-by-N that diverged the incremental replay from the canonical Loro rebuild). Insert
/// before a paragraph lands on its boundary `\n` (clamped past the sentinel), which
/// attaches the object to the previous block's tail exactly as `push_flow_blocks`
/// re-segments it.
fn projection_block_lead_pos_in_loro(doc: &LoroDoc, projection: &DocumentProjection, body: &LoroText, block_ix: usize) -> usize {
  let Some(block) = projection.blocks.get(block_ix) else {
    return body.len_unicode();
  };
  match block {
    flowstate_document::Block::Paragraph(_) => {
      let paragraph_ix = projection
        .blocks
        .iter()
        .take(block_ix)
        .filter(|block| matches!(block, flowstate_document::Block::Paragraph(_)))
        .count();
      // The paragraph's leading `\n` (sentinel-clamped: never insert before body pos 1).
      paragraph_boundary_loro_unicode_index(doc, projection, paragraph_ix).max(1)
    },
    _ => projection
      .ids
      .block_ids
      .get(block_ix)
      .and_then(|block_id| object_loro_block_by_projected_id(doc, body, *block_id))
      .map_or_else(|| body.len_unicode(), |(_, _, anchor_pos)| anchor_pos),
  }
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

/// Live start (unicode) of the paragraph identified by `paragraph_id` in the actual
/// Loro body flow, resolved from its durable boundary cursor — the paragraph's text
/// begins just past its boundary newline. Coalescing-agnostic: unlike the
/// projection-derived body-unicode index, this reflects boundary newlines that are
/// physically present in the body even when the projection has coalesced that
/// paragraph out of view (an object-adjacent empty paragraph). Returns `None` when
/// the durable record or its cursor cannot be resolved, so callers fall back to the
/// projection-space start.
#[hotpath::measure]
pub(crate) fn paragraph_body_start_in_loro(doc: &LoroDoc, paragraph_id: ParagraphId) -> Option<usize> {
  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID).ok()?;
  // Fast path: canonical record keys are directly constructible from the id
  // (`paragraph.{u128}`, plus the seeded first-paragraph key), so the common
  // case is an O(1) map probe instead of an O(records) key scan.
  let canonical_key = format!("paragraph.{}", paragraph_id.0);
  for key in [canonical_key.as_str(), ROOT_FIRST_PARAGRAPH_ID] {
    if loro_id_u128(key) == paragraph_id.0
      && let Some(paragraph) = child_map(&paragraphs, key)
    {
      return paragraph_record_body_start(doc, &paragraph);
    }
  }
  for key in map_keys(&paragraphs) {
    if loro_id_u128(&key) != paragraph_id.0 {
      continue;
    }
    let paragraph = child_map(&paragraphs, &key)?;
    return paragraph_record_body_start(doc, &paragraph);
  }
  None
}

fn paragraph_record_body_start(doc: &LoroDoc, paragraph: &LoroMap) -> Option<usize> {
  static DERIVE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  let debug = *DERIVE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some());
  for field in ["boundary_cursor", "start_cursor"] {
    let Some(bytes) = map_binary_opt(paragraph, field) else {
      if debug {
        eprintln!("derive[body-start]: field {field} missing on record");
      }
      continue;
    };
    let cursor = match Cursor::decode(&bytes) {
      Ok(cursor) => cursor,
      Err(error) => {
        if debug {
          eprintln!("derive[body-start]: field {field} decode failed: {error:?}");
        }
        continue;
      },
    };
    match doc.get_cursor_pos(&cursor) {
      Ok(resolved) => return Some(resolved.current.pos.saturating_add(1)),
      Err(error) => {
        if debug {
          eprintln!("derive[body-start]: field {field} get_cursor_pos failed: {error:?}");
        }
      },
    }
  }
  None
}

/// Batched sibling of [`paragraph_body_start_in_loro`]: resolve MANY
/// paragraphs' body starts through ONE `query_text_id_positions` pass.
/// Per-paragraph `get_cursor_pos` is a linear chunk scan per call (the
/// batch-resolver lesson), so a caller resolving thousands of paragraph
/// starts — a replace-all storm — was quadratic in document size. A
/// paragraph whose record or cursor cannot be batch-resolved falls back to
/// the exact single-paragraph path (rare: degraded cursors, seeded docs).
pub(crate) fn paragraph_body_starts_in_loro(doc: &LoroDoc, paragraph_ids: &[ParagraphId]) -> Vec<Option<usize>> {
  let root = doc.get_map(ROOT);
  let Ok(paragraphs) = root.ensure_mergeable_map(PARAGRAPHS_BY_ID) else {
    return vec![None; paragraph_ids.len()];
  };
  let body = body_text(doc);
  let container = body.id();
  let mut cursor_ids: Vec<Option<ID>> = Vec::with_capacity(paragraph_ids.len());
  for paragraph_id in paragraph_ids {
    let canonical_key = format!("paragraph.{}", paragraph_id.0);
    let record = [canonical_key.as_str(), ROOT_FIRST_PARAGRAPH_ID]
      .into_iter()
      .find(|key| loro_id_u128(key) == paragraph_id.0)
      .and_then(|key| child_map(&paragraphs, key));
    let id = record.and_then(|record| {
      ["boundary_cursor", "start_cursor"]
        .into_iter()
        .find_map(|field| {
          let bytes = map_binary_opt(&record, field)?;
          let cursor = Cursor::decode(&bytes).ok()?;
          (cursor.container == container)
            .then_some(cursor.id)
            .flatten()
        })
    });
    cursor_ids.push(id);
  }
  let query: Vec<ID> = cursor_ids.iter().copied().flatten().collect();
  let mut resolved = if query.is_empty() {
    Vec::new()
  } else {
    doc.inner().query_text_id_positions(&container, &query)
  }
  .into_iter();
  let batched: Vec<Option<usize>> = paragraph_ids
    .iter()
    .zip(&cursor_ids)
    .map(|(_, cursor_id)| {
      cursor_id
        .and_then(|_| resolved.next().flatten())
        .map(|pos| pos.saturating_add(1))
    })
    .collect();
  // §hang fix (field, 2026-07-07 peer 2): the per-id fallback runs
  // `get_cursor_pos`, and a DEAD cursor there triggers a full history-trace
  // (commit barrier + DiffCalculator checkout) PER ID. After an undo restores
  // a whole-document deletion, EVERY restored record's cursor is dead — the
  // receiving peer ran thousands of history traces and hung until ctrl-C.
  // The fallback is for the rare degraded-cursor/seeded-doc case; a MASS-dead
  // state must return `None`s instead, sending callers to the O(doc) full
  // rebuild + canonical re-anchor repair (~1s) rather than a multi-minute
  // trace storm.
  const DEAD_CURSOR_FALLBACK_CAP: usize = 8;
  let misses = batched.iter().filter(|start| start.is_none()).count();
  if misses > DEAD_CURSOR_FALLBACK_CAP {
    tracing::warn!(
      misses,
      total = paragraph_ids.len(),
      "mass dead-cursor state in batched paragraph-start resolution; skipping per-id history-trace fallbacks"
    );
    return batched;
  }
  batched
    .into_iter()
    .zip(paragraph_ids)
    .map(|(start, paragraph_id)| start.or_else(|| paragraph_body_start_in_loro(doc, *paragraph_id)))
    .collect()
}

/// Loro-space boundary position (the paragraph's leading `\n`) for projection
/// paragraph `paragraph_ix`, resolved from its durable cursor so coalesced empties
/// don't shift it. Loro-space counterpart of [`paragraph_boundary_unicode_index`];
/// use at Loro body-mutation sites (join delete, style mark). The paragraph's text
/// begins one unicode past its boundary. Falls back to the projection-space boundary
/// when the durable record can't be resolved (e.g. the boundary-0 sentinel).
pub(crate) fn paragraph_boundary_loro_unicode_index(doc: &LoroDoc, projection: &DocumentProjection, paragraph_ix: usize) -> usize {
  if paragraph_ix == 0 {
    return 0;
  }
  let record = projection
    .ids
    .paragraph_ids
    .get(paragraph_ix)
    .and_then(|paragraph_id| paragraph_body_start_in_loro(doc, *paragraph_id))
    .map(|start| start.saturating_sub(1));
  paragraph_boundary_from_record(doc, projection, paragraph_ix, record)
}

/// Batched sibling of [`paragraph_boundary_loro_unicode_index`]: resolves all
/// requested paragraphs' durable starts through ONE `query_text_id_positions`
/// pass (per-paragraph `get_cursor_pos` is a linear chunk scan — the
/// batch-resolver lesson), then applies the same rot-check + fallback law per
/// paragraph. Order-preserving with the input.
pub(crate) fn paragraph_boundaries_loro_unicode_indices(doc: &LoroDoc, projection: &DocumentProjection, paragraph_ixs: &[usize]) -> Vec<usize> {
  let query_ids: Vec<ParagraphId> = paragraph_ixs
    .iter()
    .filter(|&&ix| ix != 0)
    .filter_map(|&ix| projection.ids.paragraph_ids.get(ix).copied())
    .collect();
  let mut starts = paragraph_body_starts_in_loro(doc, &query_ids).into_iter();
  paragraph_ixs
    .iter()
    .map(|&ix| {
      if ix == 0 {
        return 0;
      }
      let record = if projection.ids.paragraph_ids.get(ix).is_some() {
        starts.next().flatten().map(|start| start.saturating_sub(1))
      } else {
        None
      };
      paragraph_boundary_from_record(doc, projection, ix, record)
    })
    .collect()
}

/// The shared boundary law: trust the durable-record boundary only when the
/// body text right after it matches the paragraph's projected prefix.
///
/// The durable cursor survives coalesced empties (projection-row math does
/// not), but it can ROT under undo churn: the record's anchor re-resolves
/// onto a NEIGHBORING newline after its original op ids die, so a style
/// mark or join lands on the wrong paragraph (object-fuzz divergence).
/// Content check: the boundary is only trusted when the text right after it
/// matches the paragraph's projected prefix; otherwise fall back to
/// projection-space math (in-sync with the doc inside the gate).
fn paragraph_boundary_from_record(doc: &LoroDoc, projection: &DocumentProjection, paragraph_ix: usize, record: Option<usize>) -> usize {
  let body = body_text(doc);
  match record {
    Some(boundary) if boundary_matches_paragraph(&body, projection, paragraph_ix, boundary) => boundary,
    _ => {
      let fallback = paragraph_boundary_unicode_index(projection, paragraph_ix);
      if boundary_matches_paragraph(&body, projection, paragraph_ix, fallback) {
        fallback
      } else {
        record.unwrap_or(fallback)
      }
    },
  }
}

/// The shared boundary trust check: `boundary` is this paragraph's leading
/// newline only when the live body text right after it matches the
/// paragraph's projected prefix.
fn boundary_matches_paragraph(body: &loro::LoroText, projection: &DocumentProjection, paragraph_ix: usize, boundary: usize) -> bool {
  if body.char_at(boundary) != Ok('\n') {
    return false;
  }
  let text = flowstate_document::paragraph_text(projection, paragraph_ix);
  if text.is_empty() {
    // An empty paragraph's boundary is followed by a terminator (next
    // boundary, an object placeholder, or end of body) — anything else is a
    // different paragraph's newline. Without this, EVERY newline vacuously
    // matched an empty paragraph and a rotten cursor sailed through.
    return match body.char_at(boundary + 1) {
      Ok(next) => next == '\n' || next == flowstate_document::OBJECT_REPLACEMENT,
      Err(_) => true,
    };
  }
  text
    .chars()
    .take(4)
    .enumerate()
    .all(|(offset, expected)| body.char_at(boundary + 1 + offset) == Ok(expected))
}

/// Whether the MAINTAINED body-unicode `start` for `paragraph_ix` matches the
/// live body (its leading boundary newline plus the paragraph's projected
/// prefix) — the cheap gate that lets keystroke anchor resolution skip the
/// per-call durable-cursor chunk scan.
fn body_start_matches_paragraph(doc: &LoroDoc, projection: &DocumentProjection, paragraph_ix: usize, start: usize) -> bool {
  let Some(boundary) = start.checked_sub(1) else {
    return false;
  };
  boundary_matches_paragraph(&body_text(doc), projection, paragraph_ix, boundary)
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
  // MUST match `flowstate_document::loro_projection::loro_id_u128` exactly —
  // the projector mints every projection id with this law, and every runtime
  // lookup compares against those ids. A uuid-v5 variant here once silently
  // disagreed on every NON-decimal key (`paragraph.anchor.op-…` repair keys,
  // `paragraph.initial`), which made repaired record-less boundaries
  // permanently unresolvable to the runtime: the §6-R region resolver bailed
  // to a full rebuild on every later structural import whose region touched a
  // healed paragraph (the REMOTE-STRUCT-BARE compounding heal cost), and the
  // same paragraphs were re-reported as defects and re-repaired every round.
  let hash = blake3::hash(id.as_bytes());
  let mut bytes = [0_u8; 16];
  bytes.copy_from_slice(&hash.as_bytes()[..16]);
  u128::from_le_bytes(bytes)
}

fn replace_image_block_from_input(doc: &LoroDoc, block: &LoroMap, image: &flowstate_document::InputImageBlock) -> Result<()> {
  block.insert("kind", "image")?;
  block.insert("asset_id", image.asset_id.0.to_string())?;
  copy_asset_metadata_to_image_block(doc, block, image.asset_id.0)?;
  // §A11.9: linked images carry their external URL; written only when a
  // non-empty URL exists, deleted otherwise (a replace may swap a linked image
  // for an embedded one) — mirrors `loro_import::import_image_block` and the
  // projector's `external_url` readback. The presence guard avoids minting a
  // tombstone op per replace on the common embedded path.
  match image.external_url.as_deref().filter(|url| !url.is_empty()) {
    Some(url) => block.insert("external_url", url)?,
    None => {
      if block.get("external_url").is_some() {
        block.delete("external_url")?;
      }
    },
  }

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
    let _attrs = column_map.ensure_mergeable_map("attrs")?;
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
    let _attrs = row_map.ensure_mergeable_map("attrs")?;
    for cell in &row.cells {
      // §P2b law hardening: a cell's canonical identity IS its coordinate mix.
      // Derive it here rather than trusting the payload's `cell.id` — a
      // payload-minted random id would double-key the coordinate (writer by
      // coordinate, reader by record) and split the cell across replicas.
      let cell_id = cell_loro_id(CellId::from_coordinate(cell.row_id, cell.column_id));
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
  cell_map.insert("row_id", row_id)?;
  cell_map.insert("column_id", column_id)?;
  cell_map.insert("row_span", i64::from(cell.row_span))?;
  cell_map.insert("column_span", i64::from(cell.col_span))?;
  let _attrs = cell_map.ensure_mergeable_map("attrs")?;
  let nested_table_ids = cell_map.ensure_mergeable_movable_list("nested_table_ids")?;
  let nested_tables_by_id = cell_map.ensure_mergeable_map("nested_tables_by_id")?;
  clear_movable_list(&nested_table_ids)?;
  clear_map(&nested_tables_by_id)?;
  let flow_id = format!("{cell_id}.flow");
  cell_map.insert("flow_id", flow_id.as_str())?;
  let flow = ensure_flow(doc, &flow_id, "table_cell")?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
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
  cell_map.insert("row_id", row_id)?;
  cell_map.insert("column_id", column_id)?;
  cell_map.insert("row_span", i64::from(cell.row_span))?;
  cell_map.insert("column_span", i64::from(cell.col_span))?;
  let _attrs = cell_map.ensure_mergeable_map("attrs")?;
  let flow_id = map_string_opt(cell_map, "flow_id").unwrap_or_else(|| format!("{cell_id}.flow"));
  cell_map.insert("flow_id", flow_id.as_str())?;
  let flow = ensure_flow(doc, &flow_id, "table_cell")?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
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

pub(crate) fn join_projection_paragraphs(
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
  if body.char_at(boundary) != Ok('\n') {
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
  // The joined-away paragraph's records were retired explicitly above; no
  // record can go stale here, so the O(records) prune sweep is not run
  // (§11 — it cost ~100ms per Backspace-join on a 6k-paragraph doc).
  Ok(true)
}

pub(crate) fn delete_projection_paragraph_metadata(doc: &LoroDoc, paragraph_id: ParagraphId, block_id: BlockId) -> loro::LoroResult<()> {
  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  // Canonical keys are directly constructible, making the common case an
  // O(1) delete — the key scan (which also sweeps non-canonical duplicate
  // keys) would go quadratic on multi-paragraph deletes (select-all class).
  // A missed legacy-keyed duplicate surfaces as a projection defect on the
  // next full rebuild and is repaired there.
  let canonical_paragraph = format!("paragraph.{}", paragraph_id.0);
  if child_map(&paragraphs, &canonical_paragraph).is_some() {
    paragraphs.delete(&canonical_paragraph)?;
  } else {
    for key in map_keys(&paragraphs) {
      let id = child_map(&paragraphs, &key)
        .and_then(|paragraph| map_string_opt(&paragraph, "id"))
        .unwrap_or_else(|| key.clone());
      if loro_id_u128(&id) == paragraph_id.0 {
        paragraphs.delete(&key)?;
      }
    }
  }

  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let canonical_block = format!("paragraph_block.{}", block_id.0);
  if child_map(&blocks, &canonical_block).is_some() {
    blocks.delete(&canonical_block)?;
  } else {
    for key in map_keys(&blocks) {
      let id = child_map(&blocks, &key)
        .and_then(|block| map_string_opt(&block, "id"))
        .unwrap_or_else(|| key.clone());
      if loro_id_u128(&id) == block_id.0 {
        blocks.delete(&key)?;
      }
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

pub(crate) fn inserted_newline_boundaries(start_unicode: usize, text: &str) -> Vec<usize> {
  text
    .chars()
    .enumerate()
    .filter_map(|(offset, ch)| (ch == '\n').then_some(start_unicode + offset))
    .collect()
}

fn persist_body_paragraph_style_mark_repair(doc: &LoroDoc, package: Option<&mut DocumentPackage>, package_path: Option<&Path>) -> Result<bool> {
  let from_frontier = doc.state_frontiers();
  let from_vv = doc.state_vv();
  // §act-twelve A12.1.1: two open-cost kills, measured on the impact doc.
  // (a) `register_replica` unconditionally committed a `last_seen_at` touch,
  //     advancing the frontier on EVERY open and permanently invalidating the
  //     package's frontier-stamped projection/search caches for later opens
  //     (the probe showed hit=true only on a package's first-ever open). The
  //     if-absent variant commits only for genuinely new replicas/versions;
  //     the touch rides the first real edit (`register_pending_author_identity`).
  // (b) The whole-corpus style-mark verification scan cost 1.1-1.2s per open;
  //     packages written by live runtimes stamp `style_marks_clean_frontier`,
  //     and a frontier match skips the scan (legacy/foreign packages still
  //     scan once — their next checkpoint stamps).
  // Registration is FULLY deferred to the first local edit (the runtime's
  // `pending_replica_touch`): every open mints a fresh Loro peer id, so an
  // open-time "if absent" check still registered (and committed) every time.
  // A replica that never edits needs no record at all.
  let replica_registered = false;
  let marks_known_clean = package
    .as_ref()
    .is_some_and(|package| package.manifest.style_marks_clean_frontier.as_deref() == Some(from_frontier.encode().as_slice()));
  {
    static OPEN_PROBE3: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *OPEN_PROBE3.get_or_init(|| std::env::var_os("FLOWSTATE_OPEN_PROBE").is_some()) {
      eprintln!(
        "[flowstate-open-probe]   style-stamp: known_clean={marks_known_clean} stamp_present={} frontier_len={}",
        package
          .as_ref()
          .is_some_and(|p| p.manifest.style_marks_clean_frontier.is_some()),
        from_frontier.encode().len()
      );
    }
  }
  let paragraph_marks_repaired = if marks_known_clean {
    false
  } else {
    repair_missing_paragraph_style_marks(doc)?
  };
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
  for item in flowstate_document::streaming_delta::streaming_to_delta(body) {
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

pub(crate) fn repair_paragraph_metadata_after_text_flow_edit(
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

#[hotpath::measure]
pub(crate) fn repair_paragraph_metadata_after_stable_split(
  doc: &LoroDoc,
  body: &loro::LoroText,
  boundary: usize,
  paragraph_id: flowstate_document::ParagraphId,
  block_id: flowstate_document::BlockId,
  reason: &'static str,
) -> loro::LoroResult<()> {
  let _ = reason;
  // §11 complexity contract: a local split retires no records (fresh unique
  // ids at a fresh boundary), so the O(records) stale-metadata prune does NOT
  // run here (it cost ~100ms per Enter on a 6k-paragraph doc). Stale and
  // duplicate records only arise from concurrent-merge imports, and every
  // full rebuild collects them as projection defects and schedules canonical
  // repair (`refresh_projection` §P2a).
  ensure_paragraph_metadata_at_boundary_with_ids(doc, body, boundary, paragraph_id, block_id)
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

#[hotpath::measure]
fn ensure_paragraph_metadata_at_boundary_with_keys(
  doc: &LoroDoc,
  body: &loro::LoroText,
  boundary: usize,
  forced_paragraph_id: Option<String>,
  forced_block_id: Option<String>,
) -> loro::LoroResult<()> {
  if body.char_at(boundary) != Ok('\n') {
    tracing::warn!(
      boundary,
      "cannot create paragraph metadata because boundary is not a live paragraph newline"
    );
    return Ok(());
  }

  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  // ONE cursor resolution for all three anchor fields — `get_cursor` walks
  // text chunks per call, and this writer runs per split and per boundary in
  // a mass-repair pass; the three identical calls tripled that cost.
  let boundary_cursor = body
    .get_cursor(boundary, Side::Left)
    .map(|cursor| cursor.encode());
  let paragraph_id = forced_paragraph_id
    .or_else(|| paragraph_metadata_key_at_boundary(doc, body, &paragraphs, boundary))
    .unwrap_or_else(|| new_paragraph_metadata_id(boundary));
  let paragraph = paragraphs.ensure_mergeable_map(&paragraph_id)?;
  paragraph.insert("id", paragraph_id.as_str())?;
  paragraph.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(encoded) = &boundary_cursor {
    paragraph.insert("start_cursor", encoded.clone())?;
    paragraph.insert("boundary_cursor", encoded.clone())?;
  }
  let _paragraph_attrs = paragraph.ensure_mergeable_map("attrs")?;

  let block_id = forced_block_id
    .or_else(|| paragraph_block_key_at_boundary(doc, body, &blocks, boundary))
    .unwrap_or_else(|| new_paragraph_block_id(boundary));
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("kind", "paragraph")?;
  block.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  block.insert("paragraph_id", paragraph_id.as_str())?;
  if let Some(encoded) = boundary_cursor {
    block.insert("anchor_cursor", encoded)?;
  }
  let _block_attrs = block.ensure_mergeable_map("attrs")?;
  let _nested_refs = block.ensure_mergeable_map("nested_refs")?;
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

#[hotpath::measure]
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
  for (id, pos) in ids
    .iter()
    .copied()
    .zip(doc.inner().query_text_id_positions(&container, &ids))
  {
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
fn live_cursor_pos(
  doc: &LoroDoc,
  live_boundaries: &[usize],
  pos_by_id: &FxHashMap<ID, usize>,
  map: &LoroMap,
  cursor_key: &str,
) -> Option<usize> {
  let cursor = Cursor::decode(&map_binary_opt(map, cursor_key)?).ok()?;
  let pos = match cursor.id {
    // The batched resolver covered every id-carrying cursor: absence means the
    // anchor is DELETED. Falling back to `get_cursor_pos` here would walk
    // update history per dead anchor — O(records × history) after a mass
    // delete (the ctrl-A freeze class). Dead ⇒ not live, directly.
    Some(id) => pos_by_id.get(&id).copied()?,
    // Id-less cursors (F7: whole-body-end resolution) are rare and can't be
    // batch-resolved; the per-cursor resolution stays for them alone.
    None => doc.get_cursor_pos(&cursor).ok()?.current.pos,
  };
  live_boundaries.binary_search(&pos).is_ok().then_some(pos)
}

fn live_object_cursor_pos(doc: &LoroDoc, body_snapshot: &str, map: &LoroMap, cursor_key: &str) -> Option<usize> {
  let cursor = Cursor::decode(&map_binary_opt(map, cursor_key)?).ok()?;
  let pos = doc.get_cursor_pos(&cursor).ok()?.current.pos;
  (body_snapshot.chars().nth(pos) == Some(OBJECT_REPLACEMENT)).then_some(pos)
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

pub(crate) fn map_string_opt(map: &LoroMap, key: &str) -> Option<String> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  })
}

pub(crate) fn map_binary_opt(map: &LoroMap, key: &str) -> Option<Vec<u8>> {
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

fn install_undo_selection_callbacks(undo: &UndoManager, state: &Arc<Mutex<UndoSelectionState>>) {
  let push_state = Arc::clone(state);
  undo.set_on_push(Some(Box::new(move |_, _, _| {
    let mut meta = UndoItemMeta::new();
    if let Ok(state) = push_state.lock()
      && let Some(selection) = &state.pending_selection
    {
      // Loro-first spec §10: selection positions ride in the first-class
      // `cursors` field, which Loro actively TRANSFORMS through remote diffs
      // and container remaps while the item sits on the undo stack. The
      // opaque `value` blob keeps only affinity/direction sidecar data (plus
      // the capture-time cursor bytes as a decode fallback).
      if let Ok(snapshot) = postcard::from_bytes::<UndoSelectionSnapshot>(selection.as_ref()) {
        if let Ok(anchor) = Cursor::decode(&snapshot.anchor_cursor) {
          meta.add_cursor(&anchor);
        }
        if let Ok(head) = Cursor::decode(&snapshot.head_cursor) {
          meta.add_cursor(&head);
        }
      }
      meta.set_value(LoroValue::Binary(selection.clone().into()));
    }
    meta
  })));

  let pop_state = Arc::clone(state);
  undo.set_on_pop(Some(Box::new(move |_, _, meta| {
    let transformed: Vec<Vec<u8>> = meta
      .cursors
      .iter()
      .map(|entry| entry.cursor.encode())
      .collect();
    let LoroValue::Binary(bytes) = meta.value else {
      return;
    };
    match postcard::from_bytes::<UndoSelectionSnapshot>(bytes.as_ref()) {
      Ok(mut selection) => {
        // Prefer the stack-transformed cursors over the capture-time bytes:
        // concurrent remote edits between capture and undo have already been
        // applied to them by the UndoManager (spec §10, test #9).
        if transformed.len() == 2 {
          selection.anchor_cursor = transformed[0].clone();
          selection.head_cursor = transformed[1].clone();
        }
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

pub(crate) fn map_i64_opt(map: &LoroMap, key: &str) -> Option<i64> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  })
}

pub(crate) fn map_bool_opt(map: &LoroMap, key: &str) -> Option<bool> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Bool(value)) => Some(value),
    _ => None,
  })
}

pub(crate) fn existing_child_map(parent: &LoroMap, key: &str) -> Result<LoroMap> {
  match parent.get(key) {
    Some(ValueOrContainer::Container(Container::Map(map))) => Ok(map),
    _ => anyhow::bail!("Comment record `{key}` does not exist"),
  }
}

/// C-S1: THE thread-authorship resolution — used by both enforcement
/// (delete) and the materializer, so UI visibility can never diverge from
/// what the server checks. Threads written before the author field existed
/// fall back to the earliest message's author.
pub(crate) fn comment_thread_author(thread: &LoroMap) -> Option<u128> {
  map_string_opt(thread, "author_user_id")
    .and_then(|id| id.parse::<u128>().ok())
    .or_else(|| {
      let messages = existing_child_map(thread, "messages_by_id").ok()?;
      messages
        .values()
        .filter_map(|value| match value {
          ValueOrContainer::Container(Container::Map(message)) => Some(message),
          _ => None,
        })
        .min_by_key(|message| map_i64_opt(message, "created_at").unwrap_or_default())
        .and_then(|message| map_string_opt(&message, "author_user_id"))
        .and_then(|id| id.parse::<u128>().ok())
    })
}

fn existing_comment_thread(doc: &LoroDoc, comment_id: u128) -> Result<LoroMap> {
  let root = flowstate_document::loro_schema::root_map(doc);
  let comments = existing_child_map(&root, flowstate_document::COMMENTS_BY_ID)?;
  existing_child_map(&comments, &comment_id.to_string())
}

pub(crate) fn validated_comment_body(body: &str) -> Result<String> {
  // Strip control characters (a pasted `\r\n` becomes `\n`) so a comment can
  // never smuggle escape sequences or zero-width control bytes into peers' UIs.
  let sanitized: String = body
    .chars()
    .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
    .collect();
  let body = sanitized.trim();
  anyhow::ensure!(!body.is_empty(), "Comment text cannot be empty");
  anyhow::ensure!(body.len() <= 32 * 1024, "Comment text is too long");
  Ok(body.to_owned())
}

pub(crate) fn write_comment_message(
  messages: &LoroMap,
  message_id: u128,
  body: &str,
  author_user_id: u128,
  author_display_name: &str,
  now: i64,
) -> Result<()> {
  let message = messages.ensure_mergeable_map(&message_id.to_string())?;
  message.insert("id", message_id.to_string())?;
  message.insert("author_user_id", author_user_id.to_string())?;
  message.insert("author_display_name", author_display_name)?;
  message.insert("body", body)?;
  message.insert("created_at", now)?;
  message.insert("updated_at", now)?;
  Ok(())
}

pub(crate) fn unix_time_secs() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
    .try_into()
    .unwrap_or(i64::MAX)
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

std::thread_local! {
  /// §caret-anchor: count of O(doc) `fork_at` caret rebases on this thread. The
  /// FAST path (stored cursors) does NOT touch it; a test asserts the delta is 0
  /// when the caret was captured, so a regression to always-fork fails loud.
  static CARET_REBASE_FORKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Read this thread's caret fork-rebase count (see `CARET_REBASE_FORKS`).
pub fn caret_rebase_fork_count() -> u64 {
  CARET_REBASE_FORKS.with(std::cell::Cell::get)
}

pub(crate) fn cursor_for_boundary(text: &LoroText, unicode_pos: usize, affinity: SelectionAffinity) -> Option<Cursor> {
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

pub(crate) fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

pub(crate) fn mark_run_styles(text: &loro::LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> loro::LoroResult<()> {
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
    let _attrs = column.ensure_mergeable_map("attrs")?;
    write_table_column_width(
      &column,
      column_widths
        .get(column_ix)
        .unwrap_or(&InputTableColumnWidth::Auto),
    )?;
    minted_columns.push((column_id, column_id_str));
  }

  for _ in 0..rows {
    let row_id = RowId(Uuid::new_v4().as_u128());
    let row_id_str = row_loro_id(row_id);
    row_order.push(row_id_str.as_str())?;
    let row = rows_by_id.ensure_mergeable_map(&row_id_str)?;
    row.insert("id", row_id_str.as_str())?;
    let _attrs = row.ensure_mergeable_map("attrs")?;
    for (column_id, column_id_str) in &minted_columns {
      let cell_id_str = cell_loro_id_for(row_id, *column_id);
      let cell = cells_by_id.ensure_mergeable_map(&cell_id_str)?;
      cell.insert("id", cell_id_str.as_str())?;
      cell.insert("row_id", row_id_str.as_str())?;
      cell.insert("column_id", column_id_str.as_str())?;
      cell.insert("row_span", 1_i64)?;
      cell.insert("column_span", 1_i64)?;
      let _attrs = cell.ensure_mergeable_map("attrs")?;
      let _nested_table_ids = cell.ensure_mergeable_movable_list("nested_table_ids")?;
      let _nested_tables_by_id = cell.ensure_mergeable_map("nested_tables_by_id")?;
      let flow_id = format!("{cell_id_str}.flow");
      cell.insert("flow_id", flow_id.as_str())?;
      let flow = ensure_flow(doc, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
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
  let _attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("text_container_id", text.id().to_string())?;
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
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = text.get_cursor(pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let _attrs = block.ensure_mergeable_map("attrs")?;
  let _nested_refs = block.ensure_mergeable_map("nested_refs")?;
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

/// The off-gate half of a split checkpoint (field fix 2026-07-07): heavy
/// package assembly against a doc FORK plus the checked-out package. No live
/// doc access — safe on a detached worker.
pub struct CheckpointJob {
  /// §A13.4.4: `Some` = a gate-held fork (legacy shape, still used by tests
  /// and callers that already hold one). `None` = the job derives its doc
  /// OFF-GATE from the package it owns — the package's snapshot + segments
  /// ARE the live doc's state at the checkpoint frontier (the trailing
  /// segment was persisted under the same gate hold that built this job),
  /// so the ~110ms `doc.fork()` no longer stalls typing at save time.
  pub fork: Option<LoroDoc>,
  pub projection: DocumentProjection,
  pub package: DocumentPackage,
  pub title: String,
  /// H-S1: what the minted revision record says (tier + display title) —
  /// distinct from `title`, which names the PACKAGE.
  pub revision_stamp: flowstate_document::RevisionStamp,
  pub revision_id: u128,
  pub revision_frontiers: Frontiers,
  pub author_user_id: Option<u128>,
  pub peer_id: u64,
  pub write_path: Option<PathBuf>,
  /// `false` for the close-time cache flush: identical assembly, but no
  /// named revision is minted (a flush is not a user save).
  pub record_revision: bool,
}

impl CheckpointJob {
  /// Run the heavy assembly + disk write. Returns the package (for restore)
  /// and whether it reached disk.
  pub fn run(mut self) -> (DocumentPackage, io::Result<bool>) {
    static CHECKPOINT_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let probe = *CHECKPOINT_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_CHECKPOINT_PROBE").is_some());
    let result = (|| -> io::Result<bool> {
      let probe_t = std::time::Instant::now();
      // §A13.4.4: reconstruct the checkpoint doc from the package when no
      // fork was supplied (shallow chunk fast path, full decode fallback) —
      // and verify it landed on the manifest tip before deriving anything.
      let fork = match self.fork.take() {
        Some(fork) => fork,
        None => {
          let derived = match self.package.load_loro_doc_shallow()? {
            Some(doc) => doc,
            None => self.package.load_loro_doc_from_validated()?,
          };
          if derived.oplog_frontiers().encode() != self.package.manifest.latest_frontier {
            return Err(io::Error::new(
              io::ErrorKind::InvalidData,
              "checkpoint doc derived from the package did not land on the manifest tip",
            ));
          }
          derived
        },
      };
      let probe_derive = probe_t.elapsed();
      let probe_t = std::time::Instant::now();
      self
        .package
        .replace_assets_from_document(&self.projection)?;
      let probe_assets = probe_t.elapsed();
      // §act-five P9: ONE materialization feeds both the projection cache and the
      // search units (was two full `document_from_loro` walks per checkpoint).
      let probe_t = std::time::Instant::now();
      // §A13.4.1: the job carries the runtime's maintained projection —
      // build the cache payload from IT when frontier-current (from-Loro
      // walk only on mismatch).
      self
        .package
        .rebuild_caches_from_projection(&fork, &self.projection)?;
      let probe_caches = probe_t.elapsed();
      let probe_t = std::time::Instant::now();
      // §act-twelve A12.4.1 incremental persistence: routine revision
      // checkpoints (explicit save + debounced autosave) skip the
      // full-history snapshot export while the update-segment chain is
      // short — durability is unchanged (segments are journaled per edit;
      // this write persists them with fresh caches), only the seconds-scale
      // consolidation moves to the close/idle cache flush or the segment
      // threshold. The shallow accelerator still refreshes so the next open
      // stays fast.
      static ALWAYS_COMPACT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
      let always_compact = *ALWAYS_COMPACT.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_INCREMENTAL_CHECKPOINT").is_some());
      let consolidate = always_compact
        || !self.record_revision
        || self.package.loro_update_segments.len() >= flowstate_document::DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD;
      if consolidate {
        self.package.compact_to_snapshot_any(&fork)?;
      } else {
        self.package.refresh_shallow_snapshot(&fork);
      }
      let probe_compact = probe_t.elapsed();
      let probe_t = std::time::Instant::now();
      if self.record_revision {
        self.package.create_named_revision_at_with_id(
          &fork,
          self.revision_id,
          &self.revision_frontiers,
          self.revision_stamp.display_title(),
          "",
          self.revision_stamp.kind,
          self.author_user_id,
          Some(self.peer_id as u128),
        )?;
      }
      let probe_revision = probe_t.elapsed();
      if let Some(path) = &self.write_path {
        let probe_t = std::time::Instant::now();
        self.package.write(path)?;
        if probe {
          eprintln!(
            "[flowstate-checkpoint-probe] derive={probe_derive:?} assets={probe_assets:?} caches={probe_caches:?} compact={probe_compact:?} revision={probe_revision:?} write={:?}",
            probe_t.elapsed()
          );
        }
        return Ok(true);
      }
      if probe {
        eprintln!(
          "[flowstate-checkpoint-probe] assets={probe_assets:?} caches={probe_caches:?} compact={probe_compact:?} revision={probe_revision:?} write=skipped"
        );
      }
      Ok(false)
    })();
    (self.package, result)
  }
}

/// Assemble a full package (snapshot + assets + caches) from a FORKED doc and
/// a projection clone — no live-doc access, safe off-gate and off-thread.
pub fn assemble_package_bytes(fork: &LoroDoc, projection: &DocumentProjection, title: &str) -> io::Result<Vec<u8>> {
  let mut package = DocumentPackage::from_loro_snapshot_with_assets(fork, title, assets_from_document(projection))?;
  package.replace_assets_from_document(projection)?;
  // §act-five P9: one materialization for both caches (see `rebuild_caches_from_loro`).
  package.rebuild_caches_from_loro(fork)?;
  package.to_bytes()
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_document::{DocumentPackage, ProjectionPatch, ProjectionTextDelta, loro_schema::body_text};

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

  pub(crate) fn local_update_bytes(events: &[RuntimeEvent]) -> Vec<u8> {
    events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
        RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::FrontierViewOpened { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::ProjectionPatched { .. }
        | RuntimeEvent::HistoryRebaseRequired { .. }
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
  fn frontier_projection_shows_historical_state_and_leaves_live_doc_untouched() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "alpha".to_string(),
      styles: RunStyles::default(),
    })?;
    let frontier_after_alpha = runtime.doc().state_frontiers().encode();
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 6,
      text: " beta".to_string(),
      styles: RunStyles::default(),
    })?;

    // The historical view sees only the first edit...
    let historical = runtime.frontier_projection(&frontier_after_alpha)?;
    assert_eq!(flowstate_document::paragraph_text(&historical, 0), "alpha");
    // ...and via the command path it arrives as FrontierViewOpened.
    let events = runtime.command(SemanticCommand::OpenFrontier {
      frontier: frontier_after_alpha.clone(),
    })?;
    assert!(matches!(
      events.first(),
      Some(RuntimeEvent::FrontierViewOpened { frontier, document })
        if *frontier == frontier_after_alpha
          && flowstate_document::paragraph_text(document, 0) == "alpha"
    ));

    // The live doc is untouched by either read.
    assert_eq!(flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0), "alpha beta");
    assert_eq!(runtime.doc().state_frontiers().encode(), {
      runtime.doc().state_frontiers().encode()
    });

    // A garbage frontier fails loudly instead of projecting nonsense.
    assert!(runtime.frontier_projection(b"not a frontier").is_err());
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
  fn general_comments_tombstones_and_frontiers_converge() -> Result<()> {
    let base = flowstate_document::new_loro_document("Comments")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    source.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "alpha".to_string(),
      styles: RunStyles::default(),
    })?;
    source.set_author_identity(7, Some("Ada".into()))?;

    // A general comment: no selection, never an orphan, pinned by `general`.
    let general_id = source.create_comment(None, "Read the aff top-down first", 7, "Ada")?;
    // An anchored comment records its birth frontier.
    let selection = EditorSelection::range(DocumentOffset { paragraph: 0, byte: 0 }, DocumentOffset { paragraph: 0, byte: 5 });
    let anchored_id = source.create_comment(Some(&selection), "Tighten this", 7, "Ada")?;
    let reply_id = source.reply_to_comment(anchored_id, "on it", 9, "Sol")?;
    // Tombstone the reply (author-gated: the wrong actor is refused).
    assert!(source.delete_comment_message(anchored_id, reply_id, 7).is_err());
    source.delete_comment_message(anchored_id, reply_id, 9)?;

    // Converge to the peer via a full-state export.
    let update = source.doc().export(ExportMode::all_updates()).expect("export");
    target.import_remote_update(&update)?;

    for runtime in [&source, &target] {
      let threads = runtime.comments();
      let general = threads.iter().find(|thread| thread.comment_id == general_id).expect("general");
      assert!(general.general);
      assert!(general.anchor.is_none());
      assert_eq!(general.author_user_id, Some(7));
      let anchored = threads.iter().find(|thread| thread.comment_id == anchored_id).expect("anchored");
      assert!(!anchored.general);
      assert!(anchored.anchor.is_some());
      let frontier = anchored.created_frontier.clone().expect("birth frontier recorded");
      // H-K0 integration: the birth frontier is checkout-able.
      let historical = runtime.frontier_projection(&frontier)?;
      assert_eq!(flowstate_document::paragraph_text(&historical, 0), "alpha");
      let tombstoned = anchored
        .messages
        .iter()
        .find(|message| message.message_id == reply_id)
        .expect("tombstone survives");
      assert!(tombstoned.deleted);
      assert!(tombstoned.body.is_empty());
    }
    Ok(())
  }

  #[test]
  fn frontier_diff_reports_removed_spans_with_blame_and_insert_counts() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Diff")?;
    runtime.set_author_identity(7, Some("Ada".into()))?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "keep DOOMED keep".to_string(),
      styles: RunStyles::default(),
    })?;
    // Identity registration is lazy (rides the first intent-path edit in
    // production); the semantic-command test path registers it explicitly.
    runtime.register_pending_author_identity();
    let base = runtime.doc().state_frontiers().encode();
    // Delete "DOOMED " (unicode 6..13 in body space) and add a tail.
    runtime.command(SemanticCommand::DeleteRange {
      unicode_index: 6,
      unicode_len: 7,
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 10,
      text: " and new words".to_string(),
      styles: RunStyles::default(),
    })?;

    let diff = runtime.frontier_diff_vs(&base, None)?;
    assert_eq!(diff.removed_chars, 7);
    assert_eq!(diff.inserted_chars, 14);
    assert_eq!(diff.removed_since.len(), 1, "one removed span");
    let span = &diff.removed_since[0];
    assert_eq!((span.start.paragraph, span.start.byte), (0, 5), "span starts where the deletion began");
    assert_eq!((span.end.paragraph, span.end.byte), (0, 12));
    // Blame chain: op peer -> replica -> user -> display name.
    assert_eq!(span.author_user_id, Some(7));
    assert_eq!(span.author_display_name.as_deref(), Some("Ada"));

    // Identical frontiers diff empty.
    let now = runtime.doc().state_frontiers().encode();
    let empty = runtime.frontier_diff_vs(&now, None)?;
    assert!(empty.removed_since.is_empty() && empty.inserted_chars == 0 && empty.removed_chars == 0);
    Ok(())
  }

  #[test]
  fn restore_is_a_forward_op_with_a_safety_pin_undoable_and_convergent() -> Result<()> {
    use flowstate_document::{RevisionKind, RevisionStamp};
    let base = flowstate_document::new_loro_document("Restore")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;

    source.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "alpha".to_string(),
      styles: RunStyles::default(),
    })?;
    let historical = source.doc().state_frontiers().encode();
    source.command(SemanticCommand::InsertText {
      unicode_index: 6,
      text: " beta".to_string(),
      styles: RunStyles::default(),
    })?;
    // A package so the safety pin lands somewhere visible.
    source.checkpoint_package("Doc", None, &RevisionStamp::session())?;
    assert_eq!(flowstate_document::paragraph_text(&source.projection_snapshot()?, 0), "alpha beta");

    let events = source.restore_frontier(&historical)?;
    assert!(!events.is_empty(), "restore produces local-update events for peers");
    assert_eq!(
      flowstate_document::paragraph_text(&source.projection_snapshot()?, 0),
      "alpha",
      "the document reads as it did at the historical frontier"
    );
    // LAW: the present was pinned before the restore touched anything.
    assert!(
      source
        .revisions()
        .iter()
        .any(|revision| revision.kind == RevisionKind::Named && revision.title == "Before restore"),
      "restore is always preceded by a safety checkpoint"
    );

    // Convergence: peers see the restore as a normal edit.
    let update = source.doc().export(ExportMode::all_updates()).expect("export");
    target.import_remote_update(&update)?;
    assert_eq!(
      flowstate_document::paragraph_text(&target.projection_snapshot()?, 0),
      "alpha",
      "the restore converges like any local edit"
    );

    // Forward op: a single undo returns the pre-restore text.
    source.command(SemanticCommand::Undo)?;
    assert_eq!(
      flowstate_document::paragraph_text(&source.projection_snapshot()?, 0),
      "alpha beta",
      "restore is undoable"
    );
    Ok(())
  }

  #[test]
  fn revision_minting_tiers_rename_pins_and_thinning_spares_named() -> Result<()> {
    use flowstate_document::{RevisionKind, RevisionStamp};
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "history has tiers now".to_string(),
      styles: RunStyles::default(),
    })?;

    // First save creates the package; subsequent mints are tiered.
    runtime.checkpoint_package("Doc", None, &RevisionStamp::session())?;
    runtime.checkpoint_package("Doc", None, &RevisionStamp::auto())?;
    runtime.checkpoint_package("Doc", None, &RevisionStamp::named("Before the 1AR"))?;

    let revisions = runtime.revisions();
    let saved = revisions
      .iter()
      .find(|revision| revision.kind == RevisionKind::Session && revision.title == "Saved")
      .expect("session save carries a real title, not the filename");
    assert!(revisions.iter().all(|revision| revision.summary != "Explicit save"), "the filler summary is dead");
    let auto = revisions
      .iter()
      .find(|revision| revision.kind == RevisionKind::Auto)
      .expect("autosave grain is tiered");
    assert_eq!(auto.title, "Autosave");
    assert!(
      revisions
        .iter()
        .any(|revision| revision.kind == RevisionKind::Named && revision.title == "Before the 1AR"),
      "named pins carry the user's title"
    );
    let _ = saved;

    // Rename PINS: the auto record becomes a named pin with the new title.
    runtime.rename_revision(auto.revision_id, "Key cross-ex moment")?;
    let pinned = runtime
      .revisions()
      .into_iter()
      .find(|revision| revision.revision_id == auto.revision_id)
      .expect("renamed record survives");
    assert_eq!(pinned.kind, RevisionKind::Named);
    assert_eq!(pinned.title, "Key cross-ex moment");

    // Thinning: three stale autos in the same day-bucket collapse to one;
    // the named pin in the same bucket is exempt.
    runtime.checkpoint_package("Doc", None, &RevisionStamp::auto())?;
    runtime.checkpoint_package("Doc", None, &RevisionStamp::auto())?;
    runtime.checkpoint_package("Doc", None, &RevisionStamp::auto())?;
    let two_days = 2 * 86_400;
    {
      let package = runtime.package.as_mut().expect("package exists");
      for revision in &mut package.revisions {
        if revision.kind == RevisionKind::Auto || revision.revision_id == pinned.revision_id {
          revision.created_at_unix_secs -= two_days;
        }
      }
    }
    let removed = runtime.thin_auto_revisions()?;
    assert_eq!(removed, 2, "three same-bucket autos thin to the newest one");
    let after = runtime.revisions();
    assert_eq!(
      after.iter().filter(|revision| revision.kind == RevisionKind::Auto).count(),
      1,
      "one auto survives its day bucket"
    );
    assert!(
      after.iter().any(|revision| revision.revision_id == pinned.revision_id),
      "the backdated NAMED pin is exempt from thinning"
    );
    // The doc-side records shrank too (convergent thinning).
    let doc_count = {
      let root = flowstate_document::loro_schema::root_map(runtime.doc());
      match root.get(flowstate_document::loro_schema::REVISIONS) {
        Some(ValueOrContainer::Container(Container::List(revisions))) => revisions.len(),
        _ => 0,
      }
    };
    assert_eq!(doc_count, after.len(), "doc records mirror the manifest after thinning");
    Ok(())
  }

  #[test]
  fn orphaned_comment_reanchors_and_history_jump_shows_original_context() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "target words remain here".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.set_author_identity(7, Some("Ada".into()))?;

    // Comment on "target" (unicode 1..7 in body space → paragraph bytes 0..6).
    let selection = EditorSelection::range(DocumentOffset { paragraph: 0, byte: 0 }, DocumentOffset { paragraph: 0, byte: 6 });
    let comment_id = runtime.create_comment(Some(&selection), "This word is wrong", 7, "Ada")?;
    let created_frontier = runtime.comments()[0]
      .created_frontier
      .clone()
      .expect("birth frontier recorded");

    // Delete the anchored word: the thread orphans (anchor collapses away).
    runtime.command(SemanticCommand::DeleteRange {
      unicode_index: 1,
      unicode_len: 7,
    })?;
    assert!(
      runtime.comments()[0].anchor.is_none(),
      "deleting the anchored text must orphan the thread"
    );

    // C-S6 history-jump: the birth-frontier checkout shows the original text
    // and resolves the ORIGINAL anchor inside it.
    let (historical, anchor) = runtime.frontier_comment_context(&created_frontier, comment_id)?;
    assert_eq!(flowstate_document::paragraph_text(&historical, 0), "target words remain here");
    let (start, end) = anchor.expect("original anchor resolves at the birth frontier");
    assert_eq!((start.paragraph, start.byte), (0, 0));
    assert_eq!((end.paragraph, end.byte), (0, 6));
    // The live document is untouched by the checkout.
    assert_eq!(flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0), "words remain here");

    // C-S6 re-anchor: attach the thread to "words" and it stops being an orphan.
    let new_selection = EditorSelection::range(DocumentOffset { paragraph: 0, byte: 0 }, DocumentOffset { paragraph: 0, byte: 5 });
    runtime.reanchor_comment(comment_id, &new_selection)?;
    let thread = &runtime.comments()[0];
    assert_eq!(thread.quoted_text, "words");
    assert!(!thread.general);
    let (start, end) = thread.anchor.expect("re-anchored thread has a live anchor");
    assert_eq!((start.paragraph, start.byte), (0, 0));
    assert_eq!((end.paragraph, end.byte), (0, 5));
    // The birth frontier is untouched: history-jump still shows the TRUE origin.
    assert_eq!(
      thread.created_frontier.as_deref(),
      Some(created_frontier.as_slice()),
      "re-anchoring must not rewrite the thread's birth frontier"
    );
    Ok(())
  }

  #[test]
  fn comment_threads_converge_and_anchors_survive_edits() -> Result<()> {
    let base = flowstate_document::new_loro_document("Comments")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let insert = source.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello world".to_string(),
      styles: RunStyles::default(),
    })?;
    target.import_remote_update(&local_update_bytes(&insert))?;
    source.set_author_identity(7, Some("Ada".into()))?;
    let selection = EditorSelection::range(DocumentOffset { paragraph: 0, byte: 0 }, DocumentOffset { paragraph: 0, byte: 5 });
    let comment_id = source.create_comment(Some(&selection), "Needs a source", 7, "Ada")?;
    source.reply_to_comment(comment_id, "Added one", 9, "Grace")?;
    source.set_comment_resolved(comment_id, true)?;

    let updates = source.take_pending_publish();
    let chunks = updates
      .iter()
      .filter_map(|event| match event {
        RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.as_slice()),
        _ => None,
      })
      .collect::<Vec<_>>();
    target.import_remote_updates(&chunks)?;

    let local = source.comments();
    let remote = target.comments();
    assert_eq!(local, remote);
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].quoted_text, "hello");
    assert_eq!(local[0].messages.len(), 2);
    assert!(local[0].resolved);
    assert!(local[0].anchor.is_some());

    source.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "Well, ".to_string(),
      styles: RunStyles::default(),
    })?;
    let moved = source.comments()[0]
      .anchor
      .expect("anchor should survive an insertion before it");
    assert_eq!(moved.0.byte, 6);
    assert_eq!(moved.1.byte, 11);
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
    // §act-twelve A12.1.1: open no longer commits (replica registration is
    // deferred to the first local edit), so the floor is the two edit
    // segments — not the old open-repair segment plus edits.
    assert!(package.loro_update_segments.len() >= 2);
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
        ..Default::default()
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
          external_url: None,
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
      !violations
        .iter()
        .any(|violation| violation.contains("object-side-collapse")),
      "no object-side-collapse violation must fire: {violations:?}"
    );
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
    let blank_revision = package.create_named_revision(&doc, "Blank", "Blank document", flowstate_document::RevisionKind::Session, None, None)?;
    body_text(&doc).insert(1, "latest")?;
    doc.commit();
    package.compact_to_named_snapshot(&doc, "Latest", "Latest document", flowstate_document::RevisionKind::Session, None, None)?;
    package.write(&path)?;

    let mut runtime = CrdtRuntime::open_package(&path)?;
    let opened = runtime.command(SemanticCommand::OpenRevision { revision_id: blank_revision })?;
    assert!(matches!(
      opened.as_slice(),
      [RuntimeEvent::RevisionOpened { document, .. }] if document.paragraphs.first().is_some_and(|paragraph| paragraph.runs.is_empty())
    ));

    let forked = runtime.command(SemanticCommand::ForkRevision { revision_id: blank_revision })?;
    let [RuntimeEvent::RevisionForked { document, package, .. }] = forked.as_slice() else {
      panic!("expected fork event");
    };
    assert_eq!(
      document
        .paragraphs
        .first()
        .map(|paragraph| paragraph.runs.is_empty()),
      Some(true)
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
        | RuntimeEvent::FrontierViewOpened { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::HistoryRebaseRequired { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("remote import should emit a projection patch");

    let [
      ProjectionPatch::ParagraphText {
        row_hint, new, delta_utf8, ..
      },
    ] = patches.as_ref()
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
          external_url: None,
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
        | RuntimeEvent::FrontierViewOpened { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::HistoryRebaseRequired { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("remote import should emit a projection patch");

    let [
      ProjectionPatch::ParagraphText {
        row_hint, new, delta_utf8, ..
      },
    ] = patches.as_ref()
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
        index.paragraph_index(*id),
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
        index.block_index(*id),
        projection
          .ids
          .block_ids
          .iter()
          .position(|candidate| candidate == id),
      );
    }
    // Absent ids fall back to `None` through both the index and the scan.
    assert_eq!(index.paragraph_index(flowstate_document::new_paragraph_id()), None);
    assert_eq!(index.block_index(flowstate_document::new_block_id()), None);

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

  #[test]
  fn presence_non_collapsed_selection_resolves_to_a_colored_span() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Presence")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "abcd".to_string(),
      styles: RunStyles::default(),
    })?;
    let anchor = DocumentOffset { paragraph: 0, byte: 1 };
    let head = DocumentOffset { paragraph: 0, byte: 3 };
    let selection = EditorSelection {
      anchor,
      head,
      ..EditorSelection::default()
    };
    let presence = runtime
      .presence_selection(&selection)
      .expect("presence selection should encode");

    let resolved = runtime.resolve_presence_carets(vec![RuntimePresenceCaretRequest {
      selection: presence,
      color_rgb: 0xabcdef,
    }]);

    // The head still resolves to a caret, and the non-collapsed range resolves
    // to a single colored selection span carrying that peer's color.
    assert_eq!(resolved.carets.len(), 1);
    assert_eq!(resolved.carets[0].offset, head);
    assert_eq!(resolved.selections.len(), 1);
    assert_eq!(resolved.selections[0].color_rgb, 0xabcdef);
    assert_eq!(resolved.selections[0].selection.anchor, anchor);
    assert_eq!(resolved.selections[0].selection.head, head);
    Ok(())
  }

  #[test]
  fn presence_collapsed_selection_yields_no_span() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Presence")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "abcd".to_string(),
      styles: RunStyles::default(),
    })?;
    let offset = DocumentOffset { paragraph: 0, byte: 2 };
    let selection = EditorSelection::collapsed(offset);
    let presence = runtime
      .presence_selection(&selection)
      .expect("presence selection should encode");

    let resolved = runtime.resolve_presence_carets(vec![RuntimePresenceCaretRequest {
      selection: presence,
      color_rgb: 0xabcdef,
    }]);

    // A bare caret paints only a caret — never a zero-width highlight span.
    assert_eq!(resolved.carets.len(), 1);
    assert!(resolved.selections.is_empty());
    Ok(())
  }

  /// §P3 (act two): the close-time cache flush restores the preview fast
  /// path — an edit nulls the projection cache (preview falls back to a full
  /// Loro materialization), and `begin_cache_flush` + `CheckpointJob::run`
  /// writes a frontier-current cache back without minting a revision.
  #[test]
  fn cache_flush_restores_the_preview_fast_path() -> anyhow::Result<()> {
    use crate::local_write::{GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, TextAnchor};

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("flush.db8");
    let document = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
        style: flowstate_document::ParagraphStyle::Normal,
        runs: vec![flowstate_document::InputRun {
          text: "flush me".to_string(),
          styles: flowstate_document::RunStyles::default(),
        }],
      })],
    );
    let imported = flowstate_document::import_document_projection(document, "Flush")?;
    let bytes = assemble_package_bytes(&imported.doc, &imported.projection, "Flush")?;
    std::fs::write(&path, bytes)?;
    assert!(
      DocumentPackage::read_cached_projection(&path)?.is_some(),
      "a freshly assembled package must carry a frontier-current projection cache"
    );

    let runtime = CrdtRuntime::open_package(&path)?;
    let (handle, gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
    let paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: " again".to_string(),
        style_override: None,
      })
      .expect("insert");
    let revisions_before = DocumentPackage::read(&path)?.revisions.len();
    let mut guard = gate.lock(GateHolder::DocumentService).expect("gate");
    assert!(
      DocumentPackage::read_cached_projection(&path)?.is_none(),
      "an edit must null the projection cache (preview falls back to the full read)"
    );
    let job = guard
      .begin_cache_flush()
      .expect("edited package must need a cache flush");
    drop(guard);
    let (package, wrote) = job.run();
    let wrote = wrote?;
    let mut guard = gate.lock(GateHolder::DocumentService).expect("gate");
    guard.finish_checkpoint(package, wrote);
    drop(guard);
    assert!(wrote, "the flush must reach disk");
    let flushed = DocumentPackage::read_cached_projection(&path)?.expect("flushed package must preview via the cache fast path");
    assert_eq!(flowstate_document::paragraph_text(&flushed, 0), "flush me again");
    let revisions_after = DocumentPackage::read(&path)?.revisions.len();
    assert_eq!(revisions_before, revisions_after, "a cache flush must not mint a revision");
    let mut guard = gate.lock(GateHolder::DocumentService).expect("gate");
    assert!(guard.begin_cache_flush().is_none(), "a frontier-current package must skip the flush");
    drop(guard);
    Ok(())
  }
}

#[cfg(test)]
#[path = "crdt_runtime/projection_repair_tests.rs"]
mod projection_repair_tests;
