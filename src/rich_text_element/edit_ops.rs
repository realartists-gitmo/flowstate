use std::ops::Range;

use super::*;

pub(super) fn apply_style_to_paragraph_range(document: &mut Document, paragraph_ix: usize, range: Range<usize>, style: RunStyle) {
  if range.start >= range.end {
    return;
  }
  let Some(paragraph) = document.paragraphs.get_mut(paragraph_ix) else {
    return;
  };
  let mut output = Vec::with_capacity(paragraph.runs.len() + 2);
  let mut offset = 0;
  let old_runs = std::mem::take(&mut paragraph.runs);
  for run in &old_runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= range.start || run_start >= range.end {
      output.push(run.clone());
      continue;
    }
    let local_start = range.start.saturating_sub(run_start).min(run.len);
    let local_end = (range.end.saturating_sub(run_start)).min(run.len);
    if local_start > 0 {
      output.push(TextRun {
        len: local_start,
        styles: run.styles,
      });
    }
    let mut styles = run.styles;
    styles.apply(style);
    output.push(TextRun {
      len: local_end - local_start,
      styles,
    });
    if local_end < run.len {
      output.push(TextRun {
        len: run.len - local_end,
        styles: run.styles,
      });
    }
  }
  let new_runs = merge_adjacent_runs(output);
  if new_runs != old_runs {
    paragraph.runs = new_runs;
    bump_paragraph_version(paragraph);
  } else {
    paragraph.runs = old_runs;
  }
}

pub(super) fn merge_adjacent_runs(runs: Vec<TextRun>) -> Vec<TextRun> {
  let mut merged: Vec<TextRun> = Vec::with_capacity(runs.len());
  for run in runs {
    if run.len == 0 {
      continue;
    }
    if let Some(last) = merged.last_mut()
      && last.styles == run.styles
    {
      last.len += run.len;
      continue;
    }
    merged.push(run);
  }
  merged
}

pub(super) fn paragraph_text(document: &Document, paragraph_ix: usize) -> String {
  document_text_slice(document, paragraph_byte_range(document, paragraph_ix))
}

pub(super) fn paragraph_text_len(paragraph: &Paragraph) -> usize {
  paragraph_runs_len(paragraph)
}

pub(super) fn document_text_slice(document: &Document, range: Range<usize>) -> String {
  let mut text = String::with_capacity(range.end - range.start);
  for chunk in document.text.byte_slice(range).chunks() {
    text.push_str(chunk);
  }
  text
}

pub(super) fn capture_document_span(document: &Document, range: Range<usize>) -> DocumentSpan {
  let start = range.start.min(document.paragraphs.len());
  let end = range.end.min(document.paragraphs.len()).max(start);
  let text = if start < end {
    let byte_range = paragraph_span_byte_range(document, start, end - start);
    document_text_slice(document, byte_range)
  } else {
    String::new()
  };
  DocumentSpan {
    start_paragraph: start,
    paragraphs: document.paragraphs[start..end].to_vec(),
    text,
  }
}

pub(super) fn apply_document_span_replacement(document: &mut Document, current: &DocumentSpan, replacement: &DocumentSpan) {
  let byte_range = paragraph_span_byte_range(document, current.start_paragraph, current.paragraphs.len());
  document.text.delete(byte_range.clone());
  document.text.insert(byte_range.start, &replacement.text);
  let paragraph_end = current
    .start_paragraph
    .saturating_add(current.paragraphs.len())
    .min(document.paragraphs.len());
  document
    .paragraphs
    .splice(current.start_paragraph..paragraph_end, replacement.paragraphs.clone());
  rebuild_document_offset_index(document);
}

pub(super) fn paragraph_span_byte_range(document: &Document, start_paragraph: usize, paragraph_count: usize) -> Range<usize> {
  if paragraph_count == 0 || start_paragraph >= document.paragraphs.len() {
    let byte = document
      .paragraphs
      .get(start_paragraph)
      .map(|_| paragraph_byte_range(document, start_paragraph).start)
      .unwrap_or_else(|| document.text.byte_len());
    return byte..byte;
  }
  let end_paragraph = (start_paragraph + paragraph_count - 1).min(document.paragraphs.len() - 1);
  paragraph_byte_range(document, start_paragraph).start..paragraph_byte_range(document, end_paragraph).end
}

pub(super) fn full_document_text(document: &Document) -> String {
  document_text_slice(document, 0..document.text.byte_len())
}

pub(super) fn document_end(document: &Document) -> DocumentOffset {
  let paragraph = document.paragraphs.len().saturating_sub(1);
  DocumentOffset {
    paragraph,
    byte: document
      .paragraphs
      .get(paragraph)
      .map(paragraph_text_len)
      .unwrap_or(0),
  }
}

