//! §act-four M1 (generalizes act-three B.1): the recorded-inverse undo/redo
//! fast path, for every whole-document op class.
//!
//! A qualifying mass op commit records everything needed to invert itself as an
//! ORDINARY forward transaction — a [`RecordedDelta`] for each direction
//! (`undo` / `redo`), holding the doc-side [`RecordedMutation`] plus the
//! intent-exact projection patches. Undo replays the recorded inverse through
//! the same commit/publish ladder as any local write, skipping the
//! `UndoManager`'s checkout-based diff computation entirely; the vendored
//! `external_step_begin`/`external_step_finish` APIs keep the undo/redo stacks
//! bookkeeping-identical to a native `UndoManager::undo`. Redo replays the
//! forward delta; the slot ping-pongs between directions for free after the one
//! capture.
//!
//! The `RecordedDelta` here is the same structure the act-four version graph
//! (M5) records per commit — built flat now, carried into the tree unchanged.
//!
//! Covered op classes (the whole-document ops):
//! - `DeleteRange` (cross-paragraph mass delete) — undo restores the verbatim
//!   `slice_delta` + object containers + retired paragraph records (original
//!   ids); redo re-deletes. Reprojects via exact pre-recorded patches.
//! - `SetParagraphStyles` (select-all restyle) — each direction re-marks the
//!   target boundaries with the corresponding style; position-stable (marks
//!   never shift text). Reprojects via exact pre-recorded patches.
//! - `ReplaceMatches` (mass replace-all) — undo restores each match's original
//!   rich content (captured pre-op), redo re-applies the replacements (captured
//!   post-op); doc-side rich splices with post-position bookkeeping; both
//!   directions reproject through the regional derive ladder.
//!
//! Fidelity contract (100% bar): the fast path runs ONLY when the doc frontier
//! still equals the capture frontier (ANY interleaving commit/import/repair/
//! checkout fails this), the top stack span matches exactly (a group merge
//! changes it → declined), and the stack tracks no interleaved remote diff.
//! Anything else falls through to the checkout-based slow path unchanged.
//! Kill switch: `FLOWSTATE_DISABLE_FAST_UNDO=1`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use flowstate_document::{
  Block, BlockId, InputBlock, MARK_PARAGRAPH_STYLE, ParagraphId, ProjectionPatch, ProjectionStructuralBlock, block_ix_for_paragraph,
  input_block_from_block, input_paragraph_from_document_range, loro_schema::body_text, paragraph_text, paragraph_text_len,
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use loro::{CounterSpan, TextDelta, UndoOrRedo, cursor::PosType};

use super::commit::ResolvedPlan;
use super::resolve::ResolvedTextPosition;
use crate::crdt_runtime::{
  CrdtRuntime, ProjectionInvalidation, RuntimeEvent, delete_projection_paragraph_metadata, paragraph_style_value, projection_text_delta,
  prune_orphaned_body_object_blocks, repair_paragraph_metadata_after_stable_split, restore_input_object_block_containers,
  sentinel_protected_delete_range,
};

/// Minimum deleted-range size (unicode chars) worth a `DeleteRange` capture.
const MIN_CAPTURE_CHARS: usize = 2048;

/// Minimum target count worth a `SetParagraphStyles` capture — a MASS restyle
/// (the select-all class). A handful of paragraphs is already cheap via the
/// checkout path and the capture isn't free.
const MIN_RESTYLE_TARGETS: usize = 8;

/// Kill switch: `FLOWSTATE_DISABLE_FAST_UNDO=1` reverts every undo/redo to the
/// checkout-based slow path.
fn fast_undo_disabled() -> bool {
  static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *DISABLED.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_FAST_UNDO").is_some())
}

/// A restored paragraph boundary: the absolute body-unicode index of its
/// leading `\n` plus the ORIGINAL durable identities of the paragraph that
/// follows it.
struct RestoredBoundary {
  unicode: usize,
  paragraph_id: ParagraphId,
  block_id: BlockId,
}

/// A restored object block: the absolute body-unicode index of its U+FFFC
/// placeholder plus the ORIGINAL block id and captured content.
struct RestoredObject {
  unicode: usize,
  block_id: BlockId,
  input: InputBlock,
}

/// The doc-side replay for one direction of one op — the mutation applied to
/// Loro before commit.
enum RecordedMutation {
  /// Restore a previously-deleted body range verbatim (undo of a delete):
  /// bulk text+marks delta, object registry containers, retired paragraph
  /// records with original ids.
  RestoreRange {
    clamped_start: usize,
    restore_delta: Vec<TextDelta>,
    boundaries: Vec<RestoredBoundary>,
    objects: Vec<RestoredObject>,
  },
  /// Delete a body range (redo of a delete): the same mutation sequence as the
  /// original `DeleteRange` execution, driven from recorded data.
  DeleteRange {
    clamped_start: usize,
    clamped_len: usize,
    prune_objects: bool,
    boundaries: Vec<RestoredBoundary>,
  },
  /// Re-mark paragraph boundaries with the given style values (restyle in
  /// either direction). `(boundary_unicode, style_value)`. Position-stable.
  MarkParagraphStyles { targets: Vec<(usize, i64)> },
  /// Splice a set of ranges, each replacing `[start, start+delete_len)` with a
  /// captured rich delta (text+marks). Processed in DESCENDING start order so
  /// no earlier splice shifts a later one. Covers replace-all in either
  /// direction (restore originals / re-apply replacements).
  SpliceRichRanges { splices: Vec<RichSplice> },
  /// §act-twelve A12.2.2: (re)create a paragraph boundary — insert the `\n`,
  /// mark its paragraph style, strip absorbed run-style keys from the
  /// sentinel, and (re)write the paragraph's durable records with the
  /// ORIGINAL ids. The exact `SplitParagraph` executor sequence; also the
  /// exact inverse of a `JoinParagraphs`.
  CreateBoundary {
    at: usize,
    style_value: i64,
    paragraph_id: ParagraphId,
    block_id: BlockId,
  },
  /// §act-twelve A12.2.2: re-apply captured run styles per span via the
  /// `mark_run_styles` law (clear the four run keys, re-mark the set ones).
  /// Mark-only — never touches text identity, so replay cannot interleave
  /// with concurrent remote text. Spans are absolute body-unicode.
  MarkRunRanges { spans: Vec<(usize, usize, flowstate_document::RunStyles)> },
}

/// One rich splice: replace `[start, start+delete_len)` with `insert` (a
/// `slice_delta`-shaped rich content run — text + marks).
struct RichSplice {
  start: usize,
  delete_len: usize,
  insert: Vec<TextDelta>,
}

/// How a direction reprojects after its doc-side mutation.
enum Reproject {
  /// Apply these exact intent-synthesized patches to the maintained
  /// projection — `O(change)`, no readback. §perf-heaven T8.19: `Arc<[…]>` so the
  /// hot undo/redo shares the (possibly mass-op-sized) set with the event.
  Patches(Arc<[ProjectionPatch]>),
  /// Run the standard post-commit derive ladder (regional / ranged readback
  /// off the committed doc). For classes whose exact patches are complex to
  /// pre-record; still `O(change)` (regional), just via the proven remote
  /// derive path.
  Derive,
}

/// A recorded, replayable mutation for ONE direction (forward or inverse) of a
/// committed op: the doc-side mutation plus how it reprojects and the body
/// invalidation extent for the publish ladder.
struct RecordedDelta {
  mutation: RecordedMutation,
  reproject: Reproject,
  /// §act-ten A10.2: one `(unicode_start, unicode_len)` per touched span. A
  /// single covering span made a whole-doc replace-all's undo rematerialize
  /// EVERY paragraph between the first and last match; per-span ranges keep the
  /// derive ladder proportional to the actual change set.
  invalidation_ranges: Vec<(usize, usize)>,
}

/// Everything needed to replay one committed op in either direction.
pub(crate) struct RecordedInverse {
  /// The direction the next fast step serves (flips after every fast step).
  direction: UndoOrRedo,
  /// Doc frontier that must match EXACTLY at step time (encoded).
  expected_frontier: Vec<u8>,
  /// The top stack item's counter span that must match exactly.
  expected_span: CounterSpan,
  /// §mass-op collab (oom-leads #9): remote diffs are pending in the Loro undo
  /// stacks' buffers, but this entry was REBASED through the import's net
  /// delta ([`rebase_recorded_inverse_through_remote`]) — coordinates shifted
  /// (content entries: proven hull-disjoint; mark entries: per-coordinate).
  /// The fast step may proceed despite `peek_top_span`'s conservative dirty
  /// flag: the replay content is coordinate-correct, and the buffered diffs
  /// stay valid for any later slow-path pop of deeper (uncaptured) items —
  /// kept replays are position-neutral at every buffered position, and vendor
  /// patch #22 transforms the buffers through every external-step commit
  /// besides (the native `undo_internal` mirror).
  survives_pending_remote: bool,
  undo: RecordedDelta,
  redo: RecordedDelta,
}

