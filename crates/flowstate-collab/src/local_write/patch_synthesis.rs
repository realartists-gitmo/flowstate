//! Intent-exact projection patch synthesis (spec §7, D4).
//!
//! Called after the intent's mutations have committed, with the core's
//! projection still at its PRE-intent state (patches apply onto it) and the
//! doc at its POST-intent state. Every patch is O(touched text/blocks); the
//! only intent class permitted to fall back to a full rebuild is the compound
//! rich-fragment insert, and that fallback is loud and counted
//! (`full-rebuild-after-local-write`).

use flowstate_document::{
  DocumentProjection, InputBlock, ParagraphStyle, ProjectionPatch, ProjectionStructuralBlock, input_block_from_block, paragraph_text_len,
};

use super::commit::{ResolvedPlan, ResolvedTableOp};
use super::intents::LocalIntent;
use super::resolve::ResolvedTextPosition;
use crate::crdt_runtime::{
  CrdtRuntime, ProjectionInvalidation, body_input_paragraph_at, object_replacement_patch, paragraph_boundary_loro_unicode_index,
  projection_text_delta, text_delta_between,
};

/// What the projection should do for a committed intent.
pub(crate) enum PatchPlan {
  Patches {
    patches: Vec<ProjectionPatch>,
    invalidation: ProjectionInvalidation,
  },
  FullRebuild {
    invalidation: ProjectionInvalidation,
    reason: &'static str,
  },
}