pub(super) fn global_byte(document: &Document, offset: DocumentOffset) -> usize {
  paragraph_byte_range(document, offset.paragraph).start + offset.byte
}

pub(super) fn global_to_document_offset(document: &Document, byte: usize) -> DocumentOffset {
  let byte = byte.min(document.text.byte_len());
  let mut low = 0;
  let mut high = document.paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraph_byte_range(document, mid).end < byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  let Some(paragraph) = document.paragraphs.get(low) else {
    return document_end(document);
  };
  DocumentOffset {
    paragraph: low,
    byte: byte
      .saturating_sub(paragraph_byte_range(document, low).start)
      .min(paragraph_text_len(paragraph)),
  }
}

pub(super) fn selected_plain_text(document: &Document, range: Range<DocumentOffset>) -> String {
  if range.start.paragraph == range.end.paragraph {
    let paragraph_range = paragraph_byte_range(document, range.start.paragraph);
    return document_text_slice(document, paragraph_range.start + range.start.byte..paragraph_range.start + range.end.byte);
  }

  let mut text = String::new();
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    if paragraph_ix > range.start.paragraph {
      text.push('\n');
    }
    let paragraph = &document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    let paragraph_range = paragraph_byte_range(document, paragraph_ix);
    text.push_str(&document_text_slice(document, paragraph_range.start + start..paragraph_range.start + end));
  }
  text
}

pub(super) fn selected_rich_fragment(document: &Document, range: Range<DocumentOffset>) -> RichClipboardFragment {
  let mut paragraphs = Vec::new();
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    let paragraph = &document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    let mut runs = Vec::new();
    let mut offset = 0;
    for run in &paragraph.runs {
      let run_start = offset;
      let run_end = offset + run.len;
      offset = run_end;
      let clipped_start = run_start.max(start);
      let clipped_end = run_end.min(end);
      if clipped_start < clipped_end {
        let paragraph_range = paragraph_byte_range(document, paragraph_ix);
        runs.push(InputRun {
          text: document_text_slice(document, paragraph_range.start + clipped_start..paragraph_range.start + clipped_end),
          styles: run.styles,
        });
      }
    }
    paragraphs.push(InputParagraph {
      style: paragraph.style,
      runs,
    });
  }
  RichClipboardFragment {
    format: "debateprocessor.rich-text-fragment.v1".to_string(),
    paragraphs,
  }
}

pub(super) fn bump_paragraph_version(paragraph: &mut Paragraph) {
  paragraph.version = paragraph.version.wrapping_add(1);
}

pub(super) fn split_runs_at(runs: &[TextRun], byte: usize) -> (Vec<TextRun>, Vec<TextRun>) {
  let mut left = Vec::new();
  let mut right = Vec::new();
  let mut offset = 0;
  for run in runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= byte {
      left.push(run.clone());
    } else if run_start >= byte {
      right.push(run.clone());
    } else {
      let left_len = byte - run_start;
      let right_len = run_end - byte;
      if left_len > 0 {
        left.push(TextRun {
          len: left_len,
          styles: run.styles,
        });
      }
      if right_len > 0 {
        right.push(TextRun {
          len: right_len,
          styles: run.styles,
        });
      }
    }
  }
  (merge_adjacent_runs(left), merge_adjacent_runs(right))
}

pub(super) fn split_paragraph_at(document: &mut Document, paragraph_ix: usize, byte: usize) {
  let paragraph = document.paragraphs[paragraph_ix].clone();
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  let global = paragraph_range.start + byte;
  document.text.insert(global, "\n");
  let (left_runs, right_runs) = split_runs_at(&paragraph.runs, byte);
  let old_end = paragraph_range.end;
  document.paragraphs[paragraph_ix].byte_range = paragraph_range.start..global;
  document.paragraphs[paragraph_ix].runs = left_runs;
  bump_paragraph_version(&mut document.paragraphs[paragraph_ix]);
  document.paragraphs.insert(
    paragraph_ix + 1,
    Paragraph {
      style: paragraph.style,
      byte_range: global + 1..old_end + 1,
      runs: right_runs,
      version: paragraph.version.wrapping_add(1),
    },
  );
  rebuild_document_offset_index(document);
}