impl std::fmt::Debug for RecordedInverse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let reproject_kind = |delta: &RecordedDelta| match &delta.reproject {
      Reproject::Patches(patches) => format!("patches({})", patches.len()),
      Reproject::Derive => "derive".to_string(),
    };
    f.debug_struct("RecordedInverse")
      .field("direction", &self.direction)
      .field("expected_span", &self.expected_span)
      .field("undo", &reproject_kind(&self.undo))
      .field("redo", &reproject_kind(&self.redo))
      .finish_non_exhaustive()
  }
}

/// Pre-mutation capture: taken between resolve and execute, while the doc and
/// projection are still at their pre-op state. The `undo` delta is fully formed
/// here (it needs pre-op reads); the `redo` mutation is formed here too, but its
/// patches are supplied at [`finalize_capture`] (the op's own synthesized
/// patches are the redo patches).
pub(crate) struct PendingInverseCapture {
  undo: RecordedDelta,
  redo_mutation: RecordedMutation,
  redo_invalidation_ranges: Vec<(usize, usize)>,
  /// Redo reprojects via the derive ladder (rather than the op's synthesized
  /// patches) — set for classes whose exact patches are complex to pre-record.
  redo_derive: bool,
  /// For replace-all: the redo splices can only be captured AFTER the op runs
  /// (the replacement's applied rich content lives in the post-op doc). Each
  /// entry says: redo replaces `[redo_start, redo_start+redo_delete_len)` with
  /// the rich content sliced from the post-op doc at `[post_slice_start,
  /// post_slice_start+post_slice_len)`. `None` for all other classes.
  redo_replace_finalize: Option<Vec<ReplaceFinalize>>,
}

/// Deferred redo-splice capture for replace-all (see `redo_replace_finalize`).
struct ReplaceFinalize {
  redo_start: usize,
  redo_delete_len: usize,
  post_slice_start: usize,
  post_slice_len: usize,
}

/// Capture the recorded inverse of a qualifying mass op plan. Must run BEFORE
/// the op mutates the doc. Returns `None` (never an error) for anything that
/// doesn't qualify — the op itself proceeds identically either way.
pub(crate) fn capture_before_execute(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  if fast_undo_disabled() {
    return None;
  }
  match plan {
    ResolvedPlan::InsertText { .. } => capture_insert_text(plan),
    ResolvedPlan::DeleteRange { .. } => capture_delete_range(core, plan),
    ResolvedPlan::SetParagraphStyles { .. } => capture_set_paragraph_styles(core, plan),
    ResolvedPlan::ReplaceMatches { .. } => capture_replace_matches(core, plan),
    // §act-twelve A12.2.2 (capture-coverage completion): the everyday
    // structural/mark ops. An uncaptured op both takes the O(history)
    // checkout slow path on its own undo AND clears every deeper captured
    // fast path behind it — Enter, backspace-join, bold/italic and
    // single-paragraph restyle were doing exactly that to typing sessions.
    ResolvedPlan::SplitParagraph { .. } => capture_split_paragraph(plan),
    ResolvedPlan::JoinParagraphs { .. } => capture_join_paragraphs(core, plan),
    ResolvedPlan::SetMarks { .. } => capture_set_marks(core, plan),
    ResolvedPlan::SetParagraphStyle { .. } => capture_set_paragraph_style(core, plan),
    _ => None,
  }
}

/// §oom-leads #9 (keystroke undo): EVERY text insert records its inverse — the
/// keystroke is the most common op there is, and an uncaptured op both takes
/// the O(history) checkout slow path on its own undo AND clears every deeper
/// captured fast path behind it (a single typed char used to kill a recorded
/// mass-op undo). The record is O(insert) and needs no doc reads at capture:
/// undo = delete the inserted span (derive reprojection — a ranged readback of
/// one paragraph), redo = re-insert the post-op rich slice (captured at
/// finalize, so a `style_override`'s marks ride along verbatim; the op's own
/// synthesized patches replay the projection).
fn capture_insert_text(plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::InsertText { at, text, .. } = plan else {
    return None;
  };
  let len = text.chars().count();
  if len == 0 {
    return None;
  }
  let pos = at.body_unicode;
  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::DeleteRange {
        clamped_start: pos,
        clamped_len: len,
        prune_objects: false,
        boundaries: Vec::new(),
      },
      reproject: Reproject::Derive,
      invalidation_ranges: vec![(pos, len)],
    },
    // Filled at finalize from the post-op doc (see `redo_replace_finalize`).
    redo_mutation: RecordedMutation::SpliceRichRanges { splices: Vec::new() },
    redo_invalidation_ranges: vec![(pos, len)],
    redo_derive: false,
    redo_replace_finalize: Some(vec![ReplaceFinalize {
      redo_start: pos,
      redo_delete_len: 0,
      post_slice_start: pos,
      post_slice_len: len,
    }]),
  })
}

/// §oom-leads #9 (keystroke undo): backspace and small intra-paragraph deletes.
/// A single paragraph's interior can contain neither `\n` nor U+FFFC, so there
/// are no boundaries or objects to retire — the record is a verbatim rich slice
/// (undo re-inserts it) plus a re-delete, both O(len).
fn capture_intra_paragraph_delete(core: &CrdtRuntime, start: &ResolvedTextPosition, end: &ResolvedTextPosition) -> Option<PendingInverseCapture> {
  let len = end.body_unicode.saturating_sub(start.body_unicode);
  if len == 0 {
    return None;
  }
  let (clamped_start, clamped_len) = sentinel_protected_delete_range(start.body_unicode, len)?;
  if (clamped_start, clamped_len) != (start.body_unicode, len) {
    return None;
  }
  let body = body_text(core.doc());
  let restore_delta = match body.slice_delta(clamped_start, clamped_start + clamped_len, PosType::Unicode) {
    Ok(delta) => delta,
    Err(error) => {
      tracing::warn!(%error, "recorded-inverse intra-paragraph capture declined: slice_delta failed");
      return None;
    },
  };
  // Defensive: the slice must be pure interior text. A boundary or placeholder
  // here means the resolved positions and the projection disagree — decline
  // rather than record a structural restore without its metadata.
  for segment in &restore_delta {
    let TextDelta::Insert { insert, .. } = segment else {
      return None;
    };
    if insert.contains('\n') || insert.contains(flowstate_document::OBJECT_REPLACEMENT) {
      return None;
    }
  }
  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::RestoreRange {
        clamped_start,
        restore_delta,
        boundaries: Vec::new(),
        objects: Vec::new(),
      },
      reproject: Reproject::Derive,
      invalidation_ranges: vec![(clamped_start, clamped_len)],
    },
    redo_mutation: RecordedMutation::DeleteRange {
      clamped_start,
      clamped_len,
      prune_objects: false,
      boundaries: Vec::new(),
    },
    redo_invalidation_ranges: vec![(clamped_start, clamped_len)],
    redo_derive: false,
    redo_replace_finalize: None,
  })
}

/// Minimum match count worth a `ReplaceMatches` capture — a MASS replace-all.
const MIN_REPLACE_MATCHES: usize = 8;

