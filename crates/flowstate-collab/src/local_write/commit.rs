//! Intent execution: resolve → mutate → commit → patch (spec §4, I-10).
//!
//! Runs with the write gate already held by the caller ([`super::handle`]).
//! Execution phases:
//!
//! 1. **Resolve** every anchor/identity against live Loro state. Any failure
//!    rejects the intent before the doc is touched (I-15).
//! 2. **Mutate** the doc through the surviving Loro-authority helpers. A
//!    mid-apply error triggers the I-10 compensation path (`revert_to`), never
//!    a partial commit escaping.
//! 3. **Commit** exactly once, origin `local`, commit message = intent class
//!    (field forensics, spec §16).
//! 4. **Synthesize** exact projection patches from the resolved intent
//!    (O(edit)); fall back to a full rebuild only for op classes that
//!    legitimately require it — loudly, counted.
//!
//! One intent = one Loro commit = one undo-group member.

use anyhow::{Context as _, Result};
use flowstate_document::{
  BlockId, DocumentProjection, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE,
  MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, ParagraphId, ProjectionPatchBatch, loro_schema::body_text,
};
use gpui_flowtext::{DocumentOffset, SelectionAffinity, VisualGravity};
use loro::LoroDoc;
use uuid::Uuid;

use super::intents::{
  CursorEndpoint, FragmentBlock, IntentCounters, LocalCommit, LocalIntent, LocalWriteOutcome, ProjectionReplace, SelectionSnapshot,
  TableIntent, WriteRejected,
};
use super::patch_synthesis::{PatchPlan, synthesize_patches};
use super::resolve::{ResolvedTextPosition, resolve_text_anchor, resolve_text_range};
use crate::crdt_runtime::{
  CrdtRuntime, cursor_for_boundary, delete_projection_object_block, delete_projection_paragraph_metadata, insert_projection_object_block,
  join_projection_paragraphs, mark_run_styles, move_projection_object_block, paragraph_boundary_loro_unicode_index, paragraph_style_value,
  prune_orphaned_body_object_blocks, repair_paragraph_metadata_after_stable_split,
  replace_projection_equation_source_range, replace_projection_image_alt_text, replace_projection_image_caption,
  replace_projection_object_block, sentinel_protected_delete_range, set_projection_image_layout, table_ops,
};

/// The fully-resolved execution plan for one intent, produced before any
/// mutation. Everything positional in here is live-Loro-space, resolved inside
/// the gate.
#[derive(Debug)]
pub(crate) enum ResolvedPlan {
  InsertText {
    at: ResolvedTextPosition,
    text: String,
    style_override: Option<flowstate_document::RunStyles>,
  },
  DeleteRange {
    start: ResolvedTextPosition,
    end: ResolvedTextPosition,
  },
  SplitParagraph {
    at: ResolvedTextPosition,
    inherited_style: flowstate_document::ParagraphStyle,
    new_paragraph: ParagraphId,
    new_block: BlockId,
  },
  JoinParagraphs {
    first: ParagraphId,
    second: ParagraphId,
    first_ix: usize,
  },
  SetMarks {
    start: ResolvedTextPosition,
    end: ResolvedTextPosition,
    styles: flowstate_document::RunStyles,
  },
  SetParagraphStyle {
    paragraph: ParagraphId,
    paragraph_ix: usize,
    style: flowstate_document::ParagraphStyle,
  },
  SetParagraphStyles {
    /// `(paragraph id, projection index, boundary loro-unicode position)`,
    /// input order; stale and non-representable targets already skipped.
    /// Marks never shift text, so pre-resolved boundaries stay valid across
    /// the whole batch.
    targets: Vec<(ParagraphId, usize, usize)>,
    style: flowstate_document::ParagraphStyle,
  },
  InsertObject {
    at: ResolvedTextPosition,
    block_ix: usize,
    new_block: BlockId,
    block: flowstate_document::InputBlock,
  },
  ReplaceObject {
    block: BlockId,
    block_ix: usize,
    after: flowstate_document::InputBlock,
  },
  DeleteBlocks {
    blocks: Vec<(BlockId, usize)>,
  },
  MoveBlock {
    block: BlockId,
    to_ix: usize,
  },
  InsertRichFragment {
    at: ResolvedTextPosition,
    blocks: Vec<FragmentBlock>,
  },
  ReplaceMatches {
    /// Resolved (start, end, replacement styles) triples, sorted DESCENDING
    /// by start and pruned of collapsed/cross-paragraph/overlapping ranges,
    /// so back-to-front application never shifts a later range.
    matches: Vec<(ResolvedTextPosition, ResolvedTextPosition, Option<flowstate_document::RunStyles>)>,
    replacement: String,
  },
  ReplaceEquationSourceRange {
    equation: BlockId,
    range: std::ops::Range<usize>,
    text: String,
  },
  ReplaceImageAltText {
    image: BlockId,
    text: String,
  },
  ReplaceImageCaption {
    image: BlockId,
    caption: Option<flowstate_document::InputParagraph>,
  },
  SetImageLayout {
    image: BlockId,
    sizing: flowstate_document::InputImageSizing,
    alignment: flowstate_document::InputBlockAlignment,
  },
  Table {
    table: BlockId,
    table_ix: usize,
    op: ResolvedTableOp,
  },
}

#[derive(Debug)]
pub(crate) enum ResolvedTableOp {
  InsertRow {
    new_row: flowstate_document::InputTableRow,
    after_row: Option<gpui_flowtext::RowId>,
  },
  DeleteRow {
    row: gpui_flowtext::RowId,
  },
  MoveRow {
    row: gpui_flowtext::RowId,
    after_row: Option<gpui_flowtext::RowId>,
  },
  InsertColumn {
    new_column: gpui_flowtext::ColumnId,
    after_column: Option<gpui_flowtext::ColumnId>,
    width: flowstate_document::InputTableColumnWidth,
    cells: Vec<flowstate_document::InputTableCell>,
  },
  DeleteColumn {
    column: gpui_flowtext::ColumnId,
  },
  MoveColumn {
    column: gpui_flowtext::ColumnId,
    after_column: Option<gpui_flowtext::ColumnId>,
  },
  ReplaceCell {
    row: gpui_flowtext::RowId,
    column: gpui_flowtext::ColumnId,
    cell: flowstate_document::InputTableCell,
  },
  SetCellSpan {
    row: gpui_flowtext::RowId,
    column: gpui_flowtext::ColumnId,
    row_span: u16,
    column_span: u16,
  },
  SetColumnWidth {
    column_ix: usize,
    width: flowstate_document::InputTableColumnWidth,
  },
}

