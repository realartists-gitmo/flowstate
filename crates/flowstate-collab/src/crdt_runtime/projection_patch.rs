use flowstate_document::{
  Block, DocumentProjection, HighlightStyle, InputBlock, InputParagraph, InputRun, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE,
  MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, Paragraph, ParagraphStyle, ProjectionPatch, ProjectionStructuralBlock,
  ProjectionTextDelta, RunSemanticStyle, RunStyles, input_block_from_block, loro_schema::body_text, new_block_id, new_paragraph_id,
  paragraph_text,
};
use loro::{LoroDoc, LoroValue};
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

use super::{ProjectionInvalidation, paragraph_body_start_in_loro, paragraph_style_from_attrs};

pub(crate) fn projection_patches_between(before: &DocumentProjection, after: &DocumentProjection) -> Option<Vec<ProjectionPatch>> {
  let mut patches = Vec::new();
  append_asset_patches(before, after, &mut patches);

  let before_ids = &before.ids.block_ids;
  let after_ids = &after.ids.block_ids;
  if before_ids == after_ids {
    append_same_shape_patches(before, after, &mut patches)?;
    return Some(patches);
  }

  if let Some((from, to)) = single_block_move(before_ids, after_ids) {
    if !retained_blocks_unchanged(before, after) {
      return None;
    }
    patches.push(ProjectionPatch::MoveBlock {
      block_id: before_ids[from],
      before: after_ids.get(to + 1).copied(),
      from_hint: from,
      to_hint: to,
    });
    return Some(patches);
  }

  let prefix = common_id_prefix(before_ids, after_ids);
  let suffix = common_id_suffix(before_ids, after_ids, prefix);
  let before_end = before_ids.len().saturating_sub(suffix);
  let after_end = after_ids.len().saturating_sub(suffix);
  if before_end > prefix {
    patches.push(ProjectionPatch::DeleteBlocks {
      block_ids: before_ids[prefix..before_end].to_vec(),
      row_hint: prefix,
    });
  }
  if after_end > prefix {
    patches.push(ProjectionPatch::InsertBlocks {
      before: after_ids.get(after_end).copied(),
      row_hint: prefix,
      blocks: structural_blocks(after, prefix..after_end),
    });
  }
  if !retained_edge_blocks_unchanged(before, after, prefix, suffix) {
    return None;
  }
  Some(patches)
}

fn append_same_shape_patches(before: &DocumentProjection, after: &DocumentProjection, patches: &mut Vec<ProjectionPatch>) -> Option<()> {
  if before.blocks.len() != after.blocks.len() {
    return None;
  }
  let mut before_paragraph_ix = 0usize;
  let mut after_paragraph_ix = 0usize;
  for (row, (before_block, after_block)) in before.blocks.iter().zip(after.blocks.iter()).enumerate() {
    match (before_block, after_block) {
      (Block::Paragraph(before_paragraph), Block::Paragraph(after_paragraph)) => {
        append_paragraph_patch(
          before,
          after,
          row,
          before_paragraph_ix,
          after_paragraph_ix,
          before_paragraph,
          after_paragraph,
          patches,
        );
        before_paragraph_ix += 1;
        after_paragraph_ix += 1;
      },
      (Block::Image(_) | Block::Equation(_) | Block::Table(_), Block::Image(_) | Block::Equation(_) | Block::Table(_)) => {
        if before_block != after_block {
          patches.push(ProjectionPatch::ReplaceObjectBlock {
            block_id: before.ids.block_ids[row],
            row_hint: row,
            block: structural_block(after, row, after_paragraph_ix),
          });
        }
      },
      _ => return None,
    }
  }
  Some(())
}