/// Replace-all: undo restores each match's original rich content; redo
/// re-applies the replacement. The doc-side mutation is a set of rich splices;
/// both directions reproject via the derive ladder (their exact patches are the
/// per-paragraph ranged readback the ladder already produces). Positions:
/// matches arrive DESCENDING by start; the op replaces `[s_i, s_i+orig_len_i)`
/// with the replacement at the original `s_i` (descending keeps `s_i` stable).
/// Undo operates on the post-op doc, where match `i`'s replacement sits at
/// `q_i = s_i + Σ_{s_j < s_i}(repl_len − orig_len_j)`.
fn capture_replace_matches(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::ReplaceMatches { matches, replacement } = plan else {
    return None;
  };
  if matches.len() < MIN_REPLACE_MATCHES {
    return None;
  }
  let repl_len = replacement.chars().count();
  let body = body_text(core.doc());

  // Post-position q for each original start, via an ascending cumulative shift.
  let mut q_by_start: std::collections::HashMap<usize, usize> = std::collections::HashMap::with_capacity(matches.len());
  let mut ascending: Vec<(usize, usize)> = matches.iter().map(|(start, end, _)| (start.body_unicode, end.body_unicode - start.body_unicode)).collect();
  ascending.sort_by_key(|(start, _)| *start);
  let mut shift: isize = 0;
  for (start, orig_len) in &ascending {
    let q = usize::try_from(*start as isize + shift).ok()?;
    if q_by_start.insert(*start, q).is_some() {
      return None; // duplicate start — should be pruned already; decline.
    }
    shift += repl_len as isize - *orig_len as isize;
  }

  let mut undo_splices = Vec::with_capacity(matches.len());
  let mut redo_finalize = Vec::with_capacity(matches.len());
  let mut undo_ranges = Vec::with_capacity(matches.len());
  let mut redo_ranges = Vec::with_capacity(matches.len());
  for (start, end, _styles) in matches {
    let s = start.body_unicode;
    let orig_len = end.body_unicode - s;
    let q = *q_by_start.get(&s)?;
    // Original rich content (text + marks), captured pre-op — the undo insert.
    let orig_delta = body.slice_delta(s, s + orig_len, PosType::Unicode).ok()?;
    undo_splices.push(RichSplice {
      start: q,
      delete_len: repl_len,
      insert: orig_delta,
    });
    redo_finalize.push(ReplaceFinalize {
      redo_start: s,
      redo_delete_len: orig_len,
      post_slice_start: q,
      post_slice_len: repl_len,
    });
    // §act-ten A10.2: one invalidation range PER MATCH — the readback then
    // touches only the matched paragraphs, not the whole covered span.
    undo_ranges.push((q, repl_len.max(1)));
    redo_ranges.push((s, orig_len.max(1)));
  }
  undo_ranges.sort_unstable();
  redo_ranges.sort_unstable();

  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::SpliceRichRanges { splices: undo_splices },
      reproject: Reproject::Derive,
      invalidation_ranges: undo_ranges,
    },
    // Filled at finalize from the post-op doc (see `redo_replace_finalize`).
    redo_mutation: RecordedMutation::SpliceRichRanges { splices: Vec::new() },
    redo_invalidation_ranges: redo_ranges,
    redo_derive: true,
    redo_replace_finalize: Some(redo_finalize),
  })
}

/// `SetParagraphStyles` (select-all restyle): each direction re-marks the target
/// boundaries. Undo → prior styles; redo → new style. Position-stable.
fn capture_set_paragraph_styles(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::SetParagraphStyles { targets, style } = plan else {
    return None;
  };
  if targets.len() < MIN_RESTYLE_TARGETS {
    return None;
  }
  let projection = core.projection_ref();
  let after_value = paragraph_style_value(*style);
  let rows = flowstate_document::paragraph_block_rows(projection);

  let mut undo_marks: Vec<(usize, i64)> = Vec::with_capacity(targets.len());
  let mut redo_marks: Vec<(usize, i64)> = Vec::with_capacity(targets.len());
  let mut undo_patches: Vec<ProjectionPatch> = Vec::with_capacity(targets.len());
  let mut boundary_ranges: Vec<(usize, usize)> = Vec::with_capacity(targets.len());
  for (paragraph, paragraph_ix, boundary) in targets {
    let before_style = projection.paragraphs.get(*paragraph_ix).map(|paragraph| paragraph.style)?;
    let before_value = paragraph_style_value(before_style);
    undo_marks.push((*boundary, before_value));
    redo_marks.push((*boundary, after_value));
    let &row = rows.get(*paragraph_ix)?;
    undo_patches.push(ProjectionPatch::ParagraphStyle {
      block_id: *projection.ids.block_ids.get(row)?,
      paragraph_id: *paragraph,
      row_hint: row,
      style: before_style,
    });
    boundary_ranges.push((*boundary, 1));
  }
  boundary_ranges.sort_unstable();

  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::MarkParagraphStyles { targets: undo_marks },
      reproject: Reproject::Patches(undo_patches.into()),
      invalidation_ranges: boundary_ranges.clone(),
    },
    redo_mutation: RecordedMutation::MarkParagraphStyles { targets: redo_marks },
    redo_invalidation_ranges: boundary_ranges,
    redo_derive: false,
    redo_replace_finalize: None,
  })
}

/// §act-twelve A12.2.2: `SplitParagraph` (Enter). The plan carries the
/// PRE-MINTED new paragraph/block ids, so both directions are fully formable
/// before execution: undo deletes the boundary and retires the new records
/// (the exact join sequence); redo re-creates the boundary with the SAME ids
/// (identity preservation — collab cursors and records survive the cycle).
fn capture_split_paragraph(plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::SplitParagraph {
    at,
    inherited_style,
    new_paragraph,
    new_block,
  } = plan
  else {
    return None;
  };
  let pos = at.body_unicode;
  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::DeleteRange {
        clamped_start: pos,
        clamped_len: 1,
        prune_objects: false,
        boundaries: vec![RestoredBoundary {
          unicode: pos,
          paragraph_id: *new_paragraph,
          block_id: *new_block,
        }],
      },
      reproject: Reproject::Derive,
      invalidation_ranges: vec![(pos, 1)],
    },
    redo_mutation: RecordedMutation::CreateBoundary {
      at: pos,
      style_value: paragraph_style_value(*inherited_style),
      paragraph_id: *new_paragraph,
      block_id: *new_block,
    },
    redo_invalidation_ranges: vec![(pos, 1)],
    redo_derive: true,
    redo_replace_finalize: None,
  })
}

/// §act-twelve A12.2.2: `JoinParagraphs` (Backspace at paragraph start). Undo
/// re-creates the boundary with the SECOND paragraph's prior style and its
/// ORIGINAL ids; redo re-deletes it. Mirrors `join_projection_paragraphs`'
/// own guards so a capture never outlives an executor no-op (the executor
/// bails on those shapes, which discards the pending capture anyway).
fn capture_join_paragraphs(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::JoinParagraphs { second, first_ix, .. } = plan else {
    return None;
  };
  let projection = core.projection_ref();
  let second_ix = first_ix + 1;
  if projection.ids.paragraph_ids.get(second_ix) != Some(second) {
    return None;
  }
  let second_style = projection.paragraphs.get(second_ix)?.style;
  let second_block_ix = block_ix_for_paragraph(projection, second_ix)?;
  let second_block = projection.ids.block_ids.get(second_block_ix).copied()?;
  let boundary = crate::crdt_runtime::paragraph_boundary_loro_unicode_index(core.doc(), projection, second_ix);
  if body_text(core.doc()).char_at(boundary) != Ok('\n') {
    return None;
  }
  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::CreateBoundary {
        at: boundary,
        style_value: paragraph_style_value(second_style),
        paragraph_id: *second,
        block_id: second_block,
      },
      reproject: Reproject::Derive,
      invalidation_ranges: vec![(boundary, 1)],
    },
    redo_mutation: RecordedMutation::DeleteRange {
      clamped_start: boundary,
      clamped_len: 1,
      prune_objects: false,
      boundaries: vec![RestoredBoundary {
        unicode: boundary,
        paragraph_id: *second,
        block_id: second_block,
      }],
    },
    redo_invalidation_ranges: vec![(boundary, 1)],
    redo_derive: true,
    redo_replace_finalize: None,
  })
}

/// §act-twelve A12.2.2: `SetMarks` (bold/italic/highlight over a range). The
/// executor's law REPLACES the four run-style keys over the range
/// (`mark_run_styles`: unmark all four, re-mark the set ones), so undo =
/// re-apply the captured PRIOR per-span styles through the same law, redo =
/// one span with the op's styles. Truth source is `slice_delta` (doc-level,
/// includes boundary chars' actual run keys); non-run attrs are ignored by
/// the mapper, so paragraph-style marks are never touched.
fn capture_set_marks(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::SetMarks { start, end, styles } = plan else {
    return None;
  };
  let s = start.body_unicode;
  let e = end.body_unicode;
  if e <= s {
    return None;
  }
  let body = body_text(core.doc());
  let prior = match body.slice_delta(s, e, PosType::Unicode) {
    Ok(delta) => delta,
    Err(error) => {
      tracing::warn!(%error, "recorded-inverse set-marks capture declined: slice_delta failed");
      return None;
    },
  };
  let mut spans: Vec<(usize, usize, flowstate_document::RunStyles)> = Vec::new();
  let mut cursor = s;
  for segment in &prior {
    let TextDelta::Insert { insert, attributes } = segment else {
      return None;
    };
    let len = insert.chars().count();
    if len == 0 {
      continue;
    }
    let span_styles = run_styles_from_mark_attrs(attributes.as_ref());
    match spans.last_mut() {
      Some((last_start, last_len, last_styles)) if *last_styles == span_styles && *last_start + *last_len == cursor => {
        *last_len += len;
      },
      _ => spans.push((cursor, len, span_styles)),
    }
    cursor += len;
  }
  if spans.is_empty() {
    return None;
  }
  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::MarkRunRanges { spans },
      reproject: Reproject::Derive,
      invalidation_ranges: vec![(s, e - s)],
    },
    redo_mutation: RecordedMutation::MarkRunRanges {
      spans: vec![(s, e - s, *styles)],
    },
    redo_invalidation_ranges: vec![(s, e - s)],
    redo_derive: true,
    redo_replace_finalize: None,
  })
}