/// Mutation-phase output: what the plan did, for patch synthesis and caret
/// placement.
#[derive(Debug, Default)]
pub(crate) struct MutationSummary {
  pub containers_touched: u32,
  pub marks_emitted: u32,
  /// Live body-unicode caret position after the mutation, when the intent
  /// defines one.
  pub caret_body_unicode: Option<usize>,
}

/// Execute one local intent against the gate-held core.
#[hotpath::measure]
pub(crate) fn apply_local_intent(core: &mut CrdtRuntime, intent: &LocalIntent) -> Result<LocalWriteOutcome, WriteRejected> {
  let mut counters = IntentCounters::default();

  // ---- Phase 1: resolve (no mutation on any failure path) -----------------
  let plan = resolve_intent(core, intent)?;

  let frontier_before = core.doc().state_frontiers();
  let vv_before = core.doc().state_vv();
  let base_frontier = frontier_before.encode();

  // ---- Phase 2: mutate (Err → I-10 compensation) ---------------------------
  let summary = match execute_plan(core, &plan) {
    Ok(summary) => summary,
    Err(error) => return Err(compensate_failed_intent(core, &frontier_before, &vv_before, intent.class(), &error)),
  };
  counters.containers_touched = summary.containers_touched;
  counters.marks_emitted = summary.marks_emitted;

  // ---- Phase 3: commit ------------------------------------------------------
  // Single-threaded inside the gate, so the doc-global next-commit options are
  // race-free here (semantics-audit C4). Origin `local` keeps the commit inside
  // the undo stack; the class message is §16 field-forensics attribution.
  counters.loro_ops = pending_op_count(core.doc());
  hotpath::measure_block!("loro_commit", {
    core.doc().set_next_commit_origin("local");
    core.doc().set_next_commit_message(intent.class());
    core.doc().commit();
  });
  if let Err(error) = core.record_undo_checkpoint() {
    // The commit itself succeeded; a checkpoint failure only degrades undo
    // granularity. Loud, not fatal.
    tracing::error!(%error, class = intent.class(), "recording undo checkpoint after local intent failed");
  }

  // Spec §16 lazy identity: this replica now demonstrably edits — register
  // the deferred author identity (no-op after the first time).
  core.register_pending_author_identity();

  let new_frontier = core.doc().state_frontiers().encode();

  // ---- Phase 4: patches + publish -------------------------------------------
  let invalidation_patches = synthesize_patches(core, intent, &plan);
  let transaction_id = Uuid::new_v4().as_u128();

  let outcome = match invalidation_patches {
    PatchPlan::Patches { patches, invalidation } => {
      counters.patch_count = u32::try_from(patches.len()).unwrap_or(u32::MAX);
      let mut invalidation = invalidation;
      core.merge_subscription_invalidation(&mut invalidation);
      core.apply_projection_patch_set(&patches);
      core.set_projection_frontier(new_frontier.clone());
      queue_publish_events(core, frontier_before, vv_before, invalidation);
      audit_patched_projection(core, intent.class());
      let batch = ProjectionPatchBatch {
        transaction_id,
        base_frontier,
        new_frontier: new_frontier.clone(),
        patches,
      };
      // Ordered stream (field fix): the editor drains this batch in commit
      // order together with any remote batches that preceded it.
      core.push_editor_stream(gpui_flowtext::ProjectionStreamItem::Patches(batch.clone()));
      LocalWriteOutcome::Committed(LocalCommit {
        patches: batch,
        frontier: new_frontier,
        version_vector: core.doc().state_vv().encode(),
        selection_after: selection_after(core, &summary),
        counters,
      })
    },
    PatchPlan::FullRebuild { invalidation, reason } => {
      counters.full_rebuild = true;
      tracing::warn!(class = intent.class(), reason, "full-rebuild-after-local-write");
      let mut invalidation = invalidation;
      core.merge_subscription_invalidation(&mut invalidation);
      if let Err(error) = core.refresh_projection() {
        return Err(compensate_failed_intent(core, &frontier_before, &vv_before, intent.class(), &error));
      }
      queue_publish_events(core, frontier_before, vv_before, invalidation);
      let replace = ProjectionReplace {
        document: core.projection_ref().clone(),
        frontier: new_frontier.clone(),
        version_vector: core.doc().state_vv().encode(),
      };
      core.push_editor_stream(gpui_flowtext::ProjectionStreamItem::Replace(Box::new(replace.document.clone())));
      LocalWriteOutcome::CommittedWithRebuild {
        commit: LocalCommit {
          patches: ProjectionPatchBatch {
            transaction_id,
            base_frontier,
            new_frontier: new_frontier.clone(),
            patches: Vec::new(),
          },
          frontier: new_frontier,
          version_vector: core.doc().state_vv().encode(),
          selection_after: selection_after(core, &summary),
          counters,
        },
        replace: Box::new(replace),
      }
    },
  };
  Ok(outcome)
}

/// Ops recorded in the pending (uncommitted) transaction — the source-of-truth
/// per-intent op count for the §11 complexity contract.
fn pending_op_count(doc: &LoroDoc) -> u64 {
  u64::try_from(doc.get_pending_txn_len()).unwrap_or(u64::MAX)
}

