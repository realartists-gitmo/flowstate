#[hotpath::measure]
pub fn insert_text_at(document: &mut Document, paragraph_ix: usize, byte: usize, text: &str, styles: RunStyles) -> bool {
  if text.is_empty() {
    return false;
  }
  if !paragraph_offset_is_char_boundary(document, paragraph_ix, byte) {
    return false;
  }
  let insert_len = text.len();
  let paragraph_start = paragraph_byte_range(document, paragraph_ix).start;
  document.text.insert(paragraph_start + byte, text);
  {
    let paragraph = &mut paragraphs_mut(document)[paragraph_ix];
    bump_paragraph_version(paragraph);
    if paragraph.runs.is_empty() {
      paragraph.runs.push(TextRun { len: insert_len, styles });
      update_paragraph_offsets_after_len_change(document, paragraph_ix);
      return true;
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
  }
  update_paragraph_offsets_after_len_change(document, paragraph_ix);
  true
}

// Removes the half-open byte range `[range.start, range.end)` from
// `paragraph`. Runs are split or dropped as needed; remaining runs are re-
// merged so adjacent same-style fragments coalesce.
#[hotpath::measure]
pub fn delete_range_in_paragraph(document: &mut Document, paragraph_ix: usize, range: Range<usize>) -> bool {
  if range.start >= range.end {
    return false;
  }
  if !paragraph_offset_is_char_boundary(document, paragraph_ix, range.start)
    || !paragraph_offset_is_char_boundary(document, paragraph_ix, range.end)
  {
    return false;
  }
  let paragraph_start = paragraph_byte_range(document, paragraph_ix).start;
  document
    .text
    .delete(paragraph_start + range.start..paragraph_start + range.end);
  {
    let paragraph = &mut paragraphs_mut(document)[paragraph_ix];
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
  };
  update_paragraph_offsets_after_len_change(document, paragraph_ix);
  true
}