#[allow(clippy::too_many_arguments, reason = "paragraph diffs need both projection coordinates and payloads")]
fn append_paragraph_patch(
  before: &DocumentProjection,
  after: &DocumentProjection,
  row: usize,
  before_paragraph_ix: usize,
  after_paragraph_ix: usize,
  before_paragraph: &Paragraph,
  after_paragraph: &Paragraph,
  patches: &mut Vec<ProjectionPatch>,
) {
  let before_text = paragraph_text(before, before_paragraph_ix);
  let after_text = paragraph_text(after, after_paragraph_ix);
  if before_text != after_text {
    patches.push(ProjectionPatch::ParagraphText {
      block_id: before.ids.block_ids[row],
      paragraph_id: before.ids.paragraph_ids[before_paragraph_ix],
      row_hint: row,
      new: input_paragraph(after, after_paragraph_ix, after_paragraph),
      delta_utf8: text_delta_between(&before_text, &after_text),
    });
    return;
  }
  if before_paragraph.style != after_paragraph.style {
    patches.push(ProjectionPatch::ParagraphStyle {
      block_id: before.ids.block_ids[row],
      paragraph_id: before.ids.paragraph_ids[before_paragraph_ix],
      row_hint: row,
      style: after_paragraph.style,
    });
  }
  if before_paragraph.runs != after_paragraph.runs {
    patches.push(ProjectionPatch::ParagraphRuns {
      block_id: before.ids.block_ids[row],
      paragraph_id: before.ids.paragraph_ids[before_paragraph_ix],
      row_hint: row,
      runs: after_paragraph.runs.clone(),
    });
  }
}

fn input_paragraph(document: &DocumentProjection, paragraph_ix: usize, paragraph: &Paragraph) -> InputParagraph {
  let text = paragraph_text(document, paragraph_ix);
  let mut byte = 0usize;
  let runs = paragraph
    .runs
    .iter()
    .map(|run| {
      let start = byte;
      let end = start.saturating_add(run.len).min(text.len());
      byte = end;
      InputRun {
        text: text.get(start..end).unwrap_or_default().to_string(),
        styles: run.styles,
      }
    })
    .collect();
  InputParagraph {
    style: paragraph.style,
    runs,
  }
}

fn structural_blocks(document: &DocumentProjection, range: std::ops::Range<usize>) -> Vec<ProjectionStructuralBlock> {
  let mut paragraph_ix = document
    .blocks
    .iter()
    .take(range.start)
    .filter(|block| matches!(block, Block::Paragraph(_)))
    .count();
  range
    .map(|row| {
      let structural = structural_block(document, row, paragraph_ix);
      if matches!(document.blocks.get(row), Some(Block::Paragraph(_))) {
        paragraph_ix += 1;
      }
      structural
    })
    .collect()
}

fn structural_block(document: &DocumentProjection, row: usize, paragraph_ix: usize) -> ProjectionStructuralBlock {
  // A `Block::Paragraph` stores only run lengths — its text lives in the document
  // Rope — so `input_block_from_block` (which has no document) would produce
  // runs with EMPTY text and silently drop the paragraph's content (e.g. a
  // split-then-insert batch inserting an empty paragraph instead of the typed
  // text). Slice the real text via `input_paragraph`; objects carry their own
  // payload and convert directly.
  let block = match document.blocks.get(row) {
    Some(Block::Paragraph(paragraph)) => InputBlock::Paragraph(input_paragraph(document, paragraph_ix, paragraph)),
    Some(other) => input_block_from_block(other),
    None => InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    }),
  };
  ProjectionStructuralBlock {
    block_id: document
      .ids
      .block_ids
      .get(row)
      .copied()
      .unwrap_or_else(new_block_id),
    paragraph_id: matches!(&block, InputBlock::Paragraph(_)).then(|| {
      document
        .ids
        .paragraph_ids
        .get(paragraph_ix)
        .copied()
        .unwrap_or_else(new_paragraph_id)
    }),
    block,
  }
}

fn append_asset_patches(before: &DocumentProjection, after: &DocumentProjection, patches: &mut Vec<ProjectionPatch>) {
  for (id, record) in &after.assets.assets {
    if before.assets.assets.get(id) != Some(record) {
      patches.push(ProjectionPatch::AssetArrived {
        id: *id,
        record: record.clone(),
      });
    }
  }
}