/// I-10 recovery. Order is load-bearing (semantics-audit F4):
/// 1. repair origin BEFORE `revert_to` — its internal `diff()` implicitly
///    commits the pending partial mutation, and that commit must be
///    undo-excluded;
/// 2. `revert_to` leaves the inverse ops uncommitted — finish with an explicit
///    repair-origin commit;
/// 3. both commits leave through ONE publish payload (`local_update_bytes`
///    exports the covering range in one blob), so no peer can import the
///    partial without its inverse.
fn compensate_failed_intent(
  core: &mut CrdtRuntime,
  frontier_before: &loro::Frontiers,
  vv_before: &loro::VersionVector,
  class: &'static str,
  error: &anyhow::Error,
) -> WriteRejected {
  tracing::error!(class, %error, "local intent failed mid-apply; compensating via revert_to (I-10)");
  core.doc().set_next_commit_origin("repair");
  core.doc().set_next_commit_message("intent-compensation");
  if let Err(revert_error) = core.doc().revert_to(frontier_before) {
    tracing::error!(class, %revert_error, "revert_to during intent compensation FAILED; core must be reloaded");
    return WriteRejected::CompensationFailed {
      class,
      diagnostic: format!("mid-apply error: {error:#}; revert_to error: {revert_error}"),
    };
  }
  // The inverse ops sit in the renewed pending txn — commit them under the
  // repair origin.
  core.doc().set_next_commit_origin("repair");
  core.doc().set_next_commit_message("intent-compensation-inverse");
  core.doc().commit();

  // One atomic publish payload covering partial + inverse: the export starts
  // from the PRE-mutation version vector, so the pair travels together and no
  // peer can import one half (spec I-10c).
  queue_publish_events(
    core,
    frontier_before.clone(),
    vv_before.clone(),
    flowstate_document_invalidation_full(core, "intent-compensation"),
  );

  // The projection never saw the partial mutation (no patches escaped), and the
  // doc is back at pre-intent state — but refresh defensively so any drifted
  // bookkeeping self-heals, and so the failure is observable in metrics.
  if let Err(refresh_error) = core.refresh_projection() {
    tracing::error!(class, %refresh_error, "projection refresh after compensation failed; core must be reloaded");
    return WriteRejected::CompensationFailed {
      class,
      diagnostic: format!("mid-apply error: {error:#}; post-compensation refresh error: {refresh_error:#}"),
    };
  }
  WriteRejected::CompensatedFailure {
    class,
    diagnostic: format!("{error:#}"),
  }
}

fn flowstate_document_invalidation_full(core: &CrdtRuntime, reason: &'static str) -> crate::crdt_runtime::ProjectionInvalidation {
  crate::crdt_runtime::ProjectionInvalidation::full_rebuild(
    core.projection_ref().frontier.clone(),
    core.doc().state_frontiers().encode(),
    reason,
  )
}

/// Produce the publish-side events (`LocalUpdate` bytes + persistence) for the
/// committed change and queue them for the I/O service to drain (spec §6).
#[hotpath::measure]
fn queue_publish_events(
  core: &mut CrdtRuntime,
  frontier_before: loro::Frontiers,
  vv_before: loro::VersionVector,
  invalidation: crate::crdt_runtime::ProjectionInvalidation,
) {
  match core.events_after_local_change(frontier_before, vv_before, invalidation, false) {
    Ok(events) => core.queue_publish(events),
    Err(error) => {
      // Export/persist failures must not fail the already-committed intent;
      // anti-entropy recovers the update. Loud.
      tracing::error!(%error, "exporting/persisting committed local intent update failed; anti-entropy must recover it");
    },
  }
}