#[hotpath::measure]
pub(crate) fn synthesize_patches(core: &CrdtRuntime, intent: &LocalIntent, plan: &ResolvedPlan) -> PatchPlan {
  let projection = core.projection_ref();
  let doc = core.doc();
  let frontier_before = projection.frontier.clone();
  let frontier_after = doc.state_frontiers().encode();
  let body_invalidation = |unicode_start: usize, unicode_len: usize| {
    ProjectionInvalidation::body_text(frontier_before.clone(), frontier_after.clone(), unicode_start, unicode_len)
  };
  let rebuild = |reason: &'static str| PatchPlan::FullRebuild {
    invalidation: ProjectionInvalidation::full_rebuild(frontier_before.clone(), frontier_after.clone(), reason),
    reason,
  };

  let patches = match plan {
    ResolvedPlan::InsertText { at, text, .. } => {
      let Some(row) = flowstate_document::block_ix_for_paragraph(projection, at.paragraph_ix) else {
        return rebuild("insert-text-block-missing");
      };
      let old_len = projection
        .paragraphs
        .get(at.paragraph_ix)
        .map(paragraph_text_len)
        .unwrap_or(0);
      let Some((paragraph_start, old_chars)) = resolved_paragraph_span(projection, at) else {
        return rebuild("insert-text-position-misaligned");
      };
      let end = paragraph_start + old_chars + text.chars().count();
      let Some(new) = body_input_paragraph_at(
        doc,
        paragraph_start.saturating_sub(1),
        end,
        paragraph_style_at(projection, at.paragraph_ix),
      ) else {
        return rebuild("insert-text-readback-missing");
      };
      Some((
        vec![ProjectionPatch::ParagraphText {
          block_id: projection.ids.block_ids[row],
          paragraph_id: projection.ids.paragraph_ids[at.paragraph_ix],
          row_hint: row,
          new,
          delta_utf8: projection_text_delta(at.byte, 0, text.len(), old_len.saturating_sub(at.byte)),
        }],
        body_invalidation(at.body_unicode, text.chars().count()),
      ))
    },
    ResolvedPlan::DeleteRange { start, end } => {
      let Some(row) = flowstate_document::block_ix_for_paragraph(projection, start.paragraph_ix) else {
        return rebuild("delete-range-block-missing");
      };
      let Some((paragraph_start, first_old_chars)) = resolved_paragraph_span(projection, start) else {
        return rebuild("delete-range-position-misaligned");
      };
      // Post-delete merged/truncated paragraph end: same-paragraph deletes
      // shrink the paragraph by the deleted span; cross-paragraph deletes
      // leave the first paragraph's prefix plus the last paragraph's tail.
      let range_end = if start.paragraph_ix == end.paragraph_ix {
        let deleted = end.body_unicode.saturating_sub(start.body_unicode);
        let Some(remaining) = first_old_chars.checked_sub(deleted) else {
          return rebuild("delete-range-length-misaligned");
        };
        paragraph_start + remaining
      } else {
        let last_text = flowstate_document::paragraph_text(projection, end.paragraph_ix);
        let Some(tail_chars) = last_text.get(end.byte..).map(|tail| tail.chars().count()) else {
          return rebuild("delete-range-tail-misaligned");
        };
        let prefix_chars = start.body_unicode - paragraph_start;
        paragraph_start + prefix_chars + tail_chars
      };
      let Some(new) = body_input_paragraph_at(
        doc,
        paragraph_start.saturating_sub(1),
        range_end,
        paragraph_style_at(projection, start.paragraph_ix),
      ) else {
        return rebuild("delete-range-readback-missing");
      };
      let old_first_len = projection
        .paragraphs
        .get(start.paragraph_ix)
        .map(paragraph_text_len)
        .unwrap_or(0);
      let mut patches = Vec::new();
      if start.paragraph_ix == end.paragraph_ix {
        patches.push(ProjectionPatch::ParagraphText {
          block_id: projection.ids.block_ids[row],
          paragraph_id: projection.ids.paragraph_ids[start.paragraph_ix],
          row_hint: row,
          new,
          delta_utf8: projection_text_delta(start.byte, end.byte.saturating_sub(start.byte), 0, old_first_len.saturating_sub(end.byte)),
        });
      } else {
        // Cross-paragraph delete: the first paragraph absorbs the tail of the
        // last; every whole paragraph/object block strictly inside the range
        // is deleted.
        let merged_len = input_paragraph_text_len(&new);
        patches.push(ProjectionPatch::ParagraphText {
          block_id: projection.ids.block_ids[row],
          paragraph_id: projection.ids.paragraph_ids[start.paragraph_ix],
          row_hint: row,
          new,
          delta_utf8: projection_text_delta(
            start.byte,
            old_first_len.saturating_sub(start.byte),
            merged_len.saturating_sub(start.byte),
            0,
          ),
        });
        let Some(first_block_ix) = flowstate_document::block_ix_for_paragraph(projection, start.paragraph_ix) else {
          return rebuild("delete-range-first-block-missing");
        };
        let Some(last_block_ix) = flowstate_document::block_ix_for_paragraph(projection, end.paragraph_ix) else {
          return rebuild("delete-range-last-block-missing");
        };
        let removed: Vec<_> = ((first_block_ix + 1)..=last_block_ix)
          .filter_map(|ix| projection.ids.block_ids.get(ix).copied())
          .collect();
        if !removed.is_empty() {
          patches.push(ProjectionPatch::DeleteBlocks {
            block_ids: removed,
            row_hint: first_block_ix + 1,
          });
        }
      }
      Some((
        patches,
        body_invalidation(start.body_unicode, end.body_unicode.saturating_sub(start.body_unicode)),
      ))
    },
    ResolvedPlan::SplitParagraph {
      at,
      inherited_style,
      new_paragraph,
      new_block,
    } => {
      let Some(source_block_ix) = flowstate_document::block_ix_for_paragraph(projection, at.paragraph_ix) else {
        return rebuild("split-source-block-missing");
      };
      let old_len = projection
        .paragraphs
        .get(at.paragraph_ix)
        .map(paragraph_text_len)
        .unwrap_or(0);
      let Some((paragraph_start, old_chars)) = resolved_paragraph_span(projection, at) else {
        return rebuild("split-position-misaligned");
      };
      // Post-split: `[start, at)` is the truncated source; the inserted `\n`
      // at `at.body_unicode` is the tail's leading sentinel (explicitly marked
      // with the inherited style by the mutation).
      let split_chars = at.body_unicode - paragraph_start;
      let source_style = paragraph_style_at(projection, at.paragraph_ix);
      let (Some(truncated), Some(tail)) = (
        body_input_paragraph_at(doc, paragraph_start.saturating_sub(1), at.body_unicode, source_style),
        body_input_paragraph_at(
          doc,
          at.body_unicode,
          at.body_unicode + 1 + (old_chars.saturating_sub(split_chars)),
          *inherited_style,
        ),
      ) else {
        return rebuild("split-readback-missing");
      };
      Some((
        vec![
          ProjectionPatch::ParagraphText {
            block_id: projection.ids.block_ids[source_block_ix],
            paragraph_id: projection.ids.paragraph_ids[at.paragraph_ix],
            row_hint: source_block_ix,
            new: truncated,
            delta_utf8: projection_text_delta(at.byte, old_len.saturating_sub(at.byte), 0, 0),
          },
          ProjectionPatch::InsertBlocks {
            before: projection.ids.block_ids.get(source_block_ix + 1).copied(),
            row_hint: source_block_ix + 1,
            blocks: vec![ProjectionStructuralBlock {
              block_id: *new_block,
              paragraph_id: Some(*new_paragraph),
              block: InputBlock::Paragraph(tail),
            }],
          },
        ],
        body_invalidation(at.body_unicode, 1),
      ))
    },
    ResolvedPlan::JoinParagraphs { first, second, first_ix, .. } => {
      let Some(first_block_ix) = flowstate_document::block_ix_for_paragraph(projection, *first_ix) else {
        return rebuild("join-first-block-missing");
      };
      let Some(second_block_ix) = flowstate_document::block_ix_for_paragraph(projection, first_ix + 1) else {
        return rebuild("join-second-block-missing");
      };
      // Post-join merged range: first paragraph's text, any object
      // placeholders that sat between the two blocks (folded out of paragraph
      // text by the readback), then the second paragraph's text.
      let sentinel = paragraph_boundary_loro_unicode_index(doc, projection, *first_ix);
      let first_chars = flowstate_document::paragraph_text(projection, *first_ix)
        .chars()
        .count();
      let second_chars = flowstate_document::paragraph_text(projection, first_ix + 1)
        .chars()
        .count();
      let objects_between = second_block_ix
        .saturating_sub(first_block_ix)
        .saturating_sub(1);
      let merged_end = sentinel + 1 + first_chars + objects_between + second_chars;
      let Some(merged) = body_input_paragraph_at(doc, sentinel, merged_end, paragraph_style_at(projection, *first_ix)) else {
        return rebuild("join-readback-missing");
      };
      let old_len = projection
        .paragraphs
        .get(*first_ix)
        .map(paragraph_text_len)
        .unwrap_or(0);
      let merged_len = input_paragraph_text_len(&merged);
      let second_block = projection.ids.block_ids.get(second_block_ix).copied();
      let mut patches = vec![ProjectionPatch::ParagraphText {
        block_id: projection.ids.block_ids[first_block_ix],
        paragraph_id: *first,
        row_hint: first_block_ix,
        new: merged,
        delta_utf8: projection_text_delta(old_len, 0, merged_len.saturating_sub(old_len), 0),
      }];
      if let Some(second_block) = second_block {
        patches.push(ProjectionPatch::DeleteBlocks {
          block_ids: vec![second_block],
          row_hint: second_block_ix,
        });
      }
      let _ = second;
      Some((patches, body_invalidation(0, 0)))
    },
    ResolvedPlan::SetMarks { start, end, .. } => {
      // One ParagraphRuns patch per touched paragraph — O(changed range).
      // Marks never change text, so paragraph starts chain forward from the
      // resolved start position: next start = start + chars + boundary +
      // object placeholders between the consecutive paragraph blocks.
      let Some((mut paragraph_start, _)) = resolved_paragraph_span(projection, start) else {
        return rebuild("set-marks-position-misaligned");
      };
      // Rows for every paragraph in one O(blocks) pass — the loop below needs
      // both `row` and the following paragraph's row, and calling
      // `block_ix_for_paragraph` twice per iteration is the §perf-heaven T2
      // quadratic on object docs.
      let rows = flowstate_document::paragraph_block_rows(projection);
      let mut patches = Vec::new();
      for paragraph_ix in start.paragraph_ix..=end.paragraph_ix {
        let Some(&row) = rows.get(paragraph_ix) else {
          return rebuild("set-marks-block-missing");
        };
        let chars = flowstate_document::paragraph_text(projection, paragraph_ix)
          .chars()
          .count();
        let Some(new) = body_input_paragraph_at(
          doc,
          paragraph_start.saturating_sub(1),
          paragraph_start + chars,
          paragraph_style_at(projection, paragraph_ix),
        ) else {
          return rebuild("set-marks-readback-missing");
        };
        if paragraph_ix < end.paragraph_ix {
          let Some(&next_row) = rows.get(paragraph_ix + 1) else {
            return rebuild("set-marks-next-block-missing");
          };
          paragraph_start += chars + 1 + next_row.saturating_sub(row).saturating_sub(1);
        }
        let runs = flowstate_document::document_from_input_blocks(projection.theme.clone(), vec![InputBlock::Paragraph(new)])
          .paragraphs
          .first()
          .map(|paragraph| paragraph.runs.clone());
        let Some(runs) = runs else {
          return rebuild("set-marks-runs-missing");
        };
        patches.push(ProjectionPatch::ParagraphRuns {
          block_id: projection.ids.block_ids[row],
          paragraph_id: projection.ids.paragraph_ids[paragraph_ix],
          row_hint: row,
          runs,
        });
      }
      Some((
        patches,
        body_invalidation(start.body_unicode, end.body_unicode.saturating_sub(start.body_unicode)),
      ))
    },
    ResolvedPlan::SetParagraphStyle {
      paragraph,
      paragraph_ix,
      style,
    } => {
      let Some(row) = flowstate_document::block_ix_for_paragraph(projection, *paragraph_ix) else {
        return rebuild("set-paragraph-style-block-missing");
      };
      Some((
        vec![ProjectionPatch::ParagraphStyle {
          block_id: projection.ids.block_ids[row],
          paragraph_id: *paragraph,
          row_hint: row,
          style: *style,
        }],
        body_invalidation(0, 0),
      ))
    },
    ResolvedPlan::SetParagraphStyles { targets, style } => {
      // One exact ParagraphStyle patch per target; style marks never change
      // text, so no shift bookkeeping. Rows come from one O(doc) pass, not an
      // O(doc) scan per target.
      let rows = flowstate_document::paragraph_block_rows(projection);
      let mut patches = Vec::with_capacity(targets.len());
      for (paragraph, paragraph_ix, _) in targets {
        let Some(&row) = rows.get(*paragraph_ix) else {
          return rebuild("set-paragraph-styles-block-missing");
        };
        patches.push(ProjectionPatch::ParagraphStyle {
          block_id: projection.ids.block_ids[row],
          paragraph_id: *paragraph,
          row_hint: row,
          style: *style,
        });
      }
      Some((patches, body_invalidation(0, 0)))
    },
    ResolvedPlan::InsertObject {
      at,
      block_ix,
      new_block,
      block,
      ..
    } => {
      // Whitelist: the exact patch is only valid for the ONE shape where no
      // identity re-derives — a placeholder landing AFTER a non-empty
      // paragraph's text (byte == text_len > 0) whose next block is another
      // PARAGRAPH (a real `\n` boundary right behind the insertion). Every
      // other shape re-derives identities under the materializer law: byte-0
      // and mid-text inserts create boundary-less interstitial rows, inserts
      // into empty paragraphs coalesce the row into the object, end-of-doc
      // inserts grow a trailing fabricated row, and object-cluster inserts
      // shift interstitial anchors. Loud rebuild for all of those (rare,
      // explicit UI ops; found by the object-fuzz undo arm).
      let text_len = projection
        .paragraphs
        .get(at.paragraph_ix)
        .map(paragraph_text_len)
        .unwrap_or(0);
      let followed_by_paragraph = flowstate_document::block_ix_for_paragraph(projection, at.paragraph_ix)
        .and_then(|row| projection.blocks.get(row + 1))
        .is_some_and(|next| matches!(next, flowstate_document::Block::Paragraph(_)));
      if text_len == 0 || at.byte < text_len || !followed_by_paragraph {
        return rebuild("insert-object-boundary-adjacent");
      }
      Some((
        vec![ProjectionPatch::InsertBlocks {
          before: projection.ids.block_ids.get(*block_ix).copied(),
          row_hint: (*block_ix).min(projection.blocks.len()),
          blocks: vec![ProjectionStructuralBlock {
            block_id: *new_block,
            paragraph_id: None,
            block: block.clone(),
          }],
        }],
        ProjectionInvalidation::body_object(frontier_before.clone(), frontier_after.clone(), at.body_unicode, block_kind(block)),
      ))
    },
    ResolvedPlan::ReplaceObject { block_ix, after, .. } => {
      object_replacement_patch(projection, *block_ix, after.clone()).map(|patches| (patches, body_invalidation(0, 0)))
    },
    ResolvedPlan::DeleteBlocks { .. } => {
      // Removing a block row changes the boundary geometry around it: a
      // paragraph that was interstitial (boundary-less, after an object)
      // re-attaches, and a coalesced empty resurrects with its durable
      // record's identity — re-derivations only the materializer law can
      // produce. Loud rebuild (block deletes are rare, explicit UI ops;
      // found by the object-fuzz undo arm as maintained-vs-canonical id
      // divergence when deleting a boundary-adjacent object).
      return rebuild("delete-blocks-boundary-sensitive");
    },
    ResolvedPlan::MoveBlock { .. } => {
      // A move is a boundary-geometry change at BOTH endpoints (see the
      // DeleteBlocks rationale) — identities around the source and the
      // destination re-derive under the materializer law. Loud rebuild.
      return rebuild("move-block-boundary-sensitive");
    },
    ResolvedPlan::InsertRichFragment { .. } => {
      // The one op class where a full rebuild is the documented contract
      // (compound multi-container splice). Loud + counted by the caller.
      return rebuild("insert-rich-fragment");
    },
    ResolvedPlan::ReplaceEquationSourceRange { equation, range, text } => {
      let Some(block_ix) = core.projection_index_ref().block_index(*equation) else {
        return rebuild("equation-block-missing");
      };
      let Some(InputBlock::Equation(mut equation_input)) = projection.blocks.get(block_ix).map(input_block_from_block) else {
        return rebuild("equation-input-missing");
      };
      if range.start > range.end || range.end > equation_input.source.len() {
        return rebuild("equation-range-invalid");
      }
      equation_input.source.replace_range(range.clone(), text);
      object_replacement_patch(projection, block_ix, InputBlock::Equation(equation_input)).map(|patches| (patches, body_invalidation(0, 0)))
    },
    ResolvedPlan::ReplaceImageAltText { image, text } => {
      image_patch(core, *image, |input| input.alt_text = text.clone()).map(|patches| (patches, body_invalidation(0, 0)))
    },
    ResolvedPlan::ReplaceImageCaption { image, caption } => {
      image_patch(core, *image, |input| input.caption = caption.clone()).map(|patches| (patches, body_invalidation(0, 0)))
    },
    ResolvedPlan::SetImageLayout { image, sizing, alignment } => image_patch(core, *image, |input| {
      input.sizing = sizing.clone();
      input.alignment = *alignment;
    })
    .map(|patches| (patches, body_invalidation(0, 0))),
    ResolvedPlan::Table { table, table_ix, op } => table_patch(core, *table, *table_ix, op).map(|patches| (patches, body_invalidation(0, 0))),
    ResolvedPlan::ReplaceMatches { matches, replacement } => {
      // Matches are same-paragraph and sorted descending (resolution
      // contract); group per paragraph and read each affected paragraph back
      // ONCE through the ranged readback — O(matches + affected paragraphs),
      // never O(doc), regardless of how many matches the storm carries.
      // Groups are processed in ASCENDING position order with a running net
      // shift: resolved positions are PRE-commit, but the readback runs
      // POST-commit, where every paragraph after an edited one has moved by
      // the net length delta of the edits before it.
      let replacement_chars = replacement.chars().count();
      let mut groups: Vec<&[(ResolvedTextPosition, ResolvedTextPosition, Option<flowstate_document::RunStyles>)]> = Vec::new();
      let mut ix = 0;
      while ix < matches.len() {
        let paragraph_ix = matches[ix].0.paragraph_ix;
        let group_len = matches[ix..]
          .iter()
          .take_while(|(start, ..)| start.paragraph_ix == paragraph_ix)
          .count();
        groups.push(&matches[ix..ix + group_len]);
        ix += group_len;
      }

      // Rows for every paragraph in one O(blocks) pass instead of an O(blocks)
      // scan per matched group (§perf-heaven T2 quadratic on object docs).
      let rows = flowstate_document::paragraph_block_rows(projection);
      let mut patches = Vec::new();
      let mut invalid_lo = usize::MAX;
      let mut invalid_hi = 0usize;
      let mut shift = 0isize;
      for group in groups.into_iter().rev() {
        let paragraph_ix = group[0].0.paragraph_ix;
        let Some(&row) = rows.get(paragraph_ix) else {
          return rebuild("replace-matches-block-missing");
        };
        let Some((paragraph_start, old_chars)) = resolved_paragraph_span(projection, &group[0].0) else {
          return rebuild("replace-matches-position-misaligned");
        };
        let removed: usize = group
          .iter()
          .map(|(start, end, _)| end.body_unicode - start.body_unicode)
          .sum();
        let added = replacement_chars * group.len();
        let Some(new_chars) = (old_chars + added).checked_sub(removed) else {
          return rebuild("replace-matches-length-misaligned");
        };
        let Some(post_start) = paragraph_start.checked_add_signed(shift) else {
          return rebuild("replace-matches-shift-misaligned");
        };
        let Some(new) = body_input_paragraph_at(
          doc,
          post_start.saturating_sub(1),
          post_start + new_chars,
          paragraph_style_at(projection, paragraph_ix),
        ) else {
          return rebuild("replace-matches-readback-missing");
        };
        shift += added as isize - removed as isize;
        let old_text = flowstate_document::paragraph_text(projection, paragraph_ix);
        let new_text: String = new.runs.iter().map(|run| run.text.as_str()).collect();
        let delta_utf8 = text_delta_between(&old_text, &new_text);
        patches.push(ProjectionPatch::ParagraphText {
          block_id: projection.ids.block_ids[row],
          paragraph_id: projection.ids.paragraph_ids[paragraph_ix],
          row_hint: row,
          new,
          delta_utf8,
        });
        // Descending within the group: last entry has the lowest start.
        invalid_lo = invalid_lo.min(group[group.len() - 1].0.body_unicode);
        invalid_hi = invalid_hi.max(group[0].1.body_unicode);
      }
      Some((patches, body_invalidation(invalid_lo, invalid_hi.saturating_sub(invalid_lo))))
    },
  };

  match patches {
    Some((patches, invalidation)) => PatchPlan::Patches { patches, invalidation },
    None => rebuild(intent_rebuild_reason(intent)),
  }
}