fn single_block_move(before: &[flowstate_document::BlockId], after: &[flowstate_document::BlockId]) -> Option<(usize, usize)> {
  if before.len() != after.len() || before.len() < 2 {
    return None;
  }

  let mut before_positions = FxHashMap::default();
  for (index, id) in before.iter().copied().enumerate() {
    if before_positions.insert(id, index).is_some() {
      return None;
    }
  }

  let mut moved = None;
  for (after_index, id) in after.iter().copied().enumerate() {
    let before_index = *before_positions.get(&id)?;
    if before_index != after_index {
      match moved {
        Some((existing_id, _, _)) if existing_id != id => return None,
        Some(_) => {},
        None => moved = Some((id, before_index, after_index)),
      }
    }
  }

  let (moved_id, from, _) = moved?;
  let mut candidate = before.to_vec();
  candidate.remove(from);
  let to = after.iter().position(|id| *id == moved_id)?;
  candidate.insert(to, moved_id);
  (candidate == after).then_some((from, to))
}

fn retained_blocks_unchanged(before: &DocumentProjection, after: &DocumentProjection) -> bool {
  let mut before_blocks = FxHashMap::default();
  // §perf: rows are visited in ascending order, so carry a running paragraph counter
  // instead of recomputing the paragraph index per row (see `input_block_at_seq`).
  let mut before_paragraph_ix = 0;
  for row in 0..before.ids.block_ids.len() {
    let Some(id) = before.ids.block_ids.get(row).copied() else {
      return false;
    };
    let Some(block) = input_block_at_seq(before, row, &mut before_paragraph_ix) else {
      return false;
    };
    before_blocks.insert(id, block);
  }
  let mut after_paragraph_ix = 0;
  for row in 0..after.ids.block_ids.len() {
    let Some(id) = after.ids.block_ids.get(row).copied() else {
      return false;
    };
    let Some(before_block) = before_blocks.get(&id) else {
      return false;
    };
    let Some(after_block) = input_block_at_seq(after, row, &mut after_paragraph_ix) else {
      return false;
    };
    if before_block != &after_block {
      return false;
    }
  }
  true
}

fn retained_edge_blocks_unchanged(before: &DocumentProjection, after: &DocumentProjection, prefix: usize, suffix: usize) -> bool {
  let before_len = before.ids.block_ids.len();
  let after_len = after.ids.block_ids.len();
  // §perf: the prefix rows are contiguous from 0, so run two ascending paragraph
  // counters (one per document) instead of an O(row) recount per row.
  let mut before_paragraph_ix = 0;
  let mut after_paragraph_ix = 0;
  for row in 0..prefix {
    if input_block_at_seq(before, row, &mut before_paragraph_ix) != input_block_at_seq(after, row, &mut after_paragraph_ix) {
      return false;
    }
  }
  // The suffix rows are a contiguous tail range in each document; seed each counter
  // once (a single scan up to the suffix start) then advance it sequentially.
  let before_suffix_start = before_len.saturating_sub(suffix);
  let after_suffix_start = after_len.saturating_sub(suffix);
  let mut before_paragraph_ix = count_paragraph_blocks_before(before, before_suffix_start);
  let mut after_paragraph_ix = count_paragraph_blocks_before(after, after_suffix_start);
  for offset in 0..suffix {
    let before_row = before_suffix_start + offset;
    let after_row = after_suffix_start + offset;
    if input_block_at_seq(before, before_row, &mut before_paragraph_ix) != input_block_at_seq(after, after_row, &mut after_paragraph_ix) {
      return false;
    }
  }
  true
}

// §perf: sequential variant of the former `input_block_at`. `paragraph_ix` must hold
// the number of Paragraph blocks in `document.blocks[0..row]`; callers visit rows in
// ascending order and advance it here, so building a block no longer recomputes that
// count with an O(row) scan. This turns the patch-diff verification
// (retained_blocks_unchanged / retained_edge_blocks_unchanged), which runs on every
// structural edit, from O(blocks²) into O(blocks).
fn input_block_at_seq(document: &DocumentProjection, row: usize, paragraph_ix: &mut usize) -> Option<InputBlock> {
  let block = document.blocks.get(row)?;
  match block {
    Block::Paragraph(paragraph) => {
      let ix = *paragraph_ix;
      *paragraph_ix += 1;
      Some(InputBlock::Paragraph(input_paragraph(document, ix, paragraph)))
    },
    _ => Some(input_block_from_block(block)),
  }
}

