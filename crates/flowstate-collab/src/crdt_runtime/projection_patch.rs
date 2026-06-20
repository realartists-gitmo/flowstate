use flowstate_document::{
  Block, CollabPatch, CollabStructuralBlock, CollabTextDelta, DocumentProjection, HighlightStyle, InputBlock, InputParagraph, InputRun,
  MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, Paragraph,
  ParagraphStyle, RunSemanticStyle, RunStyles, input_block_from_block, loro_schema::body_text, new_block_id, new_paragraph_id,
  paragraph_text,
};
use loro::{LoroDoc, LoroValue};
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

use super::{ProjectionInvalidation, paragraph_style_from_attrs};

pub(super) fn projection_patches_between(before: &DocumentProjection, after: &DocumentProjection) -> Option<Vec<CollabPatch>> {
  let mut patches = Vec::new();
  append_asset_patches(before, after, &mut patches);

  let before_ids = &before.ids.block_ids;
  let after_ids = &after.ids.block_ids;
  if before_ids == after_ids {
    append_same_shape_patches(before, after, &mut patches)?;
    return Some(patches);
  }

  if let Some((from, to)) = single_block_move(before_ids, after_ids) {
    patches.push(CollabPatch::MoveBlock { from, to });
    return Some(patches);
  }

  let prefix = common_id_prefix(before_ids, after_ids);
  let suffix = common_id_suffix(before_ids, after_ids, prefix);
  let before_end = before_ids.len().saturating_sub(suffix);
  let after_end = after_ids.len().saturating_sub(suffix);
  if before_end > prefix {
    patches.push(CollabPatch::DeleteBlocks {
      row: prefix,
      count: before_end - prefix,
    });
  }
  if after_end > prefix {
    patches.push(CollabPatch::InsertBlocks {
      row: prefix,
      blocks: structural_blocks(after, prefix..after_end),
    });
  }
  Some(patches)
}