/// The four run-style keys `mark_run_styles` manages, mapped back from Loro
/// mark attributes — the exact inverse of that writer (other keys, e.g.
/// paragraph style or vert-align, are deliberately ignored: `SetMarks` never
/// touches them, so neither may its undo).
fn run_styles_from_mark_attrs<S: std::hash::BuildHasher>(attrs: Option<&std::collections::HashMap<String, loro::LoroValue, S>>) -> flowstate_document::RunStyles {
  use flowstate_document::loro_schema::{MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH};
  let mut styles = flowstate_document::RunStyles::default();
  let Some(attrs) = attrs else {
    return styles;
  };
  if let Some(loro::LoroValue::I64(slot)) = attrs.get(MARK_RUN_SEMANTIC_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.semantic = flowstate_document::RunSemanticStyle::Custom(slot);
  }
  if let Some(loro::LoroValue::I64(slot)) = attrs.get(MARK_HIGHLIGHT_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.highlight = Some(flowstate_document::HighlightStyle::Custom(slot));
  }
  if matches!(attrs.get(MARK_DIRECT_UNDERLINE), Some(loro::LoroValue::Bool(true))) {
    styles.direct_underline = true;
  }
  if matches!(attrs.get(MARK_STRIKETHROUGH), Some(loro::LoroValue::Bool(true))) {
    styles.strikethrough = true;
  }
  styles
}

/// §act-twelve A12.2.2: single-paragraph `SetParagraphStyle` — the plural
/// capture's law with one target (no minimum gate: the capture is O(1)).
fn capture_set_paragraph_style(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::SetParagraphStyle { paragraph, paragraph_ix, style } = plan else {
    return None;
  };
  let projection = core.projection_ref();
  if projection.ids.paragraph_ids.get(*paragraph_ix) != Some(paragraph) {
    return None;
  }
  let before_style = projection.paragraphs.get(*paragraph_ix)?.style;
  let boundary = crate::crdt_runtime::paragraph_boundary_loro_unicode_index(core.doc(), projection, *paragraph_ix);
  let rows = flowstate_document::paragraph_block_rows(projection);
  let &row = rows.get(*paragraph_ix)?;
  let undo_patch = ProjectionPatch::ParagraphStyle {
    block_id: *projection.ids.block_ids.get(row)?,
    paragraph_id: *paragraph,
    row_hint: row,
    style: before_style,
  };
  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::MarkParagraphStyles {
        targets: vec![(boundary, paragraph_style_value(before_style))],
      },
      reproject: Reproject::Patches(vec![undo_patch].into()),
      invalidation_ranges: vec![(boundary, 1)],
    },
    redo_mutation: RecordedMutation::MarkParagraphStyles {
      targets: vec![(boundary, paragraph_style_value(*style))],
    },
    redo_invalidation_ranges: vec![(boundary, 1)],
    redo_derive: false,
    redo_replace_finalize: None,
  })
}

/// Cross-paragraph mass `DeleteRange`: undo restores verbatim, redo re-deletes.
fn capture_delete_range(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  let ResolvedPlan::DeleteRange { start, end } = plan else {
    return None;
  };
  let len = end.body_unicode.saturating_sub(start.body_unicode);
  // §oom-leads #9: intra-paragraph deletes (backspace, word/selection deletes)
  // capture at ANY size — the record is O(len) with no structural bookkeeping.
  if start.paragraph_ix == end.paragraph_ix {
    return capture_intra_paragraph_delete(core, start, end);
  }
  if len < MIN_CAPTURE_CHARS {
    return None;
  }
  let (clamped_start, clamped_len) = sentinel_protected_delete_range(start.body_unicode, len)?;
  if (clamped_start, clamped_len) != (start.body_unicode, len) {
    // Sentinel-clamped deletes shift the whole geometry; rare — slow path.
    return None;
  }
  let projection = core.projection_ref();
  let first_block_ix = block_ix_for_paragraph(projection, start.paragraph_ix)?;
  let last_block_ix = block_ix_for_paragraph(projection, end.paragraph_ix)?;

  // The rows the delete removes: everything strictly after the first
  // paragraph's row through the last paragraph's row (which merges into the
  // first). Captured with their ORIGINAL ids so undo restores identity.
  let mut removed_rows = Vec::with_capacity(last_block_ix - first_block_ix);
  let mut removed_paragraphs: Vec<(ParagraphId, BlockId)> = Vec::new();
  let mut removed_objects: Vec<(BlockId, InputBlock)> = Vec::new();
  let mut paragraph_ix = start.paragraph_ix;
  for block_ix in (first_block_ix + 1)..=last_block_ix {
    let block = projection.blocks.get(block_ix)?;
    let block_id = projection.ids.block_ids.get(block_ix).copied()?;
    match block {
      Block::Table(_) => {
        // Durable row/column/cell ids cannot be re-minted losslessly here.
        return None;
      },
      Block::Paragraph(_) => {
        paragraph_ix += 1;
        let paragraph_id = projection.ids.paragraph_ids.get(paragraph_ix).copied()?;
        removed_paragraphs.push((paragraph_id, block_id));
        removed_rows.push(ProjectionStructuralBlock {
          block_id,
          paragraph_id: Some(paragraph_id),
          block: InputBlock::Paragraph(input_paragraph_from_document_range(projection, paragraph_ix, 0..usize::MAX)),
        });
      },
      Block::Image(_) | Block::Equation(_) => {
        let input = input_block_from_block(block);
        removed_objects.push((block_id, input.clone()));
        removed_rows.push(ProjectionStructuralBlock {
          block_id,
          paragraph_id: None,
          block: input,
        });
      },
    }
  }
  if paragraph_ix != end.paragraph_ix {
    tracing::warn!(
      start = start.paragraph_ix,
      end = end.paragraph_ix,
      walked = paragraph_ix,
      "recorded-inverse capture declined: paragraph row walk mismatch"
    );
    return None;
  }

  // Verbatim capture of the deleted range straight from Loro.
  let body = body_text(core.doc());
  let restore_delta = match body.slice_delta(clamped_start, clamped_start + clamped_len, PosType::Unicode) {
    Ok(delta) => delta,
    Err(error) => {
      tracing::warn!(%error, "recorded-inverse capture declined: slice_delta failed");
      return None;
    },
  };

  // Pair each deleted `\n`/U+FFFC with the captured rows, in document order.
  let mut boundaries = Vec::with_capacity(removed_paragraphs.len());
  let mut objects = Vec::with_capacity(removed_objects.len());
  {
    let mut paragraph_iter = removed_paragraphs.into_iter();
    let mut object_iter = removed_objects.into_iter();
    let mut offset = clamped_start;
    for segment in &restore_delta {
      let TextDelta::Insert { insert, .. } = segment else {
        tracing::warn!("recorded-inverse capture declined: non-insert slice segment");
        return None;
      };
      for ch in insert.chars() {
        match ch {
          '\n' => {
            let Some((paragraph_id, block_id)) = paragraph_iter.next() else {
              tracing::warn!("recorded-inverse capture declined: more boundaries than retired paragraphs");
              return None;
            };
            boundaries.push(RestoredBoundary {
              unicode: offset,
              paragraph_id,
              block_id,
            });
          },
          flowstate_document::OBJECT_REPLACEMENT => {
            let Some((block_id, input)) = object_iter.next() else {
              tracing::warn!("recorded-inverse capture declined: more placeholders than captured objects");
              return None;
            };
            objects.push(RestoredObject {
              unicode: offset,
              block_id,
              input,
            });
          },
          _ => {},
        }
        offset += 1;
      }
    }
    if paragraph_iter.next().is_some() || object_iter.next().is_some() {
      tracing::warn!("recorded-inverse capture declined: retired rows not covered by deleted text");
      return None;
    }
  }

  // Undo-direction patches: restore the first paragraph's original content and
  // re-insert the removed rows (original ids) before the surviving successor.
  let first_para_text = paragraph_text(projection, start.paragraph_ix);
  if first_para_text.len() < start.byte {
    return None;
  }
  let last_para_len = projection.paragraphs.get(end.paragraph_ix).map(paragraph_text_len)?;
  if last_para_len < end.byte {
    return None;
  }
  let merged_len = start.byte + (last_para_len - end.byte);
  let mut undo_patches = vec![ProjectionPatch::ParagraphText {
    block_id: projection.ids.block_ids.get(first_block_ix).copied()?,
    paragraph_id: projection.ids.paragraph_ids.get(start.paragraph_ix).copied()?,
    row_hint: first_block_ix,
    new: input_paragraph_from_document_range(projection, start.paragraph_ix, 0..usize::MAX),
    delta_utf8: projection_text_delta(start.byte, merged_len - start.byte, first_para_text.len() - start.byte, 0),
  }];
  let redo_boundaries = boundaries.iter().map(|b| RestoredBoundary { unicode: b.unicode, paragraph_id: b.paragraph_id, block_id: b.block_id }).collect();
  let prune_objects = !objects.is_empty();
  if !removed_rows.is_empty() {
    undo_patches.push(ProjectionPatch::InsertBlocks {
      before: projection.ids.block_ids.get(last_block_ix + 1).copied(),
      row_hint: first_block_ix + 1,
      blocks: removed_rows,
    });
  }

  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::RestoreRange {
        clamped_start,
        restore_delta,
        boundaries,
        objects,
      },
      reproject: Reproject::Patches(undo_patches.into()),
      invalidation_ranges: vec![(clamped_start, clamped_len)],
    },
    redo_mutation: RecordedMutation::DeleteRange {
      clamped_start,
      clamped_len,
      prune_objects,
      boundaries: redo_boundaries,
    },
    redo_invalidation_ranges: vec![(clamped_start, clamped_len)],
    redo_derive: false,
    redo_replace_finalize: None,
  })
}

