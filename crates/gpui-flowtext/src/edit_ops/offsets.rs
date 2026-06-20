#[hotpath::measure]
#[must_use]
pub fn paragraph_runs_len(paragraph: &Paragraph) -> usize {
  paragraph.runs.iter().map(|run| run.len).sum()
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_widths(paragraphs: &[Paragraph]) -> Vec<usize> {
  paragraphs
    .iter()
    .enumerate()
    .map(|(ix, _)| paragraph_width(paragraphs, ix).unwrap_or(0))
    .collect()
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_width(paragraphs: &[Paragraph], paragraph_ix: usize) -> Option<usize> {
  let paragraph = paragraphs.get(paragraph_ix)?;
  let newline_len = usize::from(paragraph_ix + 1 < paragraphs.len());
  Some(paragraph_runs_len(paragraph) + newline_len)
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_byte_range(document: &DocumentProjection, paragraph_ix: usize) -> Range<usize> {
  let start = document.offset_index.paragraph_start(paragraph_ix);
  start..start + paragraph_text_len(&document.paragraphs[paragraph_ix])
}

/// Clamp a paragraph-local UTF-8 byte offset to the nearest valid character
/// boundary at or before it.
///
/// Editor coordinates are byte-based. Keeping this normalization at the
/// document boundary prevents a stale or transformed caret from ever reaching
/// `crop::Rope` with an interior UTF-8 code-unit offset.
#[hotpath::measure]
#[must_use]
pub fn clamp_paragraph_byte_to_char_boundary(document: &DocumentProjection, paragraph_ix: usize, byte: usize) -> usize {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix) else {
    return 0;
  };
  let paragraph_start = document.offset_index.paragraph_start(paragraph_ix);
  let mut byte = byte.min(paragraph_text_len(paragraph));
  while byte > 0 && !document.text.is_char_boundary(paragraph_start + byte) {
    byte -= 1;
  }
  byte
}

#[hotpath::measure]
pub fn refresh_paragraph_range(document: &mut DocumentProjection, paragraph_ix: usize) {
  let range = paragraph_byte_range(document, paragraph_ix);
  paragraphs_mut(document)[paragraph_ix].byte_range = range;
}

#[hotpath::measure]
pub fn refresh_paragraph_ranges(document: &mut DocumentProjection) {
  for paragraph_ix in 0..document.paragraphs.len() {
    refresh_paragraph_range(document, paragraph_ix);
  }
}

#[hotpath::measure]
pub fn rebuild_document_offset_index(document: &mut DocumentProjection) {
  document.offset_index.rebuild(&document.paragraphs);
  refresh_paragraph_ranges(document);
}

fn shift_byte_range(range: &Range<usize>, delta: isize) -> Range<usize> {
  range.start.saturating_add_signed(delta)..range.end.saturating_add_signed(delta)
}

// Per-keystroke hot path: a single paragraph's text length changed (paragraph
// COUNT is unchanged). Update the Fenwick offset index in O(log n), shift the
// cached `byte_range`s of the edited paragraph and the ones after it, and mirror
// the change into the parallel block representation in place. We deliberately do
// NOT clone the paragraph tail, rebuild the block vector, reconcile ids, or
// rebuild the section outline: a content-only edit changes none of those.
#[hotpath::measure]
pub fn update_paragraph_offsets_after_len_change(document: &mut DocumentProjection, paragraph_ix: usize) {
  if paragraph_ix >= document.paragraphs.len() {
    return;
  }
  let new_len = paragraph_text_len(&document.paragraphs[paragraph_ix]);
  let old_len = document.paragraphs[paragraph_ix].byte_range.len();
  let delta = new_len as isize - old_len as isize;
  document
    .offset_index
    .update_paragraph_width(paragraph_ix, &document.paragraphs);

  let start = document.paragraphs[paragraph_ix].byte_range.start;
  {
    let paragraphs = paragraphs_mut(document);
    paragraphs[paragraph_ix].byte_range = start..start + new_len;
    if delta != 0 {
      for paragraph in paragraphs.iter_mut().skip(paragraph_ix + 1) {
        paragraph.byte_range = shift_byte_range(&paragraph.byte_range, delta);
      }
    }
  }

  let mut edited = Some(document.paragraphs[paragraph_ix].clone());
  let blocks = Arc::make_mut(&mut document.blocks);
  let mut paragraph_ord = 0usize;
  for block in blocks.iter_mut() {
    let Block::Paragraph(paragraph) = block else {
      continue;
    };
    if paragraph_ord == paragraph_ix {
      if let Some(updated) = edited.take() {
        *paragraph = updated;
      }
    } else if paragraph_ord > paragraph_ix && delta != 0 {
      paragraph.byte_range = shift_byte_range(&paragraph.byte_range, delta);
    }
    paragraph_ord += 1;
  }
}

// Returns `(run_index, local_byte)` for the given absolute byte offset within
// the paragraph. Biases to the LEFT run at run boundaries — i.e. when `byte`
// equals the end of run i and the start of run i+1, we return run i. This is
// what lets typed text inherit styles from the run "just before the caret".
#[hotpath::measure]
#[must_use]
pub fn run_containing(paragraph: &Paragraph, byte: usize) -> (usize, usize) {
  let mut offset = 0;
  for (ix, run) in paragraph.runs.iter().enumerate() {
    let run_end = offset + run.len;
    if byte <= run_end {
      return (ix, byte - offset);
    }
    offset = run_end;
  }
  // byte is beyond the end — clamp to the last run.
  if paragraph.runs.is_empty() {
    (0, 0)
  } else {
    let last = paragraph.runs.len() - 1;
    (last, paragraph.runs[last].len)
  }
}

#[cfg(test)]
mod offsets_tests {
  use super::*;
  use crate::{DocumentTheme, document_from_input};

  #[test]
  fn insert_text_refreshes_following_paragraph_ranges_and_blocks() {
    let mut document = document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "alpha".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "Kepe et al. ‘23".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
    );

    insert_text_at(&mut document, 0, "alpha".len(), " beta", RunStyles::default());

    assert_eq!(document.paragraphs[1].byte_range, "alpha beta\n".len().."alpha beta\nKepe et al. ‘23".len());
    assert!(matches!(&document.blocks[1], Block::Paragraph(paragraph) if paragraph.byte_range == document.paragraphs[1].byte_range));
  }
}

// Inserts `text` (with `styles`) into `paragraph` at `byte`. Splits the run
// straddling the byte if needed and re-merges adjacent runs with identical
// styles afterwards.
