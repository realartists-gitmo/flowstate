pub(super) fn fragments_for_range(paragraph: &Paragraph, range: &Range<usize>, rendered_text: &str) -> Vec<VisualFragment> {
  let mut byte_offset = 0;
  let rendered_len = rendered_text.len();
  let mut fragments = Vec::with_capacity(paragraph.runs.len());
  for run in &paragraph.runs {
    let run_start = byte_offset;
    let run_end = byte_offset + run.len;
    byte_offset = run_end;
    let start = run_start.max(range.start);
    let end = run_end.min(range.end);
    if start >= end || rendered_len == 0 {
      continue;
    }
    let line_start = ceil_char_boundary(rendered_text, start.saturating_sub(range.start).min(rendered_len));
    let line_end = ceil_char_boundary(rendered_text, end.saturating_sub(range.start).min(rendered_len));
    if line_start >= line_end {
      continue;
    }
    fragments.push(VisualFragment {
      styles: run.styles,
      line_range: line_start..line_end,
      run_range: run_start..run_end,
      source_start: range.start + line_start,
    });
  }
  fragments
}