/// Debug/CI audit (spec §7): the patch-applied projection must equal an
/// independent full materialization. Release sampling is wired by the handle's
/// config; this function is the debug-build always-on arm.
#[hotpath::measure]
fn audit_patched_projection(core: &mut CrdtRuntime, class: &'static str) {
  #[cfg(debug_assertions)]
  {
    if let Err(error) = core.audit_projection_against_full_rebuild(class) {
      // The audit itself repairs via snapshot fallback; surfacing here is for
      // test visibility.
      tracing::error!(class, %error, "audit-mismatch: patch-applied projection diverged from full rebuild");
    }
  }
  #[cfg(not(debug_assertions))]
  {
    let _ = (core, class);
  }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[hotpath::measure]
fn resolve_intent(core: &CrdtRuntime, intent: &LocalIntent) -> Result<ResolvedPlan, WriteRejected> {
  let doc = core.doc();
  let projection = core.projection_ref();
  let index = core.projection_index_ref();
  match intent {
    LocalIntent::InsertText(insert) => {
      if insert.text.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      if insert.text.contains('\n') || insert.text.contains(OBJECT_REPLACEMENT) {
        // Structure enters through split/object/fragment intents only; a text
        // insert is always intra-paragraph (keeps patch synthesis exact and
        // repair-free — spec I-8).
        return Err(WriteRejected::StructureViolation(
          "InsertText must not contain paragraph breaks or object placeholders; use SplitParagraph / InsertObject / InsertRichFragment",
        ));
      }
      Ok(ResolvedPlan::InsertText {
        at: resolve_text_anchor(doc, projection, index, &insert.at)?,
        text: insert.text.clone(),
        style_override: insert.style_override,
      })
    },
    LocalIntent::DeleteRange(delete) => {
      let (start, end) = resolve_text_range(doc, projection, index, &delete.start, &delete.end)?;
      if start.body_unicode == end.body_unicode {
        return Err(WriteRejected::EmptyIntent);
      }
      Ok(ResolvedPlan::DeleteRange { start, end })
    },
    LocalIntent::SplitParagraph(split) => Ok(ResolvedPlan::SplitParagraph {
      at: resolve_text_anchor(doc, projection, index, &split.at)?,
      inherited_style: split.inherited_style,
      new_paragraph: ParagraphId(Uuid::new_v4().as_u128()),
      new_block: BlockId(Uuid::new_v4().as_u128()),
    }),
    LocalIntent::JoinParagraphs(join) => {
      let first_ix = index
        .paragraph_index(join.first)
        .ok_or(WriteRejected::UnresolvedParagraph(join.first))?;
      let second_ix = index
        .paragraph_index(join.second)
        .ok_or(WriteRejected::UnresolvedParagraph(join.second))?;
      if second_ix != first_ix + 1 {
        return Err(WriteRejected::StructureViolation("JoinParagraphs requires adjacent paragraphs"));
      }
      Ok(ResolvedPlan::JoinParagraphs {
        first: join.first,
        second: join.second,
        first_ix,
      })
    },
    LocalIntent::SetMarks(marks) => {
      let (start, end) = resolve_text_range(doc, projection, index, &marks.start, &marks.end)?;
      if start.body_unicode == end.body_unicode {
        return Err(WriteRejected::EmptyIntent);
      }
      Ok(ResolvedPlan::SetMarks {
        start,
        end,
        styles: marks.styles,
      })
    },
    LocalIntent::SetParagraphStyle(style) => {
      let paragraph_ix = index
        .paragraph_index(style.paragraph)
        .ok_or(WriteRejected::UnresolvedParagraph(style.paragraph))?;
      // An interstitial paragraph (one that directly follows an object row)
      // has no boundary `\n` of its own — paragraph style is CARRIED by the
      // boundary mark, so its style is canonically inherited and cannot be
      // set: marking the nearest newline would restyle a DIFFERENT paragraph
      // (the object-fuzz caught exactly that as a maintained-vs-canonical
      // style divergence). Reject before mutation (I-15) until the schema
      // grows a per-record style field for boundary-less rows.
      let interstitial = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)
        .is_some_and(|row| row > 0 && !matches!(projection.blocks.get(row - 1), Some(flowstate_document::Block::Paragraph(_))));
      if interstitial {
        return Err(WriteRejected::StructureViolation(
          "paragraph style on an object-following paragraph is not representable (boundary-less row)",
        ));
      }
      Ok(ResolvedPlan::SetParagraphStyle {
        paragraph: style.paragraph,
        paragraph_ix,
        style: style.style,
      })
    },
    LocalIntent::SetParagraphStyles(styles) => {
      if styles.paragraphs.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      // Batched resolution (§11): stale identities and non-representable
      // (object-following, boundary-less) rows are SKIPPED, not rejected —
      // the surviving targets carry the user's intent, mirroring
      // ReplaceMatches. Rows resolve in one pass; boundaries resolve through
      // ONE batched cursor query (per-target `get_cursor_pos` is a linear
      // chunk scan — quadratic over a select-all restyle without this).
      let rows = flowstate_document::paragraph_block_rows(projection);
      let mut targets: Vec<(ParagraphId, usize)> = Vec::with_capacity(styles.paragraphs.len());
      for paragraph in &styles.paragraphs {
        let Some(paragraph_ix) = index.paragraph_index(*paragraph) else {
          continue;
        };
        if projection.ids.paragraph_ids.get(paragraph_ix).copied() != Some(*paragraph) {
          continue;
        }
        let interstitial = rows
          .get(paragraph_ix)
          .is_some_and(|&row| row > 0 && !matches!(projection.blocks.get(row - 1), Some(flowstate_document::Block::Paragraph(_))));
        if interstitial {
          continue;
        }
        targets.push((*paragraph, paragraph_ix));
      }
      if targets.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      let indices: Vec<usize> = targets.iter().map(|(_, paragraph_ix)| *paragraph_ix).collect();
      let boundaries = crate::crdt_runtime::paragraph_boundaries_loro_unicode_indices(doc, projection, &indices);
      Ok(ResolvedPlan::SetParagraphStyles {
        targets: targets
          .into_iter()
          .zip(boundaries)
          .map(|((paragraph, paragraph_ix), boundary)| (paragraph, paragraph_ix, boundary))
          .collect(),
        style: styles.style,
      })
    },
    LocalIntent::InsertObject(insert) => {
      let at = resolve_text_anchor(doc, projection, index, &insert.at)?;
      let block_ix = flowstate_document::block_ix_for_paragraph(projection, at.paragraph_ix)
        .map(|ix| if at.byte > 0 { ix + 1 } else { ix })
        .ok_or(WriteRejected::UnresolvedParagraph(insert.at.paragraph))?;
      Ok(ResolvedPlan::InsertObject {
        at,
        block_ix,
        new_block: BlockId(Uuid::new_v4().as_u128()),
        block: insert.block.clone(),
      })
    },
    LocalIntent::ReplaceObject(replace) => Ok(ResolvedPlan::ReplaceObject {
      block: replace.block,
      block_ix: index
        .block_index(replace.block)
        .ok_or(WriteRejected::UnresolvedBlock(replace.block))?,
      after: replace.after.clone(),
    }),
    LocalIntent::DeleteBlocks(delete) => {
      if delete.blocks.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      let blocks = delete
        .blocks
        .iter()
        .map(|block| {
          index
            .block_index(*block)
            .map(|ix| (*block, ix))
            .ok_or(WriteRejected::UnresolvedBlock(*block))
        })
        .collect::<Result<Vec<_>, _>>()?;
      Ok(ResolvedPlan::DeleteBlocks { blocks })
    },
    LocalIntent::MoveBlock(move_block) => {
      // Resolve-before-mutate (I-15): the moved block must exist even though
      // only the destination feeds the plan.
      index
        .block_index(move_block.block)
        .ok_or(WriteRejected::UnresolvedBlock(move_block.block))?;
      let to_ix = match move_block.before {
        Some(before) => index.block_index(before).ok_or(WriteRejected::UnresolvedBlock(before))?,
        None => projection.blocks.len(),
      };
      Ok(ResolvedPlan::MoveBlock {
        block: move_block.block,
        to_ix,
      })
    },
    LocalIntent::InsertRichFragment(fragment) => {
      if fragment.blocks.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      Ok(ResolvedPlan::InsertRichFragment {
        at: resolve_text_anchor(doc, projection, index, &fragment.at)?,
        blocks: fragment.blocks.clone(),
      })
    },
    LocalIntent::ReplaceMatches(replace) => {
      if replace.replacement.contains('\n') || replace.replacement.contains(OBJECT_REPLACEMENT) {
        return Err(WriteRejected::StructureViolation(
          "ReplaceMatches replacement must not contain paragraph breaks or object placeholders",
        ));
      }
      // Batched resolution (§11): a storm carries thousands of matches over
      // thousands of paragraphs, and per-anchor cursor resolution is a linear
      // chunk scan per call (the batch-resolver lesson: 27k matches on the
      // reference doc = 125s). Resolve each DISTINCT paragraph's body start
      // once through ONE batched query and derive every match position from
      // projection bytes; only anchors that carry explicit cursors (none from
      // the editor's replace-all) take the per-anchor path.
      let mut distinct: Vec<flowstate_document::ParagraphId> = Vec::new();
      let mut distinct_slots: std::collections::HashMap<flowstate_document::ParagraphId, usize> = std::collections::HashMap::new();
      for entry in &replace.matches {
        if entry.start.cursor.is_none() && entry.end.cursor.is_none() && entry.start.paragraph == entry.end.paragraph {
          distinct_slots.entry(entry.start.paragraph).or_insert_with(|| {
            distinct.push(entry.start.paragraph);
            distinct.len() - 1
          });
        }
      }
      let starts = crate::crdt_runtime::paragraph_body_starts_in_loro(doc, &distinct);
      let mut matches = Vec::with_capacity(replace.matches.len());
      for entry in &replace.matches {
        let batched = entry.start.cursor.is_none() && entry.end.cursor.is_none() && entry.start.paragraph == entry.end.paragraph;
        let (start, end) = if batched {
          // Stale identity ⇒ skip this match (not reject): the surviving
          // matches still carry the user's intent.
          let Some(paragraph_ix) = index.paragraph_index(entry.start.paragraph) else {
            continue;
          };
          if projection.ids.paragraph_ids.get(paragraph_ix).copied() != Some(entry.start.paragraph) {
            continue;
          }
          // Records without live cursors (freshly seeded paragraphs awaiting
          // repair) fall back to the index's maintained starts — in-sync with
          // the live doc inside the gate, same basis the per-anchor path uses.
          let paragraph_start = match starts[distinct_slots[&entry.start.paragraph]] {
            Some(start) => start,
            None => {
              let Some(start) = index.body_unicode_for_offset_in_loro(
                doc,
                projection,
                DocumentOffset {
                  paragraph: paragraph_ix,
                  byte: 0,
                },
              ) else {
                continue;
              };
              start
            },
          };
          let text = flowstate_document::paragraph_text(projection, paragraph_ix);
          let start_byte = super::resolve::clamp_byte_to_char_boundary(projection, paragraph_ix, entry.start.byte_hint);
          let end_byte = super::resolve::clamp_byte_to_char_boundary(projection, paragraph_ix, entry.end.byte_hint);
          let (start_byte, end_byte) = if start_byte <= end_byte { (start_byte, end_byte) } else { (end_byte, start_byte) };
          let start_unicode = paragraph_start + text[..start_byte].chars().count();
          let end_unicode = start_unicode + text[start_byte..end_byte].chars().count();
          (
            ResolvedTextPosition {
              paragraph_ix,
              byte: start_byte,
              body_unicode: start_unicode,
            },
            ResolvedTextPosition {
              paragraph_ix,
              byte: end_byte,
              body_unicode: end_unicode,
            },
          )
        } else {
          let Ok(range) = resolve_text_range(doc, projection, index, &entry.start, &entry.end) else {
            continue;
          };
          range
        };
        // Skip (not reject) matches that concurrent edits collapsed or moved
        // across a paragraph boundary — the surviving matches still carry the
        // user's intent (intent contract).
        if start.body_unicode == end.body_unicode || start.paragraph_ix != end.paragraph_ix {
          continue;
        }
        matches.push((start, end, entry.styles));
      }
      // Prune overlaps preferring the EARLIER match (search matches are
      // disjoint; an overlap means concurrency moved them, and the first
      // match is the user-visible winner), then flip to descending for
      // back-to-front application.
      matches.sort_by_key(|entry| entry.0.body_unicode);
      let mut kept: Vec<(ResolvedTextPosition, ResolvedTextPosition, Option<flowstate_document::RunStyles>)> =
        Vec::with_capacity(matches.len());
      for entry in matches {
        if kept.last().is_none_or(|prev| entry.0.body_unicode >= prev.1.body_unicode) {
          kept.push(entry);
        }
      }
      kept.reverse();
      if kept.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      Ok(ResolvedPlan::ReplaceMatches {
        matches: kept,
        replacement: replace.replacement.clone(),
      })
    },
    LocalIntent::ReplaceEquationSourceRange(eq) => {
      index
        .block_index(eq.equation)
        .ok_or(WriteRejected::UnresolvedBlock(eq.equation))?;
      Ok(ResolvedPlan::ReplaceEquationSourceRange {
        equation: eq.equation,
        range: eq.range.clone(),
        text: eq.text.clone(),
      })
    },
    LocalIntent::ReplaceImageAltText(img) => {
      index.block_index(img.image).ok_or(WriteRejected::UnresolvedBlock(img.image))?;
      Ok(ResolvedPlan::ReplaceImageAltText {
        image: img.image,
        text: img.text.clone(),
      })
    },
    LocalIntent::ReplaceImageCaption(img) => {
      index.block_index(img.image).ok_or(WriteRejected::UnresolvedBlock(img.image))?;
      Ok(ResolvedPlan::ReplaceImageCaption {
        image: img.image,
        caption: img.caption.clone(),
      })
    },
    LocalIntent::SetImageLayout(img) => {
      index.block_index(img.image).ok_or(WriteRejected::UnresolvedBlock(img.image))?;
      Ok(ResolvedPlan::SetImageLayout {
        image: img.image,
        sizing: img.sizing.clone(),
        alignment: img.alignment,
      })
    },
    LocalIntent::Table(table_intent) => resolve_table_intent(core, table_intent),
  }
}