/// UTF-8 byte length of an input paragraph's text.
/// Live-space unicode start and pre-mutation char length of the paragraph a
/// resolved position sits in. Exact for post-mutation reads because text
/// edits at/after the resolved point never move the paragraph's start.
fn resolved_paragraph_span(projection: &DocumentProjection, at: &ResolvedTextPosition) -> Option<(usize, usize)> {
  let text = flowstate_document::paragraph_text(projection, at.paragraph_ix);
  let within = text.get(..at.byte)?.chars().count();
  let start = at.body_unicode.checked_sub(within)?;
  Some((start, text.chars().count()))
}

fn paragraph_style_at(projection: &DocumentProjection, paragraph_ix: usize) -> ParagraphStyle {
  projection
    .paragraphs
    .get(paragraph_ix)
    .map(|paragraph| paragraph.style)
    .unwrap_or(ParagraphStyle::Normal)
}

fn input_paragraph_text_len(paragraph: &flowstate_document::InputParagraph) -> usize {
  paragraph.runs.iter().map(|run| run.text.len()).sum()
}

fn intent_rebuild_reason(intent: &LocalIntent) -> &'static str {
  match intent {
    LocalIntent::InsertRichFragment(_) => "insert-rich-fragment",
    _ => "patch-synthesis-bailout",
  }
}

