//! §act-three B.1: the recorded-inverse undo/redo fast path.
//!
//! A qualifying mass `DeleteRange` commit records everything needed to invert
//! itself as an ORDINARY forward transaction: the deleted rich text captured
//! VERBATIM from Loro (`slice_delta` — text, marks, paragraph-style newline
//! carriers, and object placeholders exactly as stored), the retired
//! paragraph/object identities, and the intent-exact projection patches for
//! BOTH directions. Undo then skips the `UndoManager`'s checkout-based diff
//! computation entirely: it replays the recorded inverse through the same
//! commit/publish ladder as any local write, and the vendored
//! `external_step_begin`/`external_step_finish` APIs keep the undo/redo
//! stacks bookkeeping-identical to a native `UndoManager::undo`. Redo
//! symmetrically replays the recorded delete; the slot ping-pongs between
//! directions for free after the one capture.
//!
//! Fidelity contract (spec §3 B.1, 100% bar): the fast path runs ONLY when
//! - the doc frontier still equals the frontier recorded when the slot was
//!   (re)armed — ANY interleaving commit, import, repair, or checkout fails
//!   this check;
//! - the top undo/redo stack item is exactly the recorded counter span (an
//!   undo-group merge changes the span and is therefore declined); and
//! - the stack tracks no interleaved remote/excluded-origin diff.
//!
//! Anything else falls through to the checkout-based slow path unchanged.
//! Kill switch: `FLOWSTATE_DISABLE_FAST_UNDO=1` disables capture and replay.

use anyhow::{Context as _, Result};
use flowstate_document::{
  Block, BlockId, InputBlock, ParagraphId, ProjectionPatch, ProjectionStructuralBlock, block_ix_for_paragraph, input_block_from_block,
  input_paragraph_from_document_range, loro_schema::body_text, paragraph_text, paragraph_text_len,
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use loro::{CounterSpan, TextDelta, UndoOrRedo, cursor::PosType};

use super::commit::ResolvedPlan;
use crate::crdt_runtime::{
  CrdtRuntime, ProjectionInvalidation, RuntimeEvent, delete_projection_paragraph_metadata, projection_text_delta,
  prune_orphaned_body_object_blocks, repair_paragraph_metadata_after_stable_split, restore_input_object_block_containers,
  sentinel_protected_delete_range,
};

/// Minimum deleted-range size (unicode chars) worth a capture. Below this the
/// checkout-based slow path is already cheap and the capture isn't free.
const MIN_CAPTURE_CHARS: usize = 2048;

/// Kill switch (spec §3 B.1): `FLOWSTATE_DISABLE_FAST_UNDO=1` reverts every
/// undo/redo to the checkout-based slow path.
fn fast_undo_disabled() -> bool {
  static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *DISABLED.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_FAST_UNDO").is_some())
}

/// A restored paragraph boundary: the absolute body-unicode index of its
/// leading `\n` plus the ORIGINAL durable identities of the paragraph that
/// follows it (the exact records the delete retired).
struct RestoredBoundary {
  unicode: usize,
  paragraph_id: ParagraphId,
  block_id: BlockId,
}

/// A restored object block: the absolute body-unicode index of its U+FFFC
/// placeholder plus the ORIGINAL block id and captured content. The
/// placeholder char itself rides the verbatim text restore; only the
/// registry containers are recreated.
struct RestoredObject {
  unicode: usize,
  block_id: BlockId,
  input: InputBlock,
}

/// Everything needed to replay one delete/restore pair in either direction.
pub(crate) struct RecordedInverse {
  /// The direction the next fast step serves (`Undo` restores, `Redo`
  /// re-deletes). Flips after every successful fast step.
  direction: UndoOrRedo,
  /// Doc frontier that must match EXACTLY at step time (encoded).
  expected_frontier: Vec<u8>,
  /// The top stack item's counter span that must match exactly.
  expected_span: CounterSpan,
  // -- doc-side replay material -------------------------------------------
  clamped_start: usize,
  clamped_len: usize,
  /// Verbatim `slice_delta` of the deleted range: text + marks exactly as
  /// they were stored, including paragraph-style newline carriers and object
  /// placeholders.
  restore_delta: Vec<TextDelta>,
  boundaries: Vec<RestoredBoundary>,
  objects: Vec<RestoredObject>,
  // -- projection-side replay material -------------------------------------
  undo_patches: Vec<ProjectionPatch>,
  redo_patches: Vec<ProjectionPatch>,
}

impl RecordedInverse {
  /// Probe (tests/diagnostics): the direction the slot currently serves. A
  /// successful fast step flips this; the slow path never touches it.
  #[cfg(test)]
  pub(crate) fn direction(&self) -> UndoOrRedo {
    self.direction
  }
}

impl std::fmt::Debug for RecordedInverse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RecordedInverse")
      .field("direction", &self.direction)
      .field("expected_span", &self.expected_span)
      .field("clamped_start", &self.clamped_start)
      .field("clamped_len", &self.clamped_len)
      .field("segments", &self.restore_delta.len())
      .field("boundaries", &self.boundaries.len())
      .field("objects", &self.objects.len())
      .finish_non_exhaustive()
  }
}