// Count of Paragraph blocks strictly before `row`; used once per document to seed the
// sequential counter for the suffix range.
fn count_paragraph_blocks_before(document: &DocumentProjection, row: usize) -> usize {
  document
    .blocks
    .iter()
    .take(row)
    .filter(|block| matches!(block, Block::Paragraph(_)))
    .count()
}

fn common_id_prefix(left: &[flowstate_document::BlockId], right: &[flowstate_document::BlockId]) -> usize {
  left
    .iter()
    .zip(right)
    .take_while(|(left, right)| left == right)
    .count()
}

fn common_id_suffix(left: &[flowstate_document::BlockId], right: &[flowstate_document::BlockId], prefix: usize) -> usize {
  left
    .iter()
    .skip(prefix)
    .rev()
    .zip(right.iter().skip(prefix).rev())
    .take_while(|(left, right)| left == right)
    .count()
}

fn text_delta_between(before: &str, after: &str) -> Vec<ProjectionTextDelta> {
  let prefix = common_prefix_byte_len(before, after);
  let suffix = common_suffix_byte_len(before, after, prefix);
  text_delta(
    prefix,
    before.len().saturating_sub(prefix + suffix),
    after.len().saturating_sub(prefix + suffix),
    suffix,
  )
}

pub(crate) fn remote_body_text_patch(
  projection: &DocumentProjection,
  before: &str,
  after: &str,
  doc: &LoroDoc,
  frontier_before: Vec<u8>,
  frontier_after: Vec<u8>,
) -> Option<(Vec<ProjectionPatch>, ProjectionInvalidation)> {
  if before == after {
    return None;
  }

  let prefix = common_prefix_byte_len(before, after);
  let suffix = common_suffix_byte_len(before, after, prefix);
  let before_changed_end = before.len().checked_sub(suffix)?;
  let after_changed_end = after.len().checked_sub(suffix)?;
  if prefix > before_changed_end || prefix > after_changed_end {
    return None;
  }

  let before_changed = before.get(prefix..before_changed_end)?;
  let after_changed = after.get(prefix..after_changed_end)?;
  if contains_structural_body_char(before_changed) || contains_structural_body_char(after_changed) {
    return None;
  }

  let before_location = paragraph_text_location(before, prefix)?;
  let after_location = paragraph_text_location(after, prefix)?;
  if before_location.paragraph_ix != after_location.paragraph_ix {
    return None;
  }

  let old_paragraph_len = before_location
    .paragraph_end_byte
    .checked_sub(before_location.paragraph_start_byte)?;
  let prefix_in_paragraph = prefix.checked_sub(before_location.paragraph_start_byte)?;
  let old_changed_len = before_changed_end.checked_sub(prefix)?;
  let new_changed_len = after_changed_end.checked_sub(prefix)?;
  let trailing_retain = old_paragraph_len
    .checked_sub(prefix_in_paragraph)?
    .checked_sub(old_changed_len)?;

  let new_paragraph = body_input_paragraph(doc, before_location.paragraph_ix)?;
  let delta_utf8 = text_delta(prefix_in_paragraph, old_changed_len, new_changed_len, trailing_retain);
  let unicode_start = before[..prefix].chars().count();
  let unicode_len = before_changed
    .chars()
    .count()
    .max(after_changed.chars().count());
  let invalidation = ProjectionInvalidation::body_text(frontier_before, frontier_after, unicode_start, unicode_len);
  let row = flowstate_document::block_ix_for_paragraph(projection, before_location.paragraph_ix)?;
  Some((
    vec![ProjectionPatch::ParagraphText {
      block_id: projection.ids.block_ids[row],
      paragraph_id: projection.ids.paragraph_ids[before_location.paragraph_ix],
      row_hint: row,
      new: new_paragraph,
      delta_utf8,
    }],
    invalidation,
  ))
}

