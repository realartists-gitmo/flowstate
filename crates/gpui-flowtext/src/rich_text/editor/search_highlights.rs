#[hotpath::measure_all]
impl RichTextEditor {
  pub fn set_search_highlights(&mut self, highlights: Vec<Range<DocumentOffset>>, active: Option<usize>, cx: &mut Context<Self>) {
    self.search_highlights = highlights;
    self.active_search_highlight = active.filter(|ix| *ix < self.search_highlights.len());
    cx.notify();
  }

  pub fn clear_search_highlights(&mut self, cx: &mut Context<Self>) {
    self.search_highlights.clear();
    self.active_search_highlight = None;
    cx.notify();
  }

  pub fn set_active_search_highlight(&mut self, active: Option<usize>, cx: &mut Context<Self>) {
    self.active_search_highlight = active.filter(|ix| *ix < self.search_highlights.len());
    if let Some(ix) = self.active_search_highlight {
      let range = self.search_highlights[ix].clone();
      self.pending_snap_to_paragraph = Some((range.start.paragraph, 3));
    }
    cx.notify();
  }

  pub fn replace_active_search_highlight(&mut self, replacement: &str, cx: &mut Context<Self>) -> bool {
    let Some(ix) = self.active_search_highlight else {
      return false;
    };
    let Some(range) = self.search_highlights.get(ix).cloned() else {
      return false;
    };

    self.selection = EditorSelection {
      anchor: range.start,
      head: range.end,
    };
    self.apply_document_edit(cx, |editor, cx| {
      editor.insert_text(replacement, cx);
    });
    self.clear_search_highlights(cx);
    true
  }

  pub fn replace_all_search_highlights(&mut self, replacement: &str, cx: &mut Context<Self>) -> usize {
    let mut ranges = std::mem::take(&mut self.search_highlights)
      .into_iter()
      .filter(|range| self.search_highlight_range_is_valid(range))
      .collect::<Vec<_>>();
    if ranges.is_empty() {
      self.active_search_highlight = None;
      cx.notify();
      return 0;
    }

    ranges.sort_by(|left, right| left.start.cmp(&right.start).then_with(|| left.end.cmp(&right.end)));
    let count = ranges.len();
    let paragraph_count = self.document.paragraphs.len();
    self.apply_document_edit_with_capture_range(cx, Some(0..paragraph_count), |editor, cx| {
      let mut final_caret = None;
      let mut paragraph_groups = Vec::new();
      let mut cross_paragraph_ranges = Vec::new();
      for range in ranges {
        if range.start.paragraph == range.end.paragraph {
          if paragraph_groups
            .last()
            .is_none_or(|(paragraph_ix, _): &(usize, Vec<Range<usize>>)| *paragraph_ix != range.start.paragraph)
          {
            paragraph_groups.push((range.start.paragraph, Vec::new()));
          }
          paragraph_groups
            .last_mut()
            .expect("paragraph group was just inserted")
            .1
            .push(range.start.byte..range.end.byte);
        } else {
          cross_paragraph_ranges.push(range);
        }
      }

      for range in cross_paragraph_ranges.into_iter().rev() {
        editor.selection = EditorSelection {
          anchor: range.start,
          head: range.end,
        };
        editor.insert_text(replacement, cx);
        final_caret = Some(editor.selection.head);
      }

      let affected_start = paragraph_groups.first().map(|(paragraph_ix, _)| *paragraph_ix);
      let affected_end = paragraph_groups.last().map(|(paragraph_ix, _)| paragraph_ix + 1);
      for (paragraph_ix, matches) in paragraph_groups.into_iter().rev() {
        final_caret = replace_paragraph_matches(&mut editor.document, paragraph_ix, &matches, replacement).or(final_caret);
      }
      if let (Some(start), Some(end)) = (affected_start, affected_end) {
        rebuild_document_offset_index(&mut editor.document);
        let replacements = editor.document.paragraphs[start..end.min(editor.document.paragraphs.len())].to_vec();
        replace_paragraph_blocks(&mut editor.document, start, replacements.len(), &replacements);
      }
      if let Some(caret) = final_caret {
        editor.selection = EditorSelection { anchor: caret, head: caret };
      }
      editor.after_text_mutation(cx);
    });
    self.search_highlights.clear();
    self.active_search_highlight = None;
    cx.notify();
    count
  }

