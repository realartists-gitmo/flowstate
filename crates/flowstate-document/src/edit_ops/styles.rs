use std::{ops::Range, sync::Arc};

use super::*;

#[hotpath::measure]
pub fn apply_style_to_paragraph_range(document: &mut Document, paragraph_ix: usize, range: Range<usize>, style: RunStyle) {
  if range.start >= range.end {
    return;
  }
  let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix) else {
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
    update_paragraph_block(document, paragraph_ix);
  } else {
    paragraph.runs = old_runs;
  }
}

#[hotpath::measure]
pub fn merge_adjacent_runs(runs: Vec<TextRun>) -> Vec<TextRun> {
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