pub(crate) fn remote_body_projection_patches(
  projection: &DocumentProjection,
  before: &str,
  after: &str,
  doc: &LoroDoc,
  invalidation: &ProjectionInvalidation,
) -> Option<Vec<ProjectionPatch>> {
  if invalidation.rebuild_required || !invalidation.changed_sections.is_empty() {
    return None;
  }

  if before != after {
    let mut patches = remote_body_text_patch(
      projection,
      before,
      after,
      doc,
      invalidation.frontier_before.clone(),
      invalidation.frontier_after.clone(),
    )?
    .0;
    if !invalidation.changed_blocks.is_empty()
      || !invalidation.changed_tables.is_empty()
      || invalidation
        .changed_flows
        .iter()
        .any(|flow| flow != flowstate_document::ROOT_BODY_FLOW_ID)
    {
      patches.extend(remote_object_projection_patches(projection, doc)?);
    }
    return Some(patches);
  }

  if !invalidation.changed_blocks.is_empty()
    || !invalidation.changed_tables.is_empty()
    || invalidation
      .changed_flows
      .iter()
      .any(|flow| flow != flowstate_document::ROOT_BODY_FLOW_ID)
  {
    return remote_object_projection_patches(projection, doc);
  }

  let mut touched = BTreeSet::new();
  for range in &invalidation.changed_text_ranges {
    if range.flow_id != flowstate_document::ROOT_BODY_FLOW_ID {
      return None;
    }
    touched.insert(paragraph_index_at_unicode(after, range.unicode_start));
    touched.insert(paragraph_index_at_unicode(after, range.unicode_start.saturating_add(range.unicode_len)));
  }
  if touched.is_empty() {
    return Some(Vec::new());
  }
  paragraph_projection_patches(projection, doc, touched)
}

#[hotpath::measure]
pub(crate) fn remote_nonstructural_projection_patches(
  projection: &DocumentProjection,
  doc: &LoroDoc,
  invalidation: &ProjectionInvalidation,
  touched_paragraphs: &[usize],
  live_starts: &[usize],
) -> Option<Vec<ProjectionPatch>> {
  if invalidation.rebuild_required || !invalidation.changed_sections.is_empty() {
    return None;
  }
  // Object docs are safe on this incremental path because the RANGED readback
  // resolves each touched paragraph's live range from its durable record —
  // object placeholders and coalesced empties cannot mis-align the rows the
  // way the legacy whole-body index walk could. A range that does not parse
  // as exactly one paragraph returns None and falls back to the rebuild.
  // (Convergence: proven by the N-peer structural fuzz.)
  let mut patches = paragraph_projection_patches_ranged(projection, doc, live_starts, touched_paragraphs.iter().copied())?;
  if !invalidation.changed_blocks.is_empty()
    || !invalidation.changed_tables.is_empty()
    || invalidation
      .changed_flows
      .iter()
      .any(|flow| flow != flowstate_document::ROOT_BODY_FLOW_ID)
  {
    patches.extend(remote_object_projection_patches(projection, doc)?);
  }
  Some(patches)
}

fn paragraph_projection_patches(
  projection: &DocumentProjection,
  doc: &LoroDoc,
  touched_paragraphs: impl IntoIterator<Item = usize>,
) -> Option<Vec<ProjectionPatch>> {
  let mut patches = Vec::new();
  for paragraph_ix in touched_paragraphs {
    let new_input = body_input_paragraph(doc, paragraph_ix)?;
    paragraph_patches_from_readback(projection, paragraph_ix, new_input, &mut patches)?;
  }
  Some(patches)
}

