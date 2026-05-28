#[hotpath::measure]
pub fn mutate_runs_in_range(document: &mut Document, range: Range<DocumentOffset>, mut mutate: impl FnMut(&mut RunStyles)) {
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    let paragraph = &mut paragraphs_mut(document)[paragraph_ix];
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
      update_paragraph_block(document, paragraph_ix);
    } else {
      paragraph.runs = old_runs;
    }
  }
}
