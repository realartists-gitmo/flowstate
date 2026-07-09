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

use anyhow::{Context as _, Result};
use flowstate_document::{
  Block, BlockId, InputBlock, MARK_PARAGRAPH_STYLE, ParagraphId, ProjectionPatch, ProjectionStructuralBlock, block_ix_for_paragraph,
  input_block_from_block, input_paragraph_from_document_range, loro_schema::body_text, paragraph_text, paragraph_text_len,
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use loro::{CounterSpan, TextDelta, UndoOrRedo, cursor::PosType};

use super::commit::ResolvedPlan;
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
  /// projection — `O(change)`, no readback.
  Patches(Vec<ProjectionPatch>),
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
  invalidation_start: usize,
  invalidation_len: usize,
  /// Style-only change ⇒ `body_style` invalidation; else `body_text`.
  style_only: bool,
}

/// Everything needed to replay one committed op in either direction.
pub(crate) struct RecordedInverse {
  /// The direction the next fast step serves (flips after every fast step).
  direction: UndoOrRedo,
  /// Doc frontier that must match EXACTLY at step time (encoded).
  expected_frontier: Vec<u8>,
  /// The top stack item's counter span that must match exactly.
  expected_span: CounterSpan,
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
  redo_invalidation_start: usize,
  redo_invalidation_len: usize,
  redo_style_only: bool,
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
    ResolvedPlan::DeleteRange { .. } => capture_delete_range(core, plan),
    ResolvedPlan::SetParagraphStyles { .. } => capture_set_paragraph_styles(core, plan),
    ResolvedPlan::ReplaceMatches { .. } => capture_replace_matches(core, plan),
    _ => None,
  }
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
  let (mut undo_min, mut undo_max) = (usize::MAX, 0usize);
  let (mut redo_min, mut redo_max) = (usize::MAX, 0usize);
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
    undo_min = undo_min.min(q);
    undo_max = undo_max.max(q + repl_len);
    redo_min = redo_min.min(s);
    redo_max = redo_max.max(s + orig_len);
  }

  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::SpliceRichRanges { splices: undo_splices },
      reproject: Reproject::Derive,
      invalidation_start: undo_min,
      invalidation_len: undo_max.saturating_sub(undo_min).max(1),
      style_only: false,
    },
    // Filled at finalize from the post-op doc (see `redo_replace_finalize`).
    redo_mutation: RecordedMutation::SpliceRichRanges { splices: Vec::new() },
    redo_invalidation_start: redo_min,
    redo_invalidation_len: redo_max.saturating_sub(redo_min).max(1),
    redo_style_only: false,
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
  let mut min_boundary = usize::MAX;
  let mut max_boundary = 0usize;
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
    min_boundary = min_boundary.min(*boundary);
    max_boundary = max_boundary.max(*boundary);
  }
  let inval_len = max_boundary.saturating_sub(min_boundary) + 1;

  Some(PendingInverseCapture {
    undo: RecordedDelta {
      mutation: RecordedMutation::MarkParagraphStyles { targets: undo_marks },
      reproject: Reproject::Patches(undo_patches),
      invalidation_start: min_boundary,
      invalidation_len: inval_len,
      style_only: true,
    },
    redo_mutation: RecordedMutation::MarkParagraphStyles { targets: redo_marks },
    redo_invalidation_start: min_boundary,
    redo_invalidation_len: inval_len,
    redo_style_only: true,
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
  if len < MIN_CAPTURE_CHARS || start.paragraph_ix == end.paragraph_ix {
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
      reproject: Reproject::Patches(undo_patches),
      invalidation_start: clamped_start,
      invalidation_len: clamped_len,
      style_only: false,
    },
    redo_mutation: RecordedMutation::DeleteRange {
      clamped_start,
      clamped_len,
      prune_objects,
      boundaries: redo_boundaries,
    },
    redo_invalidation_start: clamped_start,
    redo_invalidation_len: clamped_len,
    redo_style_only: false,
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
    Reproject::Patches(redo_patches.to_vec())
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
    undo: pending.undo,
    redo: RecordedDelta {
      mutation: redo_mutation,
      reproject: redo_reproject,
      invalidation_start: pending.redo_invalidation_start,
      invalidation_len: pending.redo_invalidation_len,
      style_only: pending.redo_style_only,
    },
  });
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
  let (expected_span, matches) = match active_stack(core, kind).last() {
    Some(inverse) => (inverse.expected_span, inverse.expected_frontier == expected_frontier),
    None => return Ok(None),
  };
  if !matches {
    // The top entry is stale (a concurrent change moved the frontier). The whole
    // local fast-path timeline is now unreachable — drop it; the slow path runs.
    core.clear_recorded_inverse();
    return Ok(None);
  }
  match core.undo_manager_mut().peek_top_span(kind) {
    Some((span, clean)) if clean && span == expected_span => {},
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
  let (recorded_patches, inval_start, inval_len, style_only) = {
    let inverse = active_stack(core, kind).last().expect("active entry survives replay");
    let delta = match kind {
      UndoOrRedo::Undo => &inverse.undo,
      UndoOrRedo::Redo => &inverse.redo,
    };
    let patches = match &delta.reproject {
      Reproject::Patches(patches) => Some(patches.clone()),
      Reproject::Derive => None,
    };
    (patches, delta.invalidation_start, delta.invalidation_len, delta.style_only)
  };
  let mut invalidation = if style_only {
    ProjectionInvalidation::body_style(from_frontier.encode(), core.doc().state_frontiers().encode(), inval_start, inval_len)
  } else {
    ProjectionInvalidation::body_text(from_frontier.encode(), core.doc().state_frontiers().encode(), inval_start, inval_len)
  };
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
  // §act-four M5: undo/redo lands a new frontier — append its root to the log.
  core.record_projection_version();

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