/// Ranged sibling of [`paragraph_projection_patches`]: reads each touched
/// paragraph back through [`body_input_paragraph_at`] (O(paragraph) per read
/// instead of an O(doc) whole-body walk), with ranges resolved from durable
/// paragraph records — exact in live space even for object docs, which the
/// index-walk readback cannot promise.
fn paragraph_projection_patches_ranged(
  projection: &DocumentProjection,
  doc: &LoroDoc,
  live_starts: &[usize],
  touched_paragraphs: impl IntoIterator<Item = usize>,
) -> Option<Vec<ProjectionPatch>> {
  let mut patches = Vec::new();
  for paragraph_ix in touched_paragraphs {
    let old = projection.paragraphs.get(paragraph_ix)?;
    let (sentinel, end) = live_paragraph_range(doc, projection, live_starts, paragraph_ix)?;
    let new_input = body_input_paragraph_at(doc, sentinel, end, input_paragraph(projection, paragraph_ix, old).style)?;
    paragraph_patches_from_readback(projection, paragraph_ix, new_input, &mut patches)?;
  }
  Some(patches)
}

fn paragraph_patches_from_readback(
  projection: &DocumentProjection,
  paragraph_ix: usize,
  new_input: InputParagraph,
  patches: &mut Vec<ProjectionPatch>,
) -> Option<()> {
  let old = projection.paragraphs.get(paragraph_ix)?;
  let old_input = input_paragraph(projection, paragraph_ix, old);
  let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
  let old_text = old_input
    .runs
    .iter()
    .map(|run| run.text.as_str())
    .collect::<String>();
  let new_text = new_input
    .runs
    .iter()
    .map(|run| run.text.as_str())
    .collect::<String>();
  if old_text != new_text {
    patches.push(ProjectionPatch::ParagraphText {
      block_id: projection.ids.block_ids[row],
      paragraph_id: projection.ids.paragraph_ids[paragraph_ix],
      row_hint: row,
      delta_utf8: text_delta_between(&old_text, &new_text),
      new: new_input,
    });
    return Some(());
  }
  if old_input.style != new_input.style {
    patches.push(ProjectionPatch::ParagraphStyle {
      block_id: projection.ids.block_ids[row],
      paragraph_id: projection.ids.paragraph_ids[paragraph_ix],
      row_hint: row,
      style: new_input.style,
    });
  }
  let new_runs = flowstate_document::document_from_input_blocks(projection.theme.clone(), vec![InputBlock::Paragraph(new_input)])
    .paragraphs
    .first()?
    .runs
    .clone();
  if old.runs != new_runs {
    patches.push(ProjectionPatch::ParagraphRuns {
      block_id: projection.ids.block_ids[row],
      paragraph_id: projection.ids.paragraph_ids[paragraph_ix],
      row_hint: row,
      runs: new_runs,
    });
  }
  Some(())
}

#[hotpath::measure]
fn remote_object_projection_patches(projection: &DocumentProjection, doc: &LoroDoc) -> Option<Vec<ProjectionPatch>> {
  let projected = flowstate_document::object_input_blocks_from_loro(doc).ok()?;
  let mut existing = projection
    .blocks
    .iter()
    .enumerate()
    .filter_map(|(row, block)| {
      if matches!(block, Block::Paragraph(_)) {
        return None;
      }
      Some((projection.ids.block_ids.get(row).copied()?, row, input_block_from_block(block)))
    })
    .collect::<Vec<_>>();
  existing.sort_by_key(|(id, _, _)| id.0);
  if existing.len() != projected.len()
    || existing
      .iter()
      .zip(&projected)
      .any(|((existing_id, _, _), (projected_id, _))| existing_id != projected_id)
  {
    return None;
  }
  Some(
    existing
      .into_iter()
      .zip(projected)
      .filter_map(|((block_id, row, before), (_, after))| {
        (before != after).then_some(ProjectionPatch::ReplaceObjectBlock {
          block_id,
          row_hint: row,
          block: ProjectionStructuralBlock {
            block_id,
            paragraph_id: None,
            block: after,
          },
        })
      })
      .collect(),
  )
}

fn paragraph_index_at_unicode(body: &str, unicode_index: usize) -> usize {
  let mut paragraph_ix = 0usize;
  let mut seen_sentinel = false;
  for (index, ch) in body.chars().enumerate() {
    if index >= unicode_index {
      break;
    }
    if ch == '\n' {
      if seen_sentinel {
        paragraph_ix += 1;
      } else {
        seen_sentinel = true;
      }
    }
  }
  paragraph_ix
}