pub(super) fn delete_cross_paragraph_range(document: &mut Document, range: Range<DocumentOffset>) {
  if range.start.paragraph >= range.end.paragraph {
    delete_range_in_paragraph(document, range.start.paragraph, range.start.byte..range.end.byte);
    return;
  }

  let start_ix = range.start.paragraph;
  let end_ix = range.end.paragraph;
  let start_para = document.paragraphs[start_ix].clone();
  let end_para = document.paragraphs[end_ix].clone();
  let start_para_range = paragraph_byte_range(document, start_ix);
  let end_para_range = paragraph_byte_range(document, end_ix);
  let start_global = start_para_range.start + range.start.byte;
  let end_global = end_para_range.start + range.end.byte;
  let delete_len = end_global - start_global;

  let (left_runs, _) = split_runs_at(&start_para.runs, range.start.byte);
  let (_, right_runs) = split_runs_at(&end_para.runs, range.end.byte);
  document.text.delete(start_global..end_global);

  let mut merged_runs = left_runs;
  merged_runs.extend(right_runs);
  document.paragraphs[start_ix].runs = merge_adjacent_runs(merged_runs);
  document.paragraphs[start_ix].byte_range = start_para_range.start..start_para_range.start + paragraph_runs_len(&document.paragraphs[start_ix]);
  bump_paragraph_version(&mut document.paragraphs[start_ix]);
  document.paragraphs.drain(start_ix + 1..=end_ix);
  let _ = delete_len;
  rebuild_document_offset_index(document);
}

pub(super) fn paragraph_runs_len(paragraph: &Paragraph) -> usize {
  paragraph.runs.iter().map(|run| run.len).sum()
}

pub(super) fn paragraph_widths(paragraphs: &[Paragraph]) -> Vec<usize> {
  paragraphs
    .iter()
    .enumerate()
    .map(|(ix, _)| paragraph_width(paragraphs, ix).unwrap_or(0))
    .collect()
}

pub(super) fn paragraph_width(paragraphs: &[Paragraph], paragraph_ix: usize) -> Option<usize> {
  let paragraph = paragraphs.get(paragraph_ix)?;
  let newline_len = usize::from(paragraph_ix + 1 < paragraphs.len());
  Some(paragraph_runs_len(paragraph) + newline_len)
}

pub(super) fn paragraph_byte_range(document: &Document, paragraph_ix: usize) -> Range<usize> {
  let start = document.offset_index.paragraph_start(paragraph_ix);
  start..start + paragraph_text_len(&document.paragraphs[paragraph_ix])
}

pub(super) fn refresh_paragraph_range(document: &mut Document, paragraph_ix: usize) {
  let range = paragraph_byte_range(document, paragraph_ix);
  document.paragraphs[paragraph_ix].byte_range = range;
}

pub(super) fn refresh_paragraph_ranges(document: &mut Document) {
  for paragraph_ix in 0..document.paragraphs.len() {
    refresh_paragraph_range(document, paragraph_ix);
  }
}

pub(super) fn rebuild_document_offset_index(document: &mut Document) {
  document.offset_index.rebuild(&document.paragraphs);
  refresh_paragraph_ranges(document);
}

pub(super) fn update_paragraph_offsets_after_len_change(document: &mut Document, paragraph_ix: usize) {
  document
    .offset_index
    .update_paragraph_width(paragraph_ix, &document.paragraphs);
  refresh_paragraph_range(document, paragraph_ix);
}