/// Post-commit half of the capture: arm the slot with the committed op's
/// counter span, the post-commit frontier, and the op's own synthesized patches
/// (the verbatim redo patches).
pub(crate) fn finalize_capture(core: &mut CrdtRuntime, pending: PendingInverseCapture, op_span: CounterSpan, redo_patches: &[ProjectionPatch]) {
  let expected_frontier = core.doc().state_frontiers().encode();
  let redo_reproject = if pending.redo_derive {
    Reproject::Derive
  } else {
    Reproject::Patches(Arc::from(redo_patches))
  };
  // Replace-all: the redo splices can only be captured now, from the POST-op
  // doc where the replacements' applied rich content lives. If any slice fails,
  // drop the whole record (undo/redo fall back to the checkout path) rather
  // than arm a partial one.
  let redo_mutation = match pending.redo_replace_finalize {
    Some(finalize) => {
      let body = body_text(core.doc());
      let mut splices = Vec::with_capacity(finalize.len());
      let mut ok = true;
      for entry in finalize {
        match body.slice_delta(entry.post_slice_start, entry.post_slice_start + entry.post_slice_len, PosType::Unicode) {
          Ok(insert) => splices.push(RichSplice {
            start: entry.redo_start,
            delete_len: entry.redo_delete_len,
            insert,
          }),
          Err(error) => {
            tracing::warn!(%error, "recorded-inverse replace-all redo finalize declined: slice_delta failed");
            ok = false;
            break;
          },
        }
      }
      if !ok {
        // A partial capture is worse than none: this forward edit is NOT
        // fast-undoable, and any older stacked inverse is now unreachable behind
        // it (the Loro top span no longer matches), so drop the whole cache.
        core.clear_recorded_inverse();
        return;
      }
      RecordedMutation::SpliceRichRanges { splices }
    },
    None => pending.redo_mutation,
  };
  // §act-five P3-deep: a forward edit PUSHES its inverse onto the undo stack and
  // invalidates the redo stack (the redo timeline is now unreachable). Consecutive
  // pushes let a run of undos each replay `O(change)`.
  core.recorded_redo_stack().clear();
  core.recorded_undo_stack().push(RecordedInverse {
    direction: UndoOrRedo::Undo,
    expected_frontier,
    expected_span: op_span,
    survives_pending_remote: false,
    undo: pending.undo,
    redo: RecordedDelta {
      mutation: redo_mutation,
      reproject: redo_reproject,
      invalidation_ranges: pending.redo_invalidation_ranges,
    },
  });
  // §oom-leads #9: keystrokes capture too now, so a long typing session grows
  // the stack one entry per char. Bound it: undos deeper than the cap fall to
  // the slow path exactly as an uncaptured op would (the Loro stack has its
  // own `max_stack_size` for the same reason).
  const MAX_RECORDED_ENTRIES: usize = 512;
  let stack = core.recorded_undo_stack();
  if stack.len() > MAX_RECORDED_ENTRIES {
    let excess = stack.len() - MAX_RECORDED_ENTRIES;
    stack.drain(..excess);
  }
}

/// §act-ten A10.2: re-arm the recorded fast-path stacks across a known-INERT
/// frontier advance (a "meta"-origin metadata/revision commit, e.g. the
/// autosave checkpoint). Those commits touch only meta containers — the body
/// state the recorded inverses replay against is unchanged — but they advance
/// the frontier, which the strict `expected_frontier` equality at step time
/// would read as "concurrent change, drop everything": the exact bug that made
/// fast undo dead in the field (anyone pausing >900ms before Ctrl-Z hit the
/// O(doc) slow path). Only the TOPS need re-arming — the step-time check reads
/// the active top, and each fast step re-arms the next entry as it goes. The
/// `expected_span` fail-safe still validates against the Loro `UndoManager`'s
/// real top (meta commits are origin-excluded there, so it is unchanged).
pub(crate) fn rearm_recorded_inverse_frontier(core: &mut CrdtRuntime) {
  let frontier = core.doc().state_frontiers().encode();
  if let Some(top) = core.recorded_undo_stack().last_mut() {
    top.expected_frontier.clone_from(&frontier);
  }
  if let Some(top) = core.recorded_redo_stack().last_mut() {
    top.expected_frontier = frontier;
  }
}

/// §mass-op collab (oom-leads #9): the lowest body-unicode coordinate a
/// recorded direction addresses — mutation positions plus invalidation-range
/// starts. `None` for a direction with no coordinates at all (nothing to
/// anchor a disjointness proof to — callers must clear).
fn delta_min_coordinate(delta: &RecordedDelta) -> Option<usize> {
  let mut min: Option<usize> = None;
  let mut fold = |value: usize| min = Some(min.map_or(value, |current| current.min(value)));
  match &delta.mutation {
    RecordedMutation::RestoreRange {
      clamped_start,
      boundaries,
      objects,
      ..
    } => {
      fold(*clamped_start);
      for boundary in boundaries {
        fold(boundary.unicode);
      }
      for object in objects {
        fold(object.unicode);
      }
    },
    RecordedMutation::DeleteRange {
      clamped_start, boundaries, ..
    } => {
      fold(*clamped_start);
      for boundary in boundaries {
        fold(boundary.unicode);
      }
    },
    RecordedMutation::MarkParagraphStyles { targets } => {
      for (unicode, _) in targets {
        fold(*unicode);
      }
    },
    RecordedMutation::SpliceRichRanges { splices } => {
      for splice in splices {
        fold(splice.start);
      }
    },
    RecordedMutation::CreateBoundary { at, .. } => fold(*at),
    RecordedMutation::MarkRunRanges { spans } => {
      for (start, _, _) in spans {
        fold(*start);
      }
    },
  }
  for (start, _) in &delta.invalidation_ranges {
    fold(*start);
  }
  min
}

/// Shift every body-unicode coordinate in one recorded direction by `by`.
/// Returns `false` on arithmetic failure (callers clear the cache).
fn shift_delta_coordinates(delta: &mut RecordedDelta, by: isize) -> bool {
  let shift = |value: &mut usize| -> bool {
    match value.checked_add_signed(by) {
      Some(next) => {
        *value = next;
        true
      },
      None => false,
    }
  };
  let mutation_ok = match &mut delta.mutation {
    RecordedMutation::RestoreRange {
      clamped_start,
      boundaries,
      objects,
      ..
    } => shift(clamped_start) && boundaries.iter_mut().all(|b| shift(&mut b.unicode)) && objects.iter_mut().all(|o| shift(&mut o.unicode)),
    RecordedMutation::DeleteRange {
      clamped_start, boundaries, ..
    } => shift(clamped_start) && boundaries.iter_mut().all(|b| shift(&mut b.unicode)),
    RecordedMutation::MarkParagraphStyles { targets } => targets.iter_mut().all(|(unicode, _)| shift(unicode)),
    RecordedMutation::SpliceRichRanges { splices } => splices.iter_mut().all(|splice| shift(&mut splice.start)),
    RecordedMutation::CreateBoundary { at, .. } => shift(at),
    RecordedMutation::MarkRunRanges { spans } => spans.iter_mut().all(|(start, _, _)| shift(start)),
  };
  mutation_ok && delta.invalidation_ranges.iter_mut().all(|(start, _)| shift(start))
}