/// Pre-mutation half of the capture: taken between resolve and execute, while
/// the doc and projection are still at their pre-delete state.
pub(crate) struct PendingInverseCapture {
  clamped_start: usize,
  clamped_len: usize,
  restore_delta: Vec<TextDelta>,
  boundaries: Vec<RestoredBoundary>,
  objects: Vec<RestoredObject>,
  undo_patches: Vec<ProjectionPatch>,
}

/// Capture the recorded inverse of a qualifying cross-paragraph
/// `DeleteRange` plan. Must run BEFORE the delete mutates the doc. Returns
/// `None` (never an error) for anything that doesn't qualify — the delete
/// itself proceeds identically either way.
pub(crate) fn capture_before_delete(core: &CrdtRuntime, plan: &ResolvedPlan) -> Option<PendingInverseCapture> {
  if fast_undo_disabled() {
    return None;
  }
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
  // first). Captured with their ORIGINAL ids so undo restores identity, not
  // just content.
  let mut removed_rows = Vec::with_capacity(last_block_ix - first_block_ix);
  let mut removed_paragraphs: Vec<(ParagraphId, BlockId)> = Vec::new();
  let mut removed_objects: Vec<(BlockId, InputBlock)> = Vec::new();
  let mut paragraph_ix = start.paragraph_ix;
  for block_ix in (first_block_ix + 1)..=last_block_ix {
    let block = projection.blocks.get(block_ix)?;
    let block_id = projection.ids.block_ids.get(block_ix).copied()?;
    match block {
      Block::Table(_) => {
        // Durable row/column/cell ids cannot be re-minted losslessly by the
        // restore; tables take the checkout-based slow path (v1 gate).
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
    // Projection row walk disagrees with the resolved positions — bail loudly
    // to the slow path rather than record a wrong inverse.
    tracing::warn!(
      start = start.paragraph_ix,
      end = end.paragraph_ix,
      walked = paragraph_ix,
      "recorded-inverse capture declined: paragraph row walk mismatch"
    );
    return None;
  }

  // Verbatim capture of the deleted range straight from Loro: the single
  // source of truth for text AND marks. Restoring this via `apply_delta`
  // reproduces the exact styled content, with insertion-edge style bleed
  // handled by apply_delta's override semantics.
  let body = body_text(core.doc());
  let restore_delta = match body.slice_delta(clamped_start, clamped_start + clamped_len, PosType::Unicode) {
    Ok(delta) => delta,
    Err(error) => {
      tracing::warn!(%error, "recorded-inverse capture declined: slice_delta failed");
      return None;
    },
  };

  // Pair each deleted `\n` (a retired paragraph boundary) and each deleted
  // U+FFFC (a pruned object) with the captured rows, in document order. A
  // count mismatch means the projection and Loro disagree about the range's
  // structure — decline and let the slow path (and the fidelity audits) see it.
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

  // Undo-direction patches: restore the first paragraph's original content
  // and re-insert the removed rows (original ids) before the surviving
  // successor row. The exact structural mirror of the DeleteRange synthesis.
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
  if !removed_rows.is_empty() {
    undo_patches.push(ProjectionPatch::InsertBlocks {
      before: projection.ids.block_ids.get(last_block_ix + 1).copied(),
      row_hint: first_block_ix + 1,
      blocks: removed_rows,
    });
  }

  Some(PendingInverseCapture {
    clamped_start,
    clamped_len,
    restore_delta,
    boundaries,
    objects,
    undo_patches,
  })
}

/// Post-commit half of the capture: arm the slot with the committed delete's
/// counter span, the post-commit frontier, and the delete's own synthesized
/// patches (verbatim redo material).
pub(crate) fn finalize_capture(core: &mut CrdtRuntime, pending: PendingInverseCapture, delete_span: CounterSpan, redo_patches: &[ProjectionPatch]) {
  let expected_frontier = core.doc().state_frontiers().encode();
  *core.recorded_inverse_slot() = Some(RecordedInverse {
    direction: UndoOrRedo::Undo,
    expected_frontier,
    expected_span: delete_span,
    clamped_start: pending.clamped_start,
    clamped_len: pending.clamped_len,
    restore_delta: pending.restore_delta,
    boundaries: pending.boundaries,
    objects: pending.objects,
    undo_patches: pending.undo_patches,
    redo_patches: redo_patches.to_vec(),
  });
}

/// Current local-peer counter end (oplog space) — the same measure the
/// `UndoManager` uses for item spans.
fn counter_end(core: &CrdtRuntime) -> loro::Counter {
  let peer = core.doc().peer_id();
  core.doc().oplog_vv().get(&peer).copied().unwrap_or(0)
}

/// The undo/redo fast path. Returns `Ok(Some(events))` when the recorded
/// inverse was replayed (the semantic command is complete), `Ok(None)` when
/// the slow path must run instead. Never leaves partial state behind: a
/// replay failure compensates via `revert_to` and restores the undo stacks.
pub(crate) fn try_fast_step(core: &mut CrdtRuntime, kind: UndoOrRedo) -> Result<Option<Vec<RuntimeEvent>>> {
  if fast_undo_disabled() || core.recorded_inverse_slot().is_none() {
    return Ok(None);
  }
  {
    let Some(inverse) = core.recorded_inverse_slot().as_ref() else {
      return Ok(None);
    };
    if inverse.direction != kind {
      return Ok(None);
    }
  }
  let expected_frontier = core.doc().state_frontiers().encode();
  let (expected_span, matches) = {
    let inverse = core.recorded_inverse_slot().as_ref().expect("checked above");
    (inverse.expected_span, inverse.expected_frontier == expected_frontier)
  };
  if !matches {
    // Something committed or imported since the capture — the record is dead.
    *core.recorded_inverse_slot() = None;
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
  let replayed = hotpath::measure_block!("recorded_inverse_replay", {
    match kind {
      UndoOrRedo::Undo => execute_restore(core),
      UndoOrRedo::Redo => execute_redelete(core),
    }
  });
  if let Err(error) = replayed {
    // I-10-style compensation: the pending partial mutation reverts under the
    // repair origin, the popped stack item is restored, and the caller falls
    // back to the slow path on intact state.
    tracing::error!(%error, ?kind, "recorded-inverse replay failed mid-apply; compensating via revert_to");
    core.doc().set_next_commit_origin("repair");
    core.doc().set_next_commit_message("recorded-inverse-compensation");
    if let Err(revert_error) = core.doc().revert_to(&from_frontier) {
      core.undo_manager_mut().external_step_abort(kind);
      *core.recorded_inverse_slot() = None;
      return Err(anyhow::anyhow!(
        "recorded-inverse replay failed ({error:#}) and revert_to failed ({revert_error}); runtime must be reloaded"
      ));
    }
    core.doc().set_next_commit_origin("repair");
    core.doc().set_next_commit_message("recorded-inverse-compensation-inverse");
    core.doc().commit();
    core.undo_manager_mut().external_step_abort(kind);
    *core.recorded_inverse_slot() = None;
    // The compensation commits changed the frontier; publish them so peers
    // never see the partial without its inverse.
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
    // Nothing committed — should be impossible for a non-empty range; restore
    // the stacks and let the slow path decide.
    tracing::error!(?kind, "recorded-inverse replay committed no ops; falling back");
    core.undo_manager_mut().external_step_abort(kind);
    *core.recorded_inverse_slot() = None;
    return Ok(None);
  }
  let inverse_span = CounterSpan::new(counter_before, counter_after);
  core.undo_manager_mut().external_step_finish(kind, inverse_span);

  // ---- projection + publish (same ladder as any local write) --------------
  let (patches, invalidation_len) = {
    let inverse = core.recorded_inverse_slot().as_ref().expect("slot survives replay");
    match kind {
      UndoOrRedo::Undo => (inverse.undo_patches.clone(), inverse.clamped_len),
      UndoOrRedo::Redo => (inverse.redo_patches.clone(), inverse.clamped_len),
    }
  };
  let clamped_start = core.recorded_inverse_slot().as_ref().expect("slot survives replay").clamped_start;
  let mut invalidation = ProjectionInvalidation::body_text(
    from_frontier.encode(),
    core.doc().state_frontiers().encode(),
    clamped_start,
    invalidation_len,
  );
  core.merge_subscription_invalidation(&mut invalidation);
  let mut events = core.events_after_local_change(from_frontier.clone(), from_vv, invalidation.clone(), false)?;
  core.apply_projection_patch_set(&patches);
  core.set_projection_frontier(core.doc().state_frontiers().encode());
  // Debug-build audit (spec §7): the replayed patches must equal a full
  // rematerialization — every fast undo in tests is verified end-to-end.
  super::commit::audit_patched_projection(
    core,
    match kind {
      UndoOrRedo::Undo => "recorded-inverse-undo",
      UndoOrRedo::Redo => "recorded-inverse-redo",
    },
  );
  events.push(core.projection_patched_event(patches, invalidation));

  // Selection restore: on_pop already fired inside external_step_begin; the
  // snapshot resolves against the freshly patched projection.
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

  // Flip the slot: the pushed opposite-stack item is exactly the transaction
  // we just committed, so the next opposite-direction step replays for free.
  let new_frontier = core.doc().state_frontiers().encode();
  let slot = core.recorded_inverse_slot().as_mut().expect("slot survives replay");
  slot.direction = match kind {
    UndoOrRedo::Undo => UndoOrRedo::Redo,
    UndoOrRedo::Redo => UndoOrRedo::Undo,
  };
  slot.expected_frontier = new_frontier;
  slot.expected_span = inverse_span;

  Ok(Some(events))
}

/// Undo direction: put the deleted range back exactly — one bulk
/// text+marks restore (the verbatim captured delta), the object registry
/// containers, and the retired paragraph records with their original ids.
fn execute_restore(core: &mut CrdtRuntime) -> Result<()> {
  let doc = core.doc().clone();
  let inverse = core.recorded_inverse_slot().as_ref().expect("caller checked the slot");
  let body = body_text(&doc);
  let mut delta = Vec::with_capacity(inverse.restore_delta.len() + 1);
  if inverse.clamped_start > 0 {
    delta.push(TextDelta::Retain {
      retain: inverse.clamped_start,
      attributes: None,
    });
  }
  delta.extend(inverse.restore_delta.iter().cloned());
  body.apply_delta(&delta).context("applying recorded-inverse restore delta")?;
  for object in &inverse.objects {
    restore_input_object_block_containers(&doc, object.unicode, object.block_id, &object.input)
      .context("restoring object block containers for recorded-inverse undo")?;
  }
  for boundary in &inverse.boundaries {
    repair_paragraph_metadata_after_stable_split(&doc, &body, boundary.unicode, boundary.paragraph_id, boundary.block_id, "recorded_inverse_undo")
      .context("restoring paragraph records for recorded-inverse undo")?;
  }
  Ok(())
}

/// Redo direction: replay the recorded delete — the same mutation sequence as
/// the original `DeleteRange` execution, driven from recorded data instead of
/// re-resolution (the frontier check guarantees the state is identical).
fn execute_redelete(core: &mut CrdtRuntime) -> Result<()> {
  let doc = core.doc().clone();
  let inverse = core.recorded_inverse_slot().as_ref().expect("caller checked the slot");
  let body = body_text(&doc);
  body
    .delete(inverse.clamped_start, inverse.clamped_len)
    .context("re-deleting range for recorded-inverse redo")?;
  if !inverse.objects.is_empty() {
    prune_orphaned_body_object_blocks(&doc, &body).context("pruning object blocks for recorded-inverse redo")?;
  }
  for boundary in &inverse.boundaries {
    delete_projection_paragraph_metadata(&doc, boundary.paragraph_id, boundary.block_id)
      .context("retiring paragraph records for recorded-inverse redo")?;
  }
  Ok(())
}
