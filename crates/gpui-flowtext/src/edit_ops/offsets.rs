// §perf: not hotpath-measured — a field sum called on every offset/length
// query; the measurement hooks dwarfed the work and skewed profiles.
#[inline]
#[must_use]
pub fn paragraph_runs_len(paragraph: &Paragraph) -> usize {
  paragraph.runs.iter().map(|run| run.len).sum()
}

/// §act-four M3: the document-text byte length a block contributes to the
/// body — a paragraph's run bytes, or an object's single U+FFFC placeholder
/// (`3` UTF-8 bytes). The [`crate::BlockTree`] monoid measures blocks by this,
/// so `offset ↔ block` queries stay consistent with the body rope.
#[must_use]
pub fn block_text_byte_len(block: &Block) -> usize {
  match block {
    Block::Paragraph(paragraph) => paragraph_runs_len(paragraph),
    Block::Image(_) | Block::Equation(_) | Block::Table(_) => '\u{FFFC}'.len_utf8(),
  }
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

#[hotpath::measure]
pub fn refresh_paragraph_range(document: &mut DocumentProjection, paragraph_ix: usize) {
  let range = paragraph_byte_range(document, paragraph_ix);
  if let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix) {
    paragraph.byte_range = range;
  }
}

#[hotpath::measure]
pub fn refresh_paragraph_ranges(document: &mut DocumentProjection) {
  for paragraph_ix in 0..document.paragraphs.len() {
    refresh_paragraph_range(document, paragraph_ix);
  }
}

// §act-four M3 Slice 2b: the Fenwick `ParagraphOffsetIndex` is gone — paragraph
// offsets are derived from the block tree's paragraph monoid. This now only
// refreshes the cached per-paragraph `byte_range`s from the tree. The name is
// kept so the ~call sites that request an offset refresh read unchanged.
#[hotpath::measure]
pub fn rebuild_document_offset_index(document: &mut DocumentProjection) {
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

  let start = document.paragraphs[paragraph_ix].byte_range.start;
  {
    let paragraphs = paragraphs_mut(document);
    if let Some(paragraph) = paragraphs.get_mut(paragraph_ix) {
      paragraph.byte_range = start..start + new_len;
    }
    if delta != 0 {
      for paragraph in paragraphs.iter_mut().skip(paragraph_ix + 1) {
        paragraph.byte_range = shift_byte_range(&paragraph.byte_range, delta);
      }
    }
  }

  // §perf: mirror the edit into the block copy. §act-four M3 Slice 3: the block
  // tree's copy-on-write `update_at`/`map_from_mut` mutate leaves in place when
  // the tree is uniquely owned (the hot keystroke case), so this matches the old
  // `Arc<Vec<Block>>` in-place shift with NO per-block deep clone — while still
  // preserving persistence (a retained version COWs). `byte_range` does not enter
  // the tree's `Summary`, so the trailing shift only path-refreshes summaries.
  // In the aligned (object-free) case, edit the matching block directly and shift
  // only the tail; otherwise walk once, tracking paragraph rank.
  let source = document.paragraphs[paragraph_ix].clone();
  let aligned = document.blocks.len() == document.paragraphs.len();
  if aligned {
    document.blocks.update_at(paragraph_ix, |block| {
      if let Block::Paragraph(paragraph) = block {
        paragraph.clone_from(&source);
      }
    });
    if delta != 0 {
      document.blocks.map_from_mut(paragraph_ix + 1, |block| {
        if let Block::Paragraph(paragraph) = block {
          paragraph.byte_range = shift_byte_range(&paragraph.byte_range, delta);
        }
      });
    }
  } else {
    let mut paragraph_ord = 0usize;
    document.blocks.map_from_mut(0, |block| {
      if let Block::Paragraph(paragraph) = block {
        if paragraph_ord == paragraph_ix {
          paragraph.clone_from(&source);
        } else if paragraph_ord > paragraph_ix && delta != 0 {
          paragraph.byte_range = shift_byte_range(&paragraph.byte_range, delta);
        }
        paragraph_ord += 1;
      }
    });
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