fn resolve_table_intent(core: &CrdtRuntime, intent: &TableIntent) -> Result<ResolvedPlan, WriteRejected> {
  let index = core.projection_index_ref();
  let projection = core.projection_ref();
  let table = *match intent {
    TableIntent::InsertRow { table, .. }
    | TableIntent::DeleteRow { table, .. }
    | TableIntent::MoveRow { table, .. }
    | TableIntent::InsertColumn { table, .. }
    | TableIntent::DeleteColumn { table, .. }
    | TableIntent::MoveColumn { table, .. }
    | TableIntent::ReplaceCell { table, .. }
    | TableIntent::SetCellSpan { table, .. }
    | TableIntent::SetColumnWidth { table, .. } => table,
  };
  let table_ix = index.block_index(table).ok_or(WriteRejected::UnresolvedBlock(table))?;
  let table_block = match projection.blocks.get(table_ix) {
    Some(flowstate_document::Block::Table(block)) => block,
    _ => {
      return Err(WriteRejected::UnresolvedTableEntity {
        table,
        detail: "projection block is not a table".to_string(),
      });
    },
  };
  let op = match intent {
    TableIntent::InsertRow { after_row, row, .. } => ResolvedTableOp::InsertRow {
      new_row: row.clone(),
      after_row: *after_row,
    },
    TableIntent::DeleteRow { row, .. } => {
      require_row(table_block, table, *row)?;
      ResolvedTableOp::DeleteRow { row: *row }
    },
    TableIntent::MoveRow { row, after_row, .. } => {
      require_row(table_block, table, *row)?;
      ResolvedTableOp::MoveRow {
        row: *row,
        after_row: *after_row,
      }
    },
    TableIntent::InsertColumn { after_column, width, .. } => {
      let new_column = gpui_flowtext::ColumnId(Uuid::new_v4().as_u128());
      let cells = table_block
        .rows
        .iter()
        .map(|row| crate::crdt_runtime::empty_input_table_cell(row.id, new_column))
        .collect();
      ResolvedTableOp::InsertColumn {
        new_column,
        after_column: *after_column,
        width: width.clone(),
        cells,
      }
    },
    TableIntent::DeleteColumn { column, .. } => {
      require_column(table_block, table, *column)?;
      ResolvedTableOp::DeleteColumn { column: *column }
    },
    TableIntent::MoveColumn { column, after_column, .. } => {
      require_column(table_block, table, *column)?;
      ResolvedTableOp::MoveColumn {
        column: *column,
        after_column: *after_column,
      }
    },
    TableIntent::ReplaceCell { row, column, cell, .. } => {
      require_row(table_block, table, *row)?;
      require_column(table_block, table, *column)?;
      ResolvedTableOp::ReplaceCell {
        row: *row,
        column: *column,
        cell: cell.clone(),
      }
    },
    TableIntent::SetCellSpan {
      row,
      column,
      row_span,
      column_span,
      ..
    } => {
      require_row(table_block, table, *row)?;
      require_column(table_block, table, *column)?;
      ResolvedTableOp::SetCellSpan {
        row: *row,
        column: *column,
        row_span: *row_span,
        column_span: *column_span,
      }
    },
    TableIntent::SetColumnWidth { column, width, .. } => {
      let column_ix = require_column(table_block, table, *column)?;
      ResolvedTableOp::SetColumnWidth {
        column_ix,
        width: width.clone(),
      }
    },
  };
  Ok(ResolvedPlan::Table { table, table_ix, op })
}