/// The import's shape, precomputed once for the per-entry rebase laws.
struct RemoteNetLaw {
  /// Highest PRE-space position any net insert lands at.
  max_insert_pos: Option<usize>,
  /// Highest PRE-space position any net delete ends at.
  max_delete_end: Option<usize>,
  /// Net length change (Σ inserts − Σ deletes) — the coordinate shift for an
  /// entry whose hull sits above every remote op.
  total_delta: isize,
  /// Deleted PRE-space ranges, merged and ordered (for per-target drops).
  deleted_ranges: Vec<(usize, usize)>,
  /// End of the last `changed_text_ranges` entry (POST space) — remote
  /// rich/mark changes ride the invalidation without touching the net, and a
  /// CONTENT replay (restore/splice re-applies captured text+marks) would
  /// clobber one; they must end at/before a content entry's hull. Mark-only
  /// replays are exempt: re-marking a surviving boundary char with the
  /// recorded value is exactly what the native slow path replays too.
  max_changed_end_post: Option<usize>,
}

impl RemoteNetLaw {
  fn from_net(net: &crate::crdt_runtime::import_delta::NetBodyDelta, changed_body_ranges_post: &[(usize, usize)]) -> Self {
    use crate::crdt_runtime::import_delta::NetOp;
    let mut max_insert_pos = None;
    let mut max_delete_end = None;
    let mut total_delta = 0_isize;
    let mut old_pos = 0_usize;
    for op in &net.ops {
      match op {
        NetOp::Retain(n) => old_pos += n,
        NetOp::Insert { len, .. } => {
          max_insert_pos = Some(max_insert_pos.map_or(old_pos, |current: usize| current.max(old_pos)));
          total_delta += *len as isize;
        },
        NetOp::Delete(n) => {
          old_pos += n;
          max_delete_end = Some(max_delete_end.map_or(old_pos, |current: usize| current.max(old_pos)));
          total_delta -= *n as isize;
        },
      }
    }
    Self {
      max_insert_pos,
      max_delete_end,
      total_delta,
      deleted_ranges: net.deleted_pre_ranges(),
      max_changed_end_post: changed_body_ranges_post.iter().map(|(start, len)| start + len).max(),
    }
  }

  /// The v1 hull law against ONE entry's hull floor `lo` (PRE space): every
  /// remote insert strictly before it (an insert AT the floor is ambiguous —
  /// the CRDT parks a remote edit whose original neighbors were locally
  /// deleted at the deletion point, i.e. exactly where a recorded restore
  /// would replay, and the native undo would interleave it INSIDE the restored
  /// range); deletes and rich changes may END exactly at the floor.
  fn before_hull(&self, lo: usize) -> bool {
    if !(self.max_insert_pos.is_none_or(|pos| pos < lo) && self.max_delete_end.is_none_or(|end| end <= lo)) {
      return false;
    }
    let Some(lo_post) = lo.checked_add_signed(self.total_delta) else {
      return false;
    };
    self.max_changed_end_post.is_none_or(|end| end <= lo_post)
  }

  /// Whether PRE-space `position`'s char was deleted by the import.
  fn position_deleted(&self, position: usize) -> bool {
    let ix = self.deleted_ranges.partition_point(|(start, _)| *start <= position);
    ix > 0 && {
      let (start, len) = self.deleted_ranges[ix - 1];
      position < start + len
    }
  }
}

/// Per-coordinate rebase of one mark-only direction: `shift_positions` tracks
/// each targeted boundary char exactly (net-delta space math — survivors never
/// land inside inserted runs); a target whose boundary char the import DELETED
/// is dropped, which is what the native slow path's remote-diff transform
/// would do. (Re-marking a KEPT boundary with the recorded value even when a
/// remote rich change touched it is also native semantics — the slow path's
/// transform shifts inverse-mark positions, never removes them.) Returns
/// `(ok, any_dropped)`; `ok == false` truncates the entry — unsorted targets
/// (never produced by capture) or all targets dropped (an empty replay can't
/// drive the external-step protocol).
fn rebase_mark_delta(delta: &mut RecordedDelta, net: &crate::crdt_runtime::import_delta::NetBodyDelta, law: &RemoteNetLaw) -> (bool, bool) {
  let RecordedMutation::MarkParagraphStyles { targets } = &mut delta.mutation else {
    return (false, false);
  };
  if !targets.windows(2).all(|pair| pair[0].0 <= pair[1].0) {
    return (false, false);
  }
  let positions: Vec<usize> = targets.iter().map(|(unicode, _)| *unicode).collect();
  let shifted = net.shift_positions(&positions);
  let mut kept: Vec<(usize, i64)> = Vec::with_capacity(targets.len());
  for (ix, (position, value)) in targets.iter().enumerate() {
    if law.position_deleted(*position) {
      continue;
    }
    kept.push((shifted[ix], *value));
  }
  if kept.is_empty() {
    return (false, false);
  }
  let dropped = kept.len() < targets.len();
  // Mark captures record one `(boundary, 1)` invalidation range per target —
  // rebuild the set from the kept, shifted boundaries.
  delta.invalidation_ranges = kept.iter().map(|(unicode, _)| (*unicode, 1)).collect();
  *targets = kept;
  (true, dropped)
}

/// Rebase ONE entry through the import: `true` keeps it (coordinates now in
/// POST space); `false` means the entry — and everything DEEPER in its stack —
/// must be truncated.
fn rebase_entry(entry: &mut RecordedInverse, net: &crate::crdt_runtime::import_delta::NetBodyDelta, law: &RemoteNetLaw, structure_neutral: bool) -> bool {
  let mark_only = matches!(entry.undo.mutation, RecordedMutation::MarkParagraphStyles { .. })
    && matches!(entry.redo.mutation, RecordedMutation::MarkParagraphStyles { .. });
  let keep_patches = if mark_only {
    let (undo_ok, undo_dropped) = rebase_mark_delta(&mut entry.undo, net, law);
    if !undo_ok {
      return false;
    }
    let (redo_ok, redo_dropped) = rebase_mark_delta(&mut entry.redo, net, law);
    if !redo_ok {
      return false;
    }
    // A dropped target means the import deleted a boundary — the projection's
    // row set changed, so pre-recorded row-addressed patches are stale.
    // (`structure_neutral` is false in that case too; the explicit check keeps
    // the coupling local.)
    structure_neutral && !undo_dropped && !redo_dropped
  } else {
    // Content entries (restore/delete/splice) keep the v1 law against their
    // OWN hull: all remote ops proven before it, so every recorded coordinate
    // shifts by the net's total length delta.
    let lo = match (delta_min_coordinate(&entry.undo), delta_min_coordinate(&entry.redo)) {
      (Some(a), Some(b)) => a.min(b),
      (Some(a), None) => a,
      (None, Some(b)) => b,
      (None, None) => return false,
    };
    if !law.before_hull(lo) {
      return false;
    }
    if law.total_delta != 0 && !(shift_delta_coordinates(&mut entry.undo, law.total_delta) && shift_delta_coordinates(&mut entry.redo, law.total_delta)) {
      return false;
    }
    structure_neutral
  };
  // Pre-recorded projection patches survive a remote change that neither adds
  // nor removes rows: patches are row/id-addressed, and a nonstructural
  // character shift touches neither the patched rows' indices nor their
  // contents. A STRUCTURAL remote change (paragraph boundary added/removed)
  // shifts row indices — those entries downgrade to the derive ladder, which
  // recomputes patches at O(change).
  if !(keep_patches && patches_survive_rebase(&entry.undo)) {
    entry.undo.reproject = Reproject::Derive;
  }
  if !(keep_patches && patches_survive_rebase(&entry.redo)) {
    entry.redo.reproject = Reproject::Derive;
  }
  true
}

