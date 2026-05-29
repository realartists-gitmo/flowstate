#[hotpath::measure]
pub fn paragraph_runs_len(paragraph: &Paragraph) -> usize {
  paragraph.runs.iter().map(|run| run.len).sum()
}

#[hotpath::measure]
pub fn paragraph_widths(paragraphs: &[Paragraph]) -> Vec<usize> {
  paragraphs
    .iter()
    .enumerate()
    .map(|(ix, _)| paragraph_width(paragraphs, ix).unwrap_or(0))
    .collect()
}

#[hotpath::measure]
pub fn paragraph_width(paragraphs: &[Paragraph], paragraph_ix: usize) -> Option<usize> {
  let paragraph = paragraphs.get(paragraph_ix)?;
  let newline_len = usize::from(paragraph_ix + 1 < paragraphs.len());
  Some(paragraph_runs_len(paragraph) + newline_len)
}

#[hotpath::measure]
pub fn paragraph_byte_range(document: &Document, paragraph_ix: usize) -> Range<usize> {
  let start = document.offset_index.paragraph_start(paragraph_ix);
  start..start + paragraph_text_len(&document.paragraphs[paragraph_ix])
}

#[hotpath::measure]
pub fn refresh_paragraph_range(document: &mut Document, paragraph_ix: usize) {
  let range = paragraph_byte_range(document, paragraph_ix);
  paragraphs_mut(document)[paragraph_ix].byte_range = range;
}

#[hotpath::measure]
pub fn refresh_paragraph_ranges(document: &mut Document) {
  for paragraph_ix in 0..document.paragraphs.len() {
    refresh_paragraph_range(document, paragraph_ix);
  }
}

#[hotpath::measure]
fn refresh_paragraph_ranges_from(document: &mut Document, start_paragraph: usize) {
  let paragraph_count = document.paragraphs.len();
  for paragraph_ix in start_paragraph.min(paragraph_count)..paragraph_count {
    refresh_paragraph_range(document, paragraph_ix);
  }
}

#[hotpath::measure]
pub fn rebuild_document_offset_index(document: &mut Document) {
  document.offset_index.rebuild(&document.paragraphs);
  refresh_paragraph_ranges(document);
}

#[hotpath::measure]
pub fn update_paragraph_offsets_after_len_change(document: &mut Document, paragraph_ix: usize) {
  if paragraph_ix >= document.paragraphs.len() {
    return;
  }
  document
    .offset_index
    .update_paragraph_width(paragraph_ix, &document.paragraphs);
  refresh_paragraph_ranges_from(document, paragraph_ix);
  let paragraph_count = document.paragraphs.len();
  let replacements = document.paragraphs[paragraph_ix..].to_vec();
  replace_paragraph_blocks(
    document,
    paragraph_ix,
    paragraph_count.saturating_sub(paragraph_ix),
    &replacements,
  );
}

// Returns `(run_index, local_byte)` for the given absolute byte offset within
// the paragraph. Biases to the LEFT run at run boundaries — i.e. when `byte`
// equals the end of run i and the start of run i+1, we return run i. This is
// what lets typed text inherit styles from the run "just before the caret".
#[hotpath::measure]
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