fn require_row(table: &flowstate_document::TableBlock, table_id: BlockId, row: gpui_flowtext::RowId) -> Result<usize, WriteRejected> {
  table
    .rows
    .iter()
    .position(|candidate| candidate.id == row)
    .ok_or(WriteRejected::UnresolvedTableEntity {
      table: table_id,
      detail: format!("row {} not present", row.0),
    })
}

fn require_column(table: &flowstate_document::TableBlock, table_id: BlockId, column: gpui_flowtext::ColumnId) -> Result<usize, WriteRejected> {
  table
    .columns
    .iter()
    .position(|candidate| candidate.id == column)
    .ok_or(WriteRejected::UnresolvedTableEntity {
      table: table_id,
      detail: format!("column {} not present", column.0),
    })
}

// ---------------------------------------------------------------------------
// Mutation
// ---------------------------------------------------------------------------

#[hotpath::measure]
fn execute_plan(core: &mut CrdtRuntime, plan: &ResolvedPlan) -> Result<MutationSummary> {
  let doc = core.doc().clone();
  let projection = hotpath::measure_block!("execute_plan_projection_clone", core.projection_ref().clone());
  let mut summary = MutationSummary::default();
  match plan {
    ResolvedPlan::InsertText { at, text, style_override } => {
      let body = body_text(&doc);
      body
        .insert(at.body_unicode, text)
        .context("inserting intent text into Loro body flow")?;
      summary.containers_touched = 1;
      let inserted = text.chars().count();
      // Style inheritance is expand-`After`'s job (spec §9); an override marks
      // exactly the inserted range.
      if let Some(styles) = style_override
        && inserted > 0
      {
        mark_run_styles(&body, at.body_unicode..at.body_unicode + inserted, *styles).context("marking caret style override")?;
        summary.marks_emitted = style_mark_count(*styles);
      }
      summary.caret_body_unicode = Some(at.body_unicode + inserted);
    },
    ResolvedPlan::DeleteRange { start, end } => {
      let body = body_text(&doc);
      let len = end.body_unicode - start.body_unicode;
      let Some((clamped_start, clamped_len)) = sentinel_protected_delete_range(start.body_unicode, len) else {
        anyhow::bail!("delete range collapsed by sentinel protection");
      };
      hotpath::measure_block!("delete_range_body_delete", {
        body
          .delete(clamped_start, clamped_len)
          .context("deleting intent range from Loro body flow")?;
      });
      summary.containers_touched = 1;
      // Cross-paragraph deletes remove newline boundaries: orphaned object
      // blocks and dead paragraph records must be retired with the text. The
      // dead set is known exactly from the resolved plan — every paragraph
      // after the first absorbs into it — so retirement is O(edit), not the
      // O(records) prune sweep (§11).
      if start.paragraph_ix != end.paragraph_ix {
        // The prune sweep stringifies the whole body (O(doc) + a multi-MB
        // allocation); only a delete whose PRE-STATE range covered an object
        // placeholder can orphan an object block, so consult the maintained
        // placeholder index first — the common cross-paragraph TEXT delete
        // skips the sweep entirely (measured 66ms per delete on the
        // reference doc).
        let object_in_range = {
          let positions = core.projection_index_ref().object_positions();
          let from = positions.partition_point(|&pos| pos < clamped_start);
          positions.get(from).is_some_and(|&pos| pos < clamped_start + clamped_len)
        };
        if object_in_range {
          hotpath::measure_block!("delete_range_prune_objects", {
            prune_orphaned_body_object_blocks(&doc, &body).context("pruning orphaned object blocks after cross-paragraph delete")?;
          });
        }
        hotpath::measure_block!("delete_range_retire_records", {
          for paragraph_ix in (start.paragraph_ix + 1)..=end.paragraph_ix {
            let (Some(paragraph_id), Some(block_id)) = (
              projection.ids.paragraph_ids.get(paragraph_ix).copied(),
              flowstate_document::block_ix_for_paragraph(&projection, paragraph_ix).and_then(|ix| projection.ids.block_ids.get(ix).copied()),
            ) else {
              continue;
            };
            delete_projection_paragraph_metadata(&doc, paragraph_id, block_id)
              .context("retiring merged-away paragraph records after cross-paragraph delete")?;
          }
        });
      }
      summary.caret_body_unicode = Some(clamped_start);
    },
    ResolvedPlan::SplitParagraph {
      at,
      inherited_style,
      new_paragraph,
      new_block,
    } => {
      let body = body_text(&doc);
      body.insert(at.body_unicode, "\n").context("inserting split boundary")?;
      body
        .mark(at.body_unicode..at.body_unicode + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(*inherited_style))
        .context("marking split paragraph style")?;
      // Sentinel hygiene (spec §9): expand-`After` run marks ending exactly at
      // the split point absorb the inserted newline; strip run-style keys from
      // the sentinel so styling never bleeds across the boundary.
      unmark_run_style_keys(&body, at.body_unicode..at.body_unicode + 1)?;
      summary.marks_emitted = 1;
      repair_paragraph_metadata_after_stable_split(&doc, &body, at.body_unicode, *new_paragraph, *new_block, "local_split_paragraph")
        .context("writing split paragraph records")?;
      summary.containers_touched = 3; // body text + paragraph record + block record
      summary.caret_body_unicode = Some(at.body_unicode + 1);
    },
    ResolvedPlan::JoinParagraphs { first, second, .. } => {
      if !join_projection_paragraphs(&doc, &projection, *first, *second).context("joining paragraphs")? {
        anyhow::bail!("join mutation reported no-op for resolved adjacent paragraphs");
      }
      summary.containers_touched = 2;
    },
    ResolvedPlan::SetMarks { start, end, styles } => {
      let body = body_text(&doc);
      mark_run_styles(&body, start.body_unicode..end.body_unicode, *styles).context("marking style range")?;
      summary.containers_touched = 1;
      summary.marks_emitted = style_mark_count(*styles);
    },
    ResolvedPlan::SetParagraphStyle { paragraph_ix, style, .. } => {
      let boundary = paragraph_boundary_loro_unicode_index(&doc, &projection, *paragraph_ix);
      body_text(&doc)
        .mark(boundary..boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))
        .context("marking paragraph style")?;
      summary.containers_touched = 1;
      summary.marks_emitted = 1;
    },
    ResolvedPlan::SetParagraphStyles { targets, style } => {
      let body = body_text(&doc);
      let value = paragraph_style_value(*style);
      for (_, _, boundary) in targets {
        body
          .mark(*boundary..*boundary + 1, MARK_PARAGRAPH_STYLE, value)
          .context("marking paragraph style (batched)")?;
      }
      summary.containers_touched = 1;
      summary.marks_emitted = u32::try_from(targets.len()).unwrap_or(u32::MAX);
    },
    ResolvedPlan::InsertObject {
      block_ix, new_block, block, ..
    } => {
      if !insert_projection_object_block(&doc, &projection, *new_block, *block_ix, block).context("inserting object block")? {
        anyhow::bail!("object insert reported no-op for freshly minted block id");
      }
      summary.containers_touched = 2;
    },
    ResolvedPlan::ReplaceObject { block, block_ix, after } => {
      if !replace_projection_object_block(&doc, &projection, Some(*block), *block_ix, after).context("replacing object block")? {
        anyhow::bail!("object replace reported no-op for resolved block");
      }
      summary.containers_touched = 2;
    },
    ResolvedPlan::DeleteBlocks { blocks } => {
      for (block, _) in blocks {
        if !delete_projection_object_block(&doc, *block).context("deleting object block")? {
          anyhow::bail!("object delete reported no-op for resolved block {}", block.0);
        }
      }
      summary.containers_touched = u32::try_from(blocks.len()).unwrap_or(u32::MAX);
    },
    ResolvedPlan::MoveBlock { block, to_ix, .. } => {
      if !move_projection_object_block(&doc, &projection, *block, *to_ix).context("moving object block")? {
        anyhow::bail!("object move reported no-op for resolved block");
      }
      summary.containers_touched = 1;
    },
    ResolvedPlan::InsertRichFragment { at, blocks } => {
      summary.containers_touched = execute_rich_fragment(&doc, &projection, at, blocks)?;
      summary.caret_body_unicode = None; // computed from patch plan by caller policy
    },
    ResolvedPlan::ReplaceEquationSourceRange { equation, range, text } => {
      if !replace_projection_equation_source_range(&doc, *equation, range, text).context("replacing equation source range")? {
        anyhow::bail!("equation source replace reported no-op for resolved block");
      }
      summary.containers_touched = 1;
    },
    ResolvedPlan::ReplaceImageAltText { image, text } => {
      if !replace_projection_image_alt_text(&doc, *image, text).context("replacing image alt text")? {
        anyhow::bail!("image alt-text replace reported no-op for resolved block");
      }
      summary.containers_touched = 1;
    },
    ResolvedPlan::ReplaceImageCaption { image, caption } => {
      if !replace_projection_image_caption(&doc, *image, caption.as_ref()).context("replacing image caption")? {
        anyhow::bail!("image caption replace reported no-op for resolved block");
      }
      summary.containers_touched = 1;
    },
    ResolvedPlan::SetImageLayout { image, sizing, alignment } => {
      if !set_projection_image_layout(&doc, *image, sizing, *alignment).context("setting image layout")? {
        anyhow::bail!("image layout set reported no-op for resolved block");
      }
      summary.containers_touched = 1;
    },
    ResolvedPlan::Table { table, op, .. } => {
      summary.containers_touched = execute_table_op(&doc, *table, op)?;
    },
    ResolvedPlan::ReplaceMatches { matches, replacement } => {
      let body = body_text(&doc);
      let replacement_chars = replacement.chars().count();
      // Descending order (resolution contract): applying back-to-front means
      // no earlier range's position shifts.
      for (start, end, styles) in matches {
        let len = end.body_unicode - start.body_unicode;
        hotpath::measure_block!("replace_matches_body_edit", {
          body
            .delete(start.body_unicode, len)
            .context("deleting matched range from Loro body flow")?;
          if replacement_chars > 0 {
            body
              .insert(start.body_unicode, replacement)
              .context("inserting replacement into Loro body flow")?;
          }
        });
        if replacement_chars > 0
          && let Some(styles) = styles
        {
          mark_run_styles(&body, start.body_unicode..start.body_unicode + replacement_chars, *styles)
            .context("marking replacement run styles")?;
          summary.marks_emitted += style_mark_count(*styles);
        }
      }
      summary.containers_touched = 1;
      // First entry = LAST match in document order; the caret lands after its
      // replacement (find/replace UX contract).
      summary.caret_body_unicode = matches.first().map(|(start, _, _)| start.body_unicode + replacement_chars);
    },
  }
  Ok(summary)
}

