use flowstate_document::{
  CollabPatch, CollabTextDelta, HighlightStyle, InputParagraph, InputRun, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE,
  MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, ParagraphStyle, RunSemanticStyle, RunStyles, loro_schema::body_text,
};
use loro::{LoroDoc, LoroValue};
use rustc_hash::FxHashMap;

use super::{ProjectionInvalidation, paragraph_style_from_attrs};

pub(super) fn remote_body_text_patch(
  before: &str,
  after: &str,
  doc: &LoroDoc,
  frontier_before: Vec<u8>,
  frontier_after: Vec<u8>,
) -> Option<(Vec<CollabPatch>, ProjectionInvalidation)> {
  if before == after || before.contains(OBJECT_REPLACEMENT) || after.contains(OBJECT_REPLACEMENT) {
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
  if before_changed.contains('\n') || after_changed.contains('\n') {
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
      row: before_location.paragraph_ix,
      new: new_paragraph,
      delta_utf8,
    }],
    invalidation,
  ))
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

fn body_input_paragraph(doc: &LoroDoc, target_paragraph_ix: usize) -> Option<InputParagraph> {
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