fn append_same_shape_patches(before: &DocumentProjection, after: &DocumentProjection, patches: &mut Vec<CollabPatch>) -> Option<()> {
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
          patches.push(CollabPatch::ReplaceObjectBlock {
            row,
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
  patches: &mut Vec<CollabPatch>,
) {
  let before_text = paragraph_text(before, before_paragraph_ix);
  let after_text = paragraph_text(after, after_paragraph_ix);
  if before_text != after_text {
    patches.push(CollabPatch::ParagraphText {
      row,
      new: input_paragraph(after, after_paragraph_ix, after_paragraph),
      delta_utf8: text_delta_between(&before_text, &after_text),
    });
    return;
  }
  if before_paragraph.style != after_paragraph.style {
    patches.push(CollabPatch::ParagraphStyle {
      row,
      style: after_paragraph.style,
    });
  }
  if before_paragraph.runs != after_paragraph.runs {
    patches.push(CollabPatch::ParagraphRuns {
      row,
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

fn structural_blocks(document: &DocumentProjection, range: std::ops::Range<usize>) -> Vec<CollabStructuralBlock> {
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

fn structural_block(document: &DocumentProjection, row: usize, paragraph_ix: usize) -> CollabStructuralBlock {
  let block = document
    .blocks
    .get(row)
    .map(input_block_from_block)
    .unwrap_or_else(|| {
      InputBlock::Paragraph(InputParagraph {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      })
    });
  CollabStructuralBlock {
    block_id: document.ids.block_ids.get(row).copied().unwrap_or_else(new_block_id),
    paragraph_id: matches!(&block, InputBlock::Paragraph(_))
      .then(|| {
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

fn append_asset_patches(before: &DocumentProjection, after: &DocumentProjection, patches: &mut Vec<CollabPatch>) {
  for (id, record) in &after.assets.assets {
    if before.assets.assets.get(id) != Some(record) {
      patches.push(CollabPatch::AssetArrived {
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
  for from in 0..before.len() {
    let mut candidate = before.to_vec();
    let id = candidate.remove(from);
    for to in 0..=candidate.len() {
      let mut moved = candidate.clone();
      moved.insert(to, id);
      if moved == after {
        return Some((from, to));
      }
    }
  }
  None
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

fn text_delta_between(before: &str, after: &str) -> Vec<CollabTextDelta> {
  let prefix = common_prefix_byte_len(before, after);
  let suffix = common_suffix_byte_len(before, after, prefix);
  text_delta(
    prefix,
    before.len().saturating_sub(prefix + suffix),
    after.len().saturating_sub(prefix + suffix),
    suffix,
  )
}

pub(super) fn remote_body_text_patch(
  projection: &DocumentProjection,
  before: &str,
  after: &str,
  doc: &LoroDoc,
  frontier_before: Vec<u8>,
  frontier_after: Vec<u8>,
) -> Option<(Vec<CollabPatch>, ProjectionInvalidation)> {
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
  let unicode_len = before_changed.chars().count().max(after_changed.chars().count());
  let invalidation = ProjectionInvalidation::body_text(frontier_before, frontier_after, unicode_start, unicode_len);
  Some((
    vec![CollabPatch::ParagraphText {
      row: flowstate_document::block_ix_for_paragraph(projection, before_location.paragraph_ix)?,
      new: new_paragraph,
      delta_utf8,
    }],
    invalidation,
  ))
}

pub(super) fn remote_body_projection_patches(
  projection: &DocumentProjection,
  before: &str,
  after: &str,
  doc: &LoroDoc,
  invalidation: &ProjectionInvalidation,
) -> Option<Vec<CollabPatch>> {
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
    touched.insert(paragraph_index_at_unicode(
      after,
      range.unicode_start.saturating_add(range.unicode_len),
    ));
  }
  if touched.is_empty() {
    return Some(Vec::new());
  }

  let mut patches = Vec::new();
  for paragraph_ix in touched {
    let old = projection.paragraphs.get(paragraph_ix)?;
    let old_input = input_paragraph(projection, paragraph_ix, old);
    let new_input = body_input_paragraph(doc, paragraph_ix)?;
    let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
    let old_text = old_input.runs.iter().map(|run| run.text.as_str()).collect::<String>();
    let new_text = new_input.runs.iter().map(|run| run.text.as_str()).collect::<String>();
    if old_text != new_text {
      patches.push(CollabPatch::ParagraphText {
        row,
        delta_utf8: text_delta_between(&old_text, &new_text),
        new: new_input,
      });
      continue;
    }
    if old_input.style != new_input.style {
      patches.push(CollabPatch::ParagraphStyle {
        row,
        style: new_input.style,
      });
    }
    let new_runs = flowstate_document::document_from_input_blocks(
      projection.theme.clone(),
      vec![InputBlock::Paragraph(new_input)],
    )
    .paragraphs
    .first()?
    .runs
    .clone();
    if old.runs != new_runs {
      patches.push(CollabPatch::ParagraphRuns { row, runs: new_runs });
    }
  }
  Some(patches)
}

fn remote_object_projection_patches(projection: &DocumentProjection, doc: &LoroDoc) -> Option<Vec<CollabPatch>> {
  let projected = flowstate_document::object_input_blocks_from_loro(doc).ok()?;
  let mut existing = projection
    .blocks
    .iter()
    .enumerate()
    .filter_map(|(row, block)| {
      if matches!(block, Block::Paragraph(_)) {
        return None;
      }
      Some((
        projection.ids.block_ids.get(row).copied()?,
        row,
        input_block_from_block(block),
      ))
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
        (before != after).then_some(CollabPatch::ReplaceObjectBlock {
          row,
          block: CollabStructuralBlock {
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
  text.chars().any(|ch| ch == '\n' || ch == OBJECT_REPLACEMENT)
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

fn text_delta(prefix_retain: usize, delete_len: usize, insert_len: usize, trailing_retain: usize) -> Vec<CollabTextDelta> {
  let mut delta = Vec::new();
  if prefix_retain > 0 {
    delta.push(CollabTextDelta::Retain(prefix_retain));
  }
  if delete_len > 0 {
    delta.push(CollabTextDelta::Delete(delete_len));
  }
  if insert_len > 0 {
    delta.push(CollabTextDelta::Insert(insert_len));
  }
  if trailing_retain > 0 {
    delta.push(CollabTextDelta::Retain(trailing_retain));
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

pub(super) fn body_input_paragraph(doc: &LoroDoc, target_paragraph_ix: usize) -> Option<InputParagraph> {
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
