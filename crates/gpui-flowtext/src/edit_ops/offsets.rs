// §perf: not hotpath-measured — a field sum called on every offset/length
// query; the measurement hooks dwarfed the work and skewed profiles.
#[inline]
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
  // §act-four M3 Slice 2b: paragraph offsets come from the block tree's
  // paragraph-space monoid (`paragraph_start`), which subsumes the Fenwick
  // `ParagraphOffsetIndex`. Valid because the block tree mirrors the paragraph
  // runs, and `paragraph_start` depends only on run lengths (not byte_range).
  let start = document.blocks.paragraph_start(paragraph_ix);
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
  let paragraph_start = document.blocks.paragraph_start(paragraph_ix);
  let mut byte = byte.min(paragraph_text_len(paragraph));
  while byte > 0 && !document.text.is_char_boundary(paragraph_start + byte) {
    byte -= 1;
  }
  byte
}

// Per-keystroke hot path: a single paragraph's text length changed (paragraph
// COUNT is unchanged). §perf-heaven T8.6: `byte_range` is now DERIVED (the block
// tree's `paragraph_start` prefix-sum from run lengths + `paragraph_text_len`),
// so there is NO cached absolute offset to shift across the tail — the former
// O(tail) `im::Vector` + block-tree COW (~3.8 MB/keystroke on a 6k-para doc) is
// gone. The paragraph sequence already holds the edited runs; the only remaining
// work is mirroring the edited paragraph's content into its ONE block copy (the
// two representations hold separate `Paragraph` copies), found in O(log N) via
// the tree rank query; `update_at` refreshes the tree's paragraph-rope summary
// from the new run lengths.
#[hotpath::measure]
pub fn update_paragraph_offsets_after_len_change(document: &mut DocumentProjection, paragraph_ix: usize) {
  if paragraph_ix >= document.paragraphs.len() {
    return;
  }
  let source = document.paragraphs[paragraph_ix].clone();
  let Some(row) = crate::block_ix_for_paragraph(document, paragraph_ix) else {
    return;
  };
  document.blocks.update_at(row, |block| {
    if let Block::Paragraph(paragraph) = block {
      paragraph.clone_from(&source);
    }
  });
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

    // §perf-heaven T8.6: the byte range is now DERIVED from the block tree's
    // `paragraph_start` prefix-sum. Verify the derive tracks the insert, and
    // that the block-tree mirror's runs stayed in sync (which is what makes the
    // derive correct).
    assert_eq!(paragraph_byte_range(&document, 1), "alpha beta\n".len().."alpha beta\nKepe et al. ‘23".len());
    assert!(matches!(&document.blocks[1], Block::Paragraph(paragraph) if paragraph.runs == document.paragraphs[1].runs));
  }
}

// Inserts `text` (with `styles`) into `paragraph` at `byte`. Splits the run
// straddling the byte if needed and re-merges adjacent runs with identical
// styles afterwards.