fn contains_structural_body_char(text: &str) -> bool {
  text
    .chars()
    .any(|ch| ch == '\n' || ch == OBJECT_REPLACEMENT)
}

#[derive(Clone, Copy)]
struct ParagraphTextLocation {
  paragraph_ix: usize,
  paragraph_start_byte: usize,
  paragraph_end_byte: usize,
}

fn paragraph_text_location(body: &str, body_byte: usize) -> Option<ParagraphTextLocation> {
  if body_byte > body.len() || !body.is_char_boundary(body_byte) {
    return None;
  }
  let sentinel_end = body.find('\n')? + '\n'.len_utf8();
  if body_byte < sentinel_end {
    return None;
  }
  let paragraph_start_byte = body[..body_byte]
    .rfind('\n')
    .map_or(sentinel_end, |index| index + '\n'.len_utf8());
  let paragraph_end_byte = body[body_byte..]
    .find('\n')
    .map_or(body.len(), |relative| body_byte + relative);
  let paragraph_ix = body[..paragraph_start_byte]
    .chars()
    .filter(|ch| *ch == '\n')
    .count()
    .saturating_sub(1);
  Some(ParagraphTextLocation {
    paragraph_ix,
    paragraph_start_byte,
    paragraph_end_byte,
  })
}

fn text_delta(prefix_retain: usize, delete_len: usize, insert_len: usize, trailing_retain: usize) -> Vec<ProjectionTextDelta> {
  let mut delta = Vec::new();
  if prefix_retain > 0 {
    delta.push(ProjectionTextDelta::Retain(prefix_retain));
  }
  if delete_len > 0 {
    delta.push(ProjectionTextDelta::Delete(delete_len));
  }
  if insert_len > 0 {
    delta.push(ProjectionTextDelta::Insert(insert_len));
  }
  if trailing_retain > 0 {
    delta.push(ProjectionTextDelta::Retain(trailing_retain));
  }
  delta
}

fn common_prefix_byte_len(left: &str, right: &str) -> usize {
  let mut len = 0;
  for ((left_ix, left_ch), (_, right_ch)) in left.char_indices().zip(right.char_indices()) {
    if left_ch != right_ch {
      break;
    }
    len = left_ix + left_ch.len_utf8();
  }
  len
}

fn common_suffix_byte_len(left: &str, right: &str, prefix: usize) -> usize {
  let mut len = 0;
  for ((left_ix, left_ch), (right_ix, right_ch)) in left.char_indices().rev().zip(right.char_indices().rev()) {
    if left_ix < prefix || right_ix < prefix || left_ch != right_ch {
      break;
    }
    len += left_ch.len_utf8();
  }
  len
}

/// Ranged replacement for [`body_input_paragraph`] (§11 complexity contract):
/// reads ONE paragraph back through `slice_delta` over its live unicode range
/// instead of materializing the whole body — O(paragraph), not O(doc).
///
/// `sentinel_unicode` addresses the paragraph's LEADING boundary `\n` (the
/// paragraph-style carrier; the seed sentinel for paragraph 0). `end_unicode`
/// is exclusive and may cover trailing object placeholders, which fold out of
/// paragraph text exactly like the legacy whole-body walk. Returns `None`
/// when the slice does not look like exactly one paragraph (missing leading
/// sentinel, or an interior boundary such as a coalesced empty) so callers
/// fall back to a full rebuild instead of patching the wrong rows.
pub(crate) fn body_input_paragraph_at(
  doc: &LoroDoc,
  sentinel_unicode: usize,
  end_unicode: usize,
  fallback_style: ParagraphStyle,
) -> Option<InputParagraph> {
  let text = body_text(doc);
  let end = end_unicode.min(text.len_unicode());
  if sentinel_unicode >= end {
    return None;
  }
  let spans = text.slice_delta(sentinel_unicode, end, loro::cursor::PosType::Unicode).ok()?;
  let mut current = InputParagraph {
    style: fallback_style,
    runs: Vec::new(),
  };
  let mut seen_sentinel = false;
  for item in spans {
    let loro::TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    let run_styles = run_styles_from_attrs(attributes.as_ref());
    for ch in insert.chars() {
      if ch == '\n' {
        if seen_sentinel {
          return None;
        }
        seen_sentinel = true;
        current.style = paragraph_style_from_attrs(attributes.as_ref()).unwrap_or(fallback_style);
      } else if ch != OBJECT_REPLACEMENT {
        if !seen_sentinel {
          return None;
        }
        push_input_char(&mut current, ch, run_styles);
      }
    }
  }
  seen_sentinel.then_some(current)
}