  fn search_highlight_range_is_valid(&self, range: &Range<DocumentOffset>) -> bool {
    if range.start > range.end || range.end.paragraph >= self.document.paragraphs.len() {
      return false;
    }
    let Some(start_paragraph) = self.document.paragraphs.get(range.start.paragraph) else {
      return false;
    };
    let Some(end_paragraph) = self.document.paragraphs.get(range.end.paragraph) else {
      return false;
    };
    range.start.byte <= paragraph_text_len(start_paragraph) && range.end.byte <= paragraph_text_len(end_paragraph)
  }
}

fn replace_paragraph_matches(
  document: &mut Document,
  paragraph_ix: usize,
  matches: &[Range<usize>],
  replacement: &str,
) -> Option<DocumentOffset> {
  if matches.is_empty() {
    return None;
  }
  let old_text = paragraph_text(document, paragraph_ix);
  let old_paragraph = document.paragraphs.get(paragraph_ix)?.clone();
  let final_len = old_text
    .len()
    .saturating_add(matches.len().saturating_mul(replacement.len()))
    .saturating_sub(matches.iter().map(|range| range.len()).sum::<usize>());
  let mut new_text = String::with_capacity(final_len);
  let mut new_runs = Vec::with_capacity(old_paragraph.runs.len() + matches.len().saturating_mul(2));
  let mut cursor = 0;
  for range in matches {
    append_original_paragraph_span(&old_text, &old_paragraph, cursor..range.start, &mut new_text, &mut new_runs);
    let styles = styles_at_byte(&old_paragraph, range.start);
    new_text.push_str(replacement);
    push_merged_run(&mut new_runs, TextRun { len: replacement.len(), styles });
    cursor = range.end;
  }
  append_original_paragraph_span(&old_text, &old_paragraph, cursor..old_text.len(), &mut new_text, &mut new_runs);

  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  document.text.delete(paragraph_range.clone());
  document.text.insert(paragraph_range.start, &new_text);
  let paragraph = &mut paragraphs_mut(document)[paragraph_ix];
  paragraph.runs = new_runs;
  paragraph.byte_range = paragraph_range.start..paragraph_range.start + new_text.len();
  bump_paragraph_version(paragraph);
  Some(DocumentOffset {
    paragraph: paragraph_ix,
    byte: matches.last()?.start + replacement.len(),
  })
}

fn append_original_paragraph_span(
  old_text: &str,
  old_paragraph: &Paragraph,
  span: Range<usize>,
  new_text: &mut String,
  new_runs: &mut Vec<TextRun>,
) {
  if span.is_empty() {
    return;
  }
  new_text.push_str(&old_text[span.clone()]);
  let mut run_start = 0;
  for run in &old_paragraph.runs {
    let run_end = run_start + run.len;
    let start = span.start.max(run_start);
    let end = span.end.min(run_end);
    if start < end {
      push_merged_run(new_runs, TextRun { len: end - start, styles: run.styles });
    }
    run_start = run_end;
    if run_start >= span.end {
      break;
    }
  }
}

fn styles_at_byte(paragraph: &Paragraph, byte: usize) -> RunStyles {
  let (run_ix, _) = run_containing(paragraph, byte);
  paragraph.runs.get(run_ix).map_or_else(RunStyles::default, |run| run.styles)
}

fn push_merged_run(runs: &mut Vec<TextRun>, run: TextRun) {
  if run.len == 0 {
    return;
  }
  if let Some(last) = runs.last_mut()
    && last.styles == run.styles
  {
    last.len += run.len;
    return;
  }
  runs.push(run);
}