/// Test-only deterministic fault: when set, `execute_rich_fragment` fails
/// after its first block — the §13.6 compound-intent atomicity proof.
#[cfg(test)]
pub(crate) static INJECT_FRAGMENT_FAULT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Compound fragment insertion: paragraphs splice into body text (with
/// boundaries + styles + marks), objects insert at their positions. One gate
/// hold, one commit; a failure anywhere trips I-10 compensation in the caller.
fn execute_rich_fragment(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  at: &ResolvedTextPosition,
  blocks: &[FragmentBlock],
) -> Result<u32> {
  let body = body_text(doc);
  let mut containers: u32 = 1;
  let mut cursor_unicode = at.body_unicode;
  let mut first = true;
  for (fragment_block_ix, block) in blocks.iter().enumerate() {
    #[cfg(test)]
    if fragment_block_ix > 0 && INJECT_FRAGMENT_FAULT.load(std::sync::atomic::Ordering::SeqCst) {
      anyhow::bail!("injected fragment fault (test)");
    }
    #[cfg(not(test))]
    let _ = fragment_block_ix;
    match block {
      FragmentBlock::Paragraph(paragraph) => {
        if !first {
          // New paragraph boundary before this fragment paragraph.
          body.insert(cursor_unicode, "\n").context("inserting fragment paragraph boundary")?;
          body
            .mark(cursor_unicode..cursor_unicode + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))
            .context("marking fragment paragraph style")?;
          unmark_run_style_keys(&body, cursor_unicode..cursor_unicode + 1)?;
          let new_paragraph = ParagraphId(Uuid::new_v4().as_u128());
          let new_block = BlockId(Uuid::new_v4().as_u128());
          repair_paragraph_metadata_after_stable_split(doc, &body, cursor_unicode, new_paragraph, new_block, "local_fragment_paragraph")
            .context("writing fragment paragraph records")?;
          containers = containers.saturating_add(2);
          cursor_unicode += 1;
        }
        for run in &paragraph.runs {
          if run.text.is_empty() {
            continue;
          }
          let len = run.text.chars().count();
          body
            .insert(cursor_unicode, &run.text)
            .context("inserting fragment run text")?;
          mark_run_styles(&body, cursor_unicode..cursor_unicode + len, run.styles).context("marking fragment run styles")?;
          cursor_unicode += len;
        }
        first = false;
      },
      FragmentBlock::Object(input) => {
        let new_block = BlockId(Uuid::new_v4().as_u128());
        // Objects sit between paragraphs; land it at the current position.
        crate::crdt_runtime::insert_input_object_block(doc, cursor_unicode, new_block, input)
          .context("inserting fragment object block")?;
        containers = containers.saturating_add(2);
        cursor_unicode += 1;
        first = false;
      },
    }
  }
  let _ = projection;
  Ok(containers)
}