fn block_kind(block: &InputBlock) -> &'static str {
  match block {
    InputBlock::Image(_) => "image",
    InputBlock::Equation(_) => "equation",
    InputBlock::Table(_) => "table",
    InputBlock::Paragraph(_) => "paragraph",
  }
}

fn image_patch(
  core: &CrdtRuntime,
  image: flowstate_document::BlockId,
  mutate: impl FnOnce(&mut flowstate_document::InputImageBlock),
) -> Option<Vec<ProjectionPatch>> {
  let projection = core.projection_ref();
  let block_ix = core.projection_index_ref().block_index(image)?;
  let InputBlock::Image(mut image_input) = projection
    .blocks
    .get(block_ix)
    .map(input_block_from_block)?
  else {
    return None;
  };
  mutate(&mut image_input);
  object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
}

fn table_patch(core: &CrdtRuntime, table: flowstate_document::BlockId, table_ix: usize, op: &ResolvedTableOp) -> Option<Vec<ProjectionPatch>> {
  // §6-R.1: READ the committed table back from canonical state (the same
  // one-table materialization law the full rebuild applies) instead of
  // simulating the op on the old projected table. Simulation was a second
  // doc→projection semantics and diverged under undo-churned histories (the
  // table-fuzz undo arm caught stale cell spans). Any defect or a missing
  // canonical record falls back to the loud full rebuild (`None`).
  let _ = op;
  let projection = core.projection_ref();
  let block_ix = core.projection_index_ref().block_index(table)?;
  debug_assert_eq!(block_ix, table_ix);
  let (table_input, defects) = flowstate_document::materialize_table_block(core.doc(), table.0).ok()?;
  if !defects.is_empty() {
    return None;
  }
  object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
}
