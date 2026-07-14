#[hotpath::measure]
#[must_use]
pub fn selection_run_styles(document: &DocumentProjection, range: Range<DocumentOffset>) -> Vec<RunStyles> {
  let mut styles = Vec::new();
  if document.paragraphs.is_empty() || range.start.paragraph >= document.paragraphs.len() {
    return styles;
  }
  let last_paragraph = range.end.paragraph.min(document.paragraphs.len() - 1);
  for paragraph_ix in range.start.paragraph..=last_paragraph {
    let paragraph = &document.paragraphs[paragraph_ix];
    let paragraph_len = paragraph_text_len(paragraph);
    let start = if paragraph_ix == range.start.paragraph {
      range.start.byte.min(paragraph_len)
    } else {
      0
    };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte.min(paragraph_len)
    } else {
      paragraph_len
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
#[must_use]
pub fn selection_prefers_direct_underline(document: &DocumentProjection, range: Range<DocumentOffset>) -> bool {
  if document.paragraphs.is_empty() || range.start.paragraph >= document.paragraphs.len() {
    return false;
  }
  let last_paragraph = range.end.paragraph.min(document.paragraphs.len() - 1);
  (range.start.paragraph..=last_paragraph)
    .any(|paragraph_ix| matches!(document.paragraphs[paragraph_ix].style, ParagraphStyle::Custom(3) | ParagraphStyle::Custom(4)))
}

// §perf: scan the intersecting runs directly with an early exit instead of
// allocating a Vec<RunStyles> via selection_run_styles just to fold it away.
// This feeds toolbar/formatting state that is recomputed on every selection change.
#[hotpath::measure]
pub fn selection_all_run_styles(document: &DocumentProjection, range: Range<DocumentOffset>, predicate: impl Fn(RunStyles) -> bool) -> bool {
  if document.paragraphs.is_empty() || range.start.paragraph >= document.paragraphs.len() {
    return false;
  }
  let last_paragraph = range.end.paragraph.min(document.paragraphs.len() - 1);
  let mut saw_any = false;
  for paragraph_ix in range.start.paragraph..=last_paragraph {
    let paragraph = &document.paragraphs[paragraph_ix];
    let paragraph_len = paragraph_text_len(paragraph);
    let start = if paragraph_ix == range.start.paragraph {
      range.start.byte.min(paragraph_len)
    } else {
      0
    };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte.min(paragraph_len)
    } else {
      paragraph_len
    };
    let mut offset = 0;
    for run in &paragraph.runs {
      let run_start = offset;
      let run_end = offset + run.len;
      offset = run_end;
      if run_start < end && run_end > start {
        saw_any = true;
        if !predicate(run.styles) {
          return false;
        }
      }
    }
  }
  saw_any
}

#[hotpath::measure]
#[must_use]
pub fn selection_all_underline_kind(document: &DocumentProjection, range: Range<DocumentOffset>, direct: bool) -> bool {
  selection_all_run_styles(document, range, |styles| {
    if direct {
      styles.direct_underline
    } else {
      styles.semantic == RunSemanticStyle::Custom(3)
    }
  })
}

#[hotpath::measure]
#[must_use]
pub fn selection_contains_whole_paragraph(document: &DocumentProjection, range: Range<DocumentOffset>) -> bool {
  if document.paragraphs.is_empty() || range.start.paragraph >= document.paragraphs.len() {
    return false;
  }
  let last_paragraph = range.end.paragraph.min(document.paragraphs.len() - 1);
  (range.start.paragraph..=last_paragraph).any(|paragraph_ix| {
    let paragraph_len = paragraph_text_len(&document.paragraphs[paragraph_ix]);
    let start = if paragraph_ix == range.start.paragraph {
      range.start.byte.min(paragraph_len)
    } else {
      0
    };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte.min(paragraph_len)
    } else {
      paragraph_len
    };
    start == 0 && end == paragraph_len
  })
}

#[hotpath::measure]
pub fn clear_whole_paragraph_formatting(document: &mut DocumentProjection, paragraph_ix: usize) {
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