/// Live-body unicode range `(sentinel, end_exclusive)` for projection
/// paragraph `paragraph_ix`, for [`body_input_paragraph_at`]. Starts resolve
/// from durable paragraph records first (exact live space — object-aware and
/// immune to coalesced-empty drift), with `live_starts` (the projection's
/// paragraph starts shifted into post-change space) as the fallback.
fn live_paragraph_range(doc: &LoroDoc, projection: &DocumentProjection, live_starts: &[usize], paragraph_ix: usize) -> Option<(usize, usize)> {
  let start_of = |ix: usize| {
    projection
      .ids
      .paragraph_ids
      .get(ix)
      .and_then(|id| paragraph_body_start_in_loro(doc, *id))
      .or_else(|| live_starts.get(ix).copied())
  };
  let sentinel = start_of(paragraph_ix)?.checked_sub(1)?;
  let end = if paragraph_ix + 1 < projection.ids.paragraph_ids.len() {
    start_of(paragraph_ix + 1)?.checked_sub(1)?
  } else {
    body_text(doc).len_unicode()
  };
  (sentinel < end).then_some((sentinel, end))
}

pub(crate) fn body_input_paragraph(doc: &LoroDoc, target_paragraph_ix: usize) -> Option<InputParagraph> {
  let text = body_text(doc);
  let mut current = InputParagraph {
    style: ParagraphStyle::Normal,
    runs: Vec::new(),
  };
  let mut pending_style = ParagraphStyle::Normal;
  let mut seen_sentinel = false;
  let mut paragraph_ix = 0usize;

  for item in text.to_delta() {
    let loro::TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    let run_styles = run_styles_from_attrs(attributes.as_ref());
    for ch in insert.chars() {
      if ch == '\n' {
        let style = paragraph_style_from_attrs(attributes.as_ref()).unwrap_or(pending_style);
        if !seen_sentinel {
          seen_sentinel = true;
          pending_style = style;
          current.style = style;
        } else {
          if paragraph_ix == target_paragraph_ix {
            return Some(current);
          }
          paragraph_ix += 1;
          current = InputParagraph { style, runs: Vec::new() };
          pending_style = style;
        }
      } else if ch != OBJECT_REPLACEMENT {
        push_input_char(&mut current, ch, run_styles);
      }
    }
  }

  (seen_sentinel && paragraph_ix == target_paragraph_ix).then_some(current)
}

fn push_input_char(paragraph: &mut InputParagraph, ch: char, styles: RunStyles) {
  if let Some(last) = paragraph.runs.last_mut()
    && last.styles == styles
  {
    last.text.push(ch);
    return;
  }
  paragraph.runs.push(InputRun {
    text: ch.to_string(),
    styles,
  });
}

fn run_styles_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> RunStyles {
  let mut styles = RunStyles::default();
  let Some(attrs) = attrs else {
    return styles;
  };
  if let Some(LoroValue::I64(slot)) = attrs.get(MARK_RUN_SEMANTIC_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.semantic = RunSemanticStyle::Custom(slot);
  }
  if let Some(LoroValue::I64(slot)) = attrs.get(MARK_HIGHLIGHT_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.highlight = Some(HighlightStyle::Custom(slot));
  }
  if matches!(attrs.get(MARK_DIRECT_UNDERLINE), Some(LoroValue::Bool(true))) {
    styles.direct_underline = true;
  }
  if matches!(attrs.get(MARK_STRIKETHROUGH), Some(LoroValue::Bool(true))) {
    styles.strikethrough = true;
  }
  styles
}