// Returns `(run_index, local_byte)` for the given absolute byte offset within
// the paragraph. Biases to the LEFT run at run boundaries — i.e. when `byte`
// equals the end of run i and the start of run i+1, we return run i. This is
// what lets typed text inherit styles from the run "just before the caret".
pub(super) fn run_containing(paragraph: &Paragraph, byte: usize) -> (usize, usize) {
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

// Inserts `text` (with `styles`) into `paragraph` at `byte`. Splits the run
// straddling the byte if needed and re-merges adjacent runs with identical
// styles afterwards.
pub(super) fn insert_text_at(document: &mut Document, paragraph_ix: usize, byte: usize, text: &str, styles: RunStyles) {
  if text.is_empty() {
    return;
  }
  let insert_len = text.len();
  let paragraph_start = paragraph_byte_range(document, paragraph_ix).start;
  document.text.insert(paragraph_start + byte, text);
  let paragraph = &mut document.paragraphs[paragraph_ix];
  bump_paragraph_version(paragraph);
  if paragraph.runs.is_empty() {
    paragraph.runs.push(TextRun { len: insert_len, styles });
    update_paragraph_offsets_after_len_change(document, paragraph_ix);
    return;
  }

  let mut offset = 0;
  let mut inserted = false;
  for i in 0..paragraph.runs.len() {
    let run_start = offset;
    let run_len = paragraph.runs[i].len;
    let run_end = run_start + run_len;
    if byte <= run_end {
      let local = byte - run_start;

      if paragraph.runs[i].styles == styles {
        paragraph.runs[i].len += insert_len;
        inserted = true;
        break;
      }

      if local == 0 {
        if i > 0 && paragraph.runs[i - 1].styles == styles {
          paragraph.runs[i - 1].len += insert_len;
        } else {
          paragraph
            .runs
            .insert(i, TextRun { len: insert_len, styles });
        }
        inserted = true;
        break;
      }

      if local == run_len {
        if i + 1 < paragraph.runs.len() && paragraph.runs[i + 1].styles == styles {
          paragraph.runs[i + 1].len += insert_len;
        } else {
          paragraph
            .runs
            .insert(i + 1, TextRun { len: insert_len, styles });
        }
        inserted = true;
        break;
      }

      let run_styles = paragraph.runs[i].styles;
      let right_len = run_len - local;
      paragraph.runs[i].len = local;
      paragraph
        .runs
        .insert(i + 1, TextRun { len: insert_len, styles });
      paragraph.runs.insert(
        i + 2,
        TextRun {
          len: right_len,
          styles: run_styles,
        },
      );
      inserted = true;
      break;
    }
    offset = run_end;
  }

  if !inserted && let Some(last) = paragraph.runs.last_mut() {
    if last.styles == styles {
      last.len += insert_len;
    } else {
      paragraph.runs.push(TextRun { len: insert_len, styles });
    }
  }
  update_paragraph_offsets_after_len_change(document, paragraph_ix);
}

// Removes the half-open byte range `[range.start, range.end)` from
// `paragraph`. Runs are split or dropped as needed; remaining runs are re-
// merged so adjacent same-style fragments coalesce.
pub(super) fn delete_range_in_paragraph(document: &mut Document, paragraph_ix: usize, range: Range<usize>) {
  if range.start >= range.end {
    return;
  }
  let paragraph_start = paragraph_byte_range(document, paragraph_ix).start;
  document
    .text
    .delete(paragraph_start + range.start..paragraph_start + range.end);
  let paragraph = &mut document.paragraphs[paragraph_ix];
  bump_paragraph_version(paragraph);
  let mut offset = 0;
  let mut new_runs: Vec<TextRun> = Vec::with_capacity(paragraph.runs.len());
  for run in paragraph.runs.drain(..) {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= range.start || run_start >= range.end {
      new_runs.push(run);
      continue;
    }
    let local_start = range.start.saturating_sub(run_start).min(run.len);
    let local_end = range.end.saturating_sub(run_start).min(run.len);
    let removed = local_end - local_start;
    let remaining = run.len - removed;
    if remaining > 0 {
      new_runs.push(TextRun {
        len: remaining,
        styles: run.styles,
      });
    }
  }
  paragraph.runs = merge_adjacent_runs(new_runs);
  update_paragraph_offsets_after_len_change(document, paragraph_ix);
}

pub(super) fn selection_run_styles(document: &Document, range: Range<DocumentOffset>) -> Vec<RunStyles> {
  let mut styles = Vec::new();
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    let paragraph = &document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    let mut offset = 0;
    for run in &paragraph.runs {
      let run_start = offset;
      let run_end = offset + run.len;
      offset = run_end;
      if run_start < end && run_end > start {
        styles.push(run.styles);
      }
    }
  }
  styles
}

pub(super) fn selection_prefers_direct_underline(document: &Document, range: Range<DocumentOffset>) -> bool {
  (range.start.paragraph..=range.end.paragraph).any(|paragraph_ix| {
    matches!(
      document.paragraphs[paragraph_ix].style,
      ParagraphStyle::Tag | ParagraphStyle::Analytic | ParagraphStyle::Undertag
    )
  })
}

pub(super) fn selection_has_underline_kind(document: &Document, range: Range<DocumentOffset>, direct: bool) -> bool {
  selection_run_styles(document, range)
    .into_iter()
    .any(|styles| if direct { styles.direct_underline } else { styles.style_underline })
}

pub(super) fn mutate_runs_in_range(document: &mut Document, range: Range<DocumentOffset>, mut mutate: impl FnMut(&mut RunStyles)) {
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    let paragraph = &mut document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    if start >= end {
      continue;
    }

    let mut new_runs = Vec::with_capacity(paragraph.runs.len() + 2);
    let mut offset = 0;
    let old_runs = std::mem::take(&mut paragraph.runs);
    for run in &old_runs {
      let run_start = offset;
      let run_end = offset + run.len;
      offset = run_end;
      if run_end <= start || run_start >= end {
        new_runs.push(run.clone());
        continue;
      }
      if run_start < start {
        new_runs.push(TextRun {
          len: start - run_start,
          styles: run.styles,
        });
      }
      let selected_start = run_start.max(start);
      let selected_end = run_end.min(end);
      let mut selected_styles = run.styles;
      mutate(&mut selected_styles);
      new_runs.push(TextRun {
        len: selected_end - selected_start,
        styles: selected_styles,
      });
      if run_end > end {
        new_runs.push(TextRun {
          len: run_end - end,
          styles: run.styles,
        });
      }
    }
    let new_runs = merge_adjacent_runs(new_runs);
    if new_runs != old_runs {
      paragraph.runs = new_runs;
      bump_paragraph_version(paragraph);
    } else {
      paragraph.runs = old_runs;
    }
  }
}
