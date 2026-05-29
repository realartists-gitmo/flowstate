#[hotpath::measure]
pub fn selection_run_styles(document: &Document, range: Range<DocumentOffset>) -> Vec<RunStyles> {
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

#[hotpath::measure]
pub fn selection_prefers_direct_underline(document: &Document, range: Range<DocumentOffset>) -> bool {
  (range.start.paragraph..=range.end.paragraph)
    .any(|paragraph_ix| matches!(document.paragraphs[paragraph_ix].style, ParagraphStyle::Tag | ParagraphStyle::Analytic))
}

#[hotpath::measure]
pub fn selection_all_run_styles(document: &Document, range: Range<DocumentOffset>, predicate: impl Fn(RunStyles) -> bool) -> bool {
  let styles = selection_run_styles(document, range);
  !styles.is_empty() && styles.into_iter().all(predicate)
}

#[hotpath::measure]
pub fn selection_all_underline_kind(document: &Document, range: Range<DocumentOffset>, direct: bool) -> bool {
  selection_all_run_styles(document, range, |styles| {
    if direct {
      styles.direct_underline
    } else {
      styles.semantic == RunSemanticStyle::Underline
    }
  })
}

#[hotpath::measure]
pub fn selection_contains_whole_paragraph(document: &Document, range: Range<DocumentOffset>) -> bool {
  (range.start.paragraph..=range.end.paragraph).any(|paragraph_ix| {
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(&document.paragraphs[paragraph_ix])
    };
    start == 0 && end == paragraph_text_len(&document.paragraphs[paragraph_ix])
  })
}

#[hotpath::measure]
pub fn clear_whole_paragraph_formatting(document: &mut Document, paragraph_ix: usize) {
  let Some(paragraph) = paragraphs_mut(document).get_mut(paragraph_ix) else {
    return;
  };
  let old_style = paragraph.style;
  let old_runs = paragraph.runs.clone();
  paragraph.style = ParagraphStyle::Normal;
  for run in &mut paragraph.runs {
    run.styles = RunStyles::default();
  }
  paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.runs));
  if paragraph.style != old_style || paragraph.runs != old_runs {
    bump_paragraph_version(paragraph);
    update_paragraph_block(document, paragraph_ix);
  }
}