fn execute_table_op(doc: &LoroDoc, table: BlockId, op: &ResolvedTableOp) -> Result<u32> {
  let applied = match op {
    ResolvedTableOp::InsertRow { new_row, after_row } => table_ops::insert_table_row(doc, table, new_row.id, *after_row, new_row)?,
    ResolvedTableOp::DeleteRow { row } => table_ops::delete_table_row(doc, table, *row)?,
    ResolvedTableOp::MoveRow { row, after_row } => table_ops::move_table_row(doc, table, *row, *after_row)?,
    ResolvedTableOp::InsertColumn {
      new_column,
      after_column,
      width,
      cells,
    } => table_ops::insert_table_column(doc, table, *new_column, *after_column, width, cells)?,
    ResolvedTableOp::DeleteColumn { column } => table_ops::delete_table_column(doc, table, *column)?,
    ResolvedTableOp::MoveColumn { column, after_column } => table_ops::move_table_column(doc, table, *column, *after_column)?,
    ResolvedTableOp::ReplaceCell { row, column, cell } => table_ops::replace_table_cell(doc, table, *row, *column, cell)?,
    ResolvedTableOp::SetCellSpan {
      row,
      column,
      row_span,
      column_span,
    } => table_ops::set_table_cell_span(doc, table, *row, *column, *row_span, *column_span)?,
    ResolvedTableOp::SetColumnWidth { column_ix, width, .. } => table_ops::set_table_column_width(doc, table, *column_ix, width)?,
  };
  if !applied {
    anyhow::bail!("table op reported no-op for resolved entities");
  }
  Ok(2)
}

/// Strip run-style mark keys from a range (split-sentinel hygiene, spec §9).
fn unmark_run_style_keys(body: &loro::LoroText, range: std::ops::Range<usize>) -> Result<()> {
  for key in [MARK_RUN_SEMANTIC_STYLE, MARK_HIGHLIGHT_STYLE, MARK_DIRECT_UNDERLINE, MARK_STRIKETHROUGH] {
    body
      .unmark(range.clone(), key)
      .with_context(|| format!("unmarking run-style key {key} on split sentinel"))?;
  }
  Ok(())
}

fn style_mark_count(styles: flowstate_document::RunStyles) -> u32 {
  // One mark op per style key the mark writer touches; RunStyles is a small
  // fixed set, so this is a constant bound rather than an exact per-key count.
  let _ = styles;
  4
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Cursor-backed selection after the intent (spec §8): anchor the character at
/// the caret's live body position and let the app-side delta express
/// stickiness.
#[hotpath::measure]
fn selection_after(core: &CrdtRuntime, summary: &MutationSummary) -> Option<SelectionSnapshot> {
  let caret_unicode = summary.caret_body_unicode?;
  let body = body_text(core.doc());
  let cursor = cursor_for_boundary(&body, caret_unicode, crate::presence::SelectionAffinity::Neutral)?;
  let offset = core
    .projection_index_ref()
    .offset_for_body_unicode(core.projection_ref(), caret_unicode)
    .unwrap_or(DocumentOffset { paragraph: 0, byte: 0 });
  let endpoint = CursorEndpoint {
    cursor: cursor.encode(),
    delta: 0,
    affinity: SelectionAffinity::Neutral,
    gravity: VisualGravity::Neutral,
    offset,
  };
  Some(SelectionSnapshot {
    anchor: endpoint.clone(),
    head: endpoint,
  })
}