/// §mass-op collab v2 (oom-leads #9): REBASE the recorded fast-path stacks
/// through a remote import instead of dropping them. Each stack is walked from
/// its TOP (the next entry a fast step replays) downward, deciding PER ENTRY:
///
/// * Mark-only entries (select-all restyle — both directions re-mark boundary
///   chars, a position-neutral replay) rebase PER-COORDINATE via
///   [`rebase_mark_delta`]: remote edits may land anywhere, including inside
///   the marked span.
/// * Content entries keep the v1 hull law against their OWN hull
///   ([`RemoteNetLaw::before_hull`]) and shift by the net length delta.
/// * The first entry that fails truncates ITSELF AND EVERYTHING DEEPER; the
///   kept suffix stays aligned with the Loro stack tops (once the recorded
///   stack runs dry, `try_fast_step` returns `None` and the slow path serves
///   the deeper items).
///
/// Space soundness (why no cross-entry position mapping is needed): a deeper
/// entry replays only AFTER the entries above it, so its coordinates live in a
/// space that differs from the current doc by those replays — but every KEPT
/// entry's replay is position-IDENTITY at every remote-op position (mark
/// replays move nothing anywhere; a kept content entry touches only positions
/// at/above its hull, strictly above every remote op). Inductively the remote
/// positions mean the same thing in every kept entry's space, so per-entry
/// checks and shifts against the raw net are exact — and the first entry where
/// the induction would break is exactly where the walk truncates.
///
/// The Loro stacks' buffered remote diffs stay valid across kept fast steps
/// for the same reason (kept replays are position-neutral at every buffered
/// position) — and vendor patch #22 additionally transforms them through
/// every external-step inverse commit, mirroring the native
/// `undo_internal` path, so a later slow-path pop of a deeper item is
/// coordinate-correct unconditionally.
///
/// Returns `false` when both stacks ended up empty (the old "cleared" signal).
pub(crate) fn rebase_recorded_inverse_through_remote(
  core: &mut CrdtRuntime,
  net: &crate::crdt_runtime::import_delta::NetBodyDelta,
  changed_body_ranges_post: &[(usize, usize)],
  structure_neutral: bool,
) -> bool {
  if core.recorded_undo_stack().is_empty() && core.recorded_redo_stack().is_empty() {
    return true;
  }
  let law = RemoteNetLaw::from_net(net, changed_body_ranges_post);
  static REBASE_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  let debug = *REBASE_DEBUG.get_or_init(|| std::env::var_os("FLOWSTATE_DERIVE_DEBUG").is_some());
  let frontier = core.doc().state_frontiers().encode();
  let mut any_kept = false;
  for kind in [UndoOrRedo::Undo, UndoOrRedo::Redo] {
    let stack = active_stack(core, kind);
    let before = stack.len();
    let mut cut = None;
    for ix in (0..stack.len()).rev() {
      if !rebase_entry(&mut stack[ix], net, &law, structure_neutral) {
        cut = Some(ix);
        break;
      }
    }
    if let Some(ix) = cut {
      stack.drain(..=ix);
    }
    for entry in stack.iter_mut() {
      entry.survives_pending_remote = true;
    }
    if let Some(top) = stack.last_mut() {
      top.expected_frontier.clone_from(&frontier);
      any_kept = true;
    }
    if debug {
      let kept = active_stack(core, kind).len();
      eprintln!("rebase[recorded-inverse]: {kind:?} kept {kept}/{before} pre_span={:?} changed_post={changed_body_ranges_post:?}", net.pre_change_span());
    }
  }
  any_kept
}

/// §mass-op collab (oom-leads #9, soundness): whether a pre-recorded patch set
/// stays exact through a (structure-neutral, before-hull) rebase. The hull only
/// bounds the MUTATION coordinates — but `ParagraphText.new`/`delta_utf8` and
/// `ParagraphRuns.runs` snapshot the WHOLE patched paragraph, which extends
/// back before the mutation start (to the paragraph's first char), and
/// `InsertBlocks`/`ReplaceObjectBlock` carry block content snapshots. A remote
/// edit that is hull-disjoint can still land INSIDE the first patched
/// paragraph, ahead of the mutation start; replaying the stale snapshot would
/// clobber it in the projection (the doc-side mutation is position-shifted and
/// stays correct — the divergence is projection-only, which is worse: silent).
/// Only id-addressed, content-free patches survive; everything else downgrades
/// to the derive ladder (still O(change), recomputed from the true doc).
fn patches_survive_rebase(delta: &RecordedDelta) -> bool {
  match &delta.reproject {
    Reproject::Derive => true,
    Reproject::Patches(patches) => patches.iter().all(|patch| {
      matches!(
        patch,
        ProjectionPatch::ParagraphStyle { .. } | ProjectionPatch::DeleteBlocks { .. } | ProjectionPatch::MoveBlock { .. } | ProjectionPatch::AssetArrived { .. }
      )
    }),
  }
}

/// Current local-peer counter end (oplog space).
fn counter_end(core: &CrdtRuntime) -> loro::Counter {
  let peer = core.doc().peer_id();
  core.doc().oplog_vv().get(&peer).copied().unwrap_or(0)
}

/// The active fast-path stack for `kind` (undo stack for Undo, redo for Redo) —
/// its TOP is the entry a `kind` step would replay.
fn active_stack(core: &mut CrdtRuntime, kind: UndoOrRedo) -> &mut Vec<RecordedInverse> {
  match kind {
    UndoOrRedo::Undo => core.recorded_undo_stack(),
    UndoOrRedo::Redo => core.recorded_redo_stack(),
  }
}

/// The stack a replayed `kind` entry moves to (so the reverse step replays it).
fn opposite_stack(core: &mut CrdtRuntime, kind: UndoOrRedo) -> &mut Vec<RecordedInverse> {
  match kind {
    UndoOrRedo::Undo => core.recorded_redo_stack(),
    UndoOrRedo::Redo => core.recorded_undo_stack(),
  }
}

/// The undo/redo fast path. Returns `Ok(Some(events))` when the recorded
/// inverse was replayed, `Ok(None)` when the slow path must run instead. Never
/// leaves partial state behind: a replay failure compensates via `revert_to`.
///
/// §act-five P3-deep: STACK-based — the top of `kind`'s stack is the active
/// entry; on success it moves to the opposite stack, so a RUN of consecutive
/// undos each replays `O(change)`. Fail-safe: every step is validated against
/// the Loro `UndoManager`'s real top span (`peek_top_span`), so a stale entry
/// bails to the correct slow path and the whole cache is dropped.
pub(crate) fn try_fast_step(core: &mut CrdtRuntime, kind: UndoOrRedo) -> Result<Option<Vec<RuntimeEvent>>> {
  if fast_undo_disabled() {
    return Ok(None);
  }
  let expected_frontier = core.doc().state_frontiers().encode();
  let (expected_span, matches, survives_pending_remote) = match active_stack(core, kind).last() {
    Some(inverse) => (
      inverse.expected_span,
      inverse.expected_frontier == expected_frontier,
      inverse.survives_pending_remote,
    ),
    None => return Ok(None),
  };
  if !matches {
    // The top entry is stale (a concurrent change moved the frontier). The whole
    // local fast-path timeline is now unreachable — drop it; the slow path runs.
    core.clear_recorded_inverse();
    return Ok(None);
  }
  // §oom-leads #9: `clean == false` means remote diffs are buffered in the Loro
  // stacks. Normally that kills the fast path (the recorded coordinates would
  // be stale), but the rebase proved THIS entry's replay is coordinate-correct,
  // and the buffered diffs survive the inverse commit (kept replays are
  // position-neutral at every buffered position; vendor patch #22 transforms
  // the buffers through the commit besides) — proceed.
  match core.undo_manager_mut().peek_top_span(kind) {
    Some((span, clean)) if (clean || survives_pending_remote) && span == expected_span => {},
    _ => return Ok(None),
  }

  let from_frontier = core.doc().state_frontiers();
  let from_vv = core.doc().state_vv();
  if !core.undo_manager_mut().external_step_begin(kind, expected_span) {
    return Ok(None);
  }

  // ---- replay the recorded transaction ------------------------------------
  let counter_before = counter_end(core);
  let replayed = hotpath::measure_block!("recorded_inverse_replay", apply_recorded_mutation(core, kind));
  if let Err(error) = replayed {
    // I-10-style compensation.
    tracing::error!(%error, ?kind, "recorded-inverse replay failed mid-apply; compensating via revert_to");
    core.doc().set_next_commit_origin("repair");
    core.doc().set_next_commit_message("recorded-inverse-compensation");
    if let Err(revert_error) = core.doc().revert_to(&from_frontier) {
      core.undo_manager_mut().external_step_abort(kind);
      core.clear_recorded_inverse();
      return Err(anyhow::anyhow!(
        "recorded-inverse replay failed ({error:#}) and revert_to failed ({revert_error}); runtime must be reloaded"
      ));
    }
    core.doc().set_next_commit_origin("repair");
    core.doc().set_next_commit_message("recorded-inverse-compensation-inverse");
    core.doc().commit();
    core.undo_manager_mut().external_step_abort(kind);
    core.clear_recorded_inverse();
    let mut invalidation = ProjectionInvalidation::full_rebuild(
      from_frontier.encode(),
      core.doc().state_frontiers().encode(),
      "recorded-inverse-compensation",
    );
    core.merge_subscription_invalidation(&mut invalidation);
    let events = core.events_after_local_change(from_frontier, from_vv, invalidation, false)?;
    core.queue_publish(events);
    return Ok(None);
  }

  core.doc().set_next_commit_origin("undo");
  core.doc().set_next_commit_message(match kind {
    UndoOrRedo::Undo => "recorded-inverse-undo",
    UndoOrRedo::Redo => "recorded-inverse-redo",
  });
  core.doc().commit();
  let counter_after = counter_end(core);
  if counter_after == counter_before {
    tracing::error!(?kind, "recorded-inverse replay committed no ops; falling back");
    core.undo_manager_mut().external_step_abort(kind);
    core.clear_recorded_inverse();
    return Ok(None);
  }
  let inverse_span = CounterSpan::new(counter_before, counter_after);
  core.undo_manager_mut().external_step_finish(kind, inverse_span);

  // ---- projection + publish (same ladder as any local write) --------------
  let context = match kind {
    UndoOrRedo::Undo => "recorded-inverse-undo",
    UndoOrRedo::Redo => "recorded-inverse-redo",
  };
  let (recorded_patches, inval_ranges) = {
    let inverse = active_stack(core, kind).last().expect("active entry survives replay");
    let delta = match kind {
      UndoOrRedo::Undo => &inverse.undo,
      UndoOrRedo::Redo => &inverse.redo,
    };
    let patches = match &delta.reproject {
      Reproject::Patches(patches) => Some(patches.clone()),
      Reproject::Derive => None,
    };
    (patches, delta.invalidation_ranges.clone())
  };
  // Style-only and text deltas build the same range-carrying invalidation
  // (`body_style` has always delegated to `body_text`), so one multi-range
  // constructor serves both directions.
  let mut invalidation =
    ProjectionInvalidation::body_text_ranges(from_frontier.encode(), core.doc().state_frontiers().encode(), &inval_ranges);
  let drained = core.merge_subscription_invalidation(&mut invalidation);
  let mut events = core.events_after_local_change(from_frontier.clone(), from_vv, invalidation.clone(), false)?;
  match recorded_patches {
    Some(patches) => {
      // Exact pre-recorded patches — O(change), no readback.
      core.apply_projection_patch_set(&patches);
      core.set_projection_frontier(core.doc().state_frontiers().encode());
      super::commit::audit_patched_projection(core, context);
      events.push(core.projection_patched_event(patches, invalidation));
    },
    None => {
      // Derive ladder: regional / ranged readback off the committed doc (the
      // same path remote imports and semantic commands use).
      core.derive_body_projection_events(invalidation, &drained, context, &mut events)?;
      super::commit::audit_patched_projection(core, context);
    },
  }
  if let Some(snapshot) = core.take_restored_undo_selection() {
    if let Some(selection) = core.resolve_undo_selection(&snapshot) {
      events.push(RuntimeEvent::SelectionRestored { selection });
    } else {
      core.restore_undo_selection_later(snapshot);
    }
  }

  fidelity::event(FidelityClass::Undo, "recorded-inverse", || {
    format!(
      "kind={kind:?} span={expected_span:?} inverse={inverse_span:?} frontier {:?} -> {:?}",
      from_frontier.encode(),
      core.doc().state_frontiers().encode()
    )
  });

  // MOVE the replayed entry to the OPPOSITE stack, re-armed for the reverse
  // step: the transaction we just committed is exactly what the reverse step
  // replays. The entries BELOW it on the active stack keep their arming, so the
  // NEXT same-direction step (the Ctrl-Z mash) replays for free too.
  let new_frontier = core.doc().state_frontiers().encode();
  let mut entry = active_stack(core, kind).pop().expect("active entry survives replay");
  entry.direction = match kind {
    UndoOrRedo::Undo => UndoOrRedo::Redo,
    UndoOrRedo::Redo => UndoOrRedo::Undo,
  };
  entry.expected_frontier = new_frontier.clone();
  entry.expected_span = inverse_span;
  opposite_stack(core, kind).push(entry);
  // §act-five P3-deep: replaying the top committed a NEW inverse op, so the
  // frontier advanced. The entry now exposed at the top of the active stack was
  // recorded at its OWN commit frontier, which no longer matches — but it IS
  // replayable at THIS new frontier (we just moved to it). Re-arm it so the next
  // chained step's frontier check passes. Its `expected_span` still matches the
  // Loro `UndoManager`'s now-top (external_step popped exactly this direction), so
  // only the frontier needs refreshing.
  if let Some(next) = active_stack(core, kind).last_mut() {
    next.expected_frontier = new_frontier;
  }

  Ok(Some(events))
}

/// Apply the recorded doc-side mutation for `kind`'s direction to the live doc.
fn apply_recorded_mutation(core: &mut CrdtRuntime, kind: UndoOrRedo) -> Result<()> {
  let doc = core.doc().clone();
  let body = body_text(&doc);
  // The active entry is the top of `kind`'s stack (validated by the caller).
  let inverse = active_stack(core, kind).last().expect("caller checked the active stack");
  match &(match kind {
    UndoOrRedo::Undo => &inverse.undo,
    UndoOrRedo::Redo => &inverse.redo,
  })
  .mutation
  {
    RecordedMutation::RestoreRange {
      clamped_start,
      restore_delta,
      boundaries,
      objects,
    } => {
      let mut delta = Vec::with_capacity(restore_delta.len() + 1);
      if *clamped_start > 0 {
        delta.push(TextDelta::Retain {
          retain: *clamped_start,
          attributes: None,
        });
      }
      delta.extend(restore_delta.iter().cloned());
      body.apply_delta(&delta).context("applying recorded-inverse restore delta")?;
      for object in objects {
        restore_input_object_block_containers(&doc, object.unicode, object.block_id, &object.input)
          .context("restoring object block containers for recorded-inverse undo")?;
      }
      for boundary in boundaries {
        repair_paragraph_metadata_after_stable_split(&doc, &body, boundary.unicode, boundary.paragraph_id, boundary.block_id, "recorded_inverse_undo")
          .context("restoring paragraph records for recorded-inverse undo")?;
      }
    },
    RecordedMutation::DeleteRange {
      clamped_start,
      clamped_len,
      prune_objects,
      boundaries,
    } => {
      body.delete(*clamped_start, *clamped_len).context("re-deleting range for recorded-inverse redo")?;
      if *prune_objects {
        prune_orphaned_body_object_blocks(&doc, &body).context("pruning object blocks for recorded-inverse redo")?;
      }
      for boundary in boundaries {
        delete_projection_paragraph_metadata(&doc, boundary.paragraph_id, boundary.block_id)
          .context("retiring paragraph records for recorded-inverse redo")?;
      }
    },
    RecordedMutation::MarkParagraphStyles { targets } => {
      for (boundary, value) in targets {
        body
          .mark(*boundary..*boundary + 1, MARK_PARAGRAPH_STYLE, *value)
          .context("re-marking paragraph style for recorded-inverse step")?;
      }
    },
    RecordedMutation::CreateBoundary {
      at,
      style_value,
      paragraph_id,
      block_id,
    } => {
      body.insert(*at, "\n").context("re-creating paragraph boundary for recorded-inverse step")?;
      body
        .mark(*at..*at + 1, MARK_PARAGRAPH_STYLE, *style_value)
        .context("marking re-created boundary's paragraph style")?;
      // Sentinel hygiene, mirroring the SplitParagraph executor: expand-After
      // run marks absorb the inserted newline; strip the run keys so styling
      // never bleeds across the boundary. `mark_run_styles` with default
      // styles IS the four-key unmark.
      crate::crdt_runtime::mark_run_styles(&body, *at..*at + 1, flowstate_document::RunStyles::default())
        .context("stripping run keys from re-created boundary")?;
      repair_paragraph_metadata_after_stable_split(&doc, &body, *at, *paragraph_id, *block_id, "recorded_inverse_boundary")
        .context("re-writing paragraph records for recorded-inverse boundary")?;
    },
    RecordedMutation::MarkRunRanges { spans } => {
      for (start, len, styles) in spans {
        crate::crdt_runtime::mark_run_styles(&body, *start..*start + *len, *styles)
          .context("re-applying run styles for recorded-inverse step")?;
      }
    },
    RecordedMutation::SpliceRichRanges { splices } => {
      // Splices are recorded in DESCENDING start order; applying them in that
      // order keeps every earlier splice's position valid. Each range is
      // replaced by its captured rich content via one `apply_delta`
      // (Retain → Delete → Insert…), preserving marks exactly.
      for splice in splices {
        let mut delta = Vec::with_capacity(splice.insert.len() + 2);
        if splice.start > 0 {
          delta.push(TextDelta::Retain {
            retain: splice.start,
            attributes: None,
          });
        }
        if splice.delete_len > 0 {
          delta.push(TextDelta::Delete { delete: splice.delete_len });
        }
        delta.extend(splice.insert.iter().cloned());
        body.apply_delta(&delta).context("applying recorded-inverse rich splice")?;
      }
    },
  }
  Ok(())
}
