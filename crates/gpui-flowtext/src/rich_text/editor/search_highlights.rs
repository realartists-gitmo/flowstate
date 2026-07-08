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

    let before_selection = self.selection.clone();
    self.selection = EditorSelection::range(range.start, range.end);
    if self.selection != before_selection {
      self.emit_selection_changed(cx);
    }
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

    ranges.sort_by(|left, right| {
      left
        .start
        .cmp(&right.start)
        .then_with(|| left.end.cmp(&right.end))
    });
    let count = ranges.len();

    // Invariant 5/6: every replacement goes through the write authority.
    // Same-paragraph matches ride ONE compound `ReplaceMatches` intent (one
    // gate hold, one commit, one undo member regardless of match count — the
    // §11 anti-storm law); cross-paragraph matches (a match spanning a
    // boundary) take the selection + insert-text path, which owns the
    // structural join. The old direct-projection mutation silently never
    // reached canonical state — peers, saves, and undo all lost it.
    let mut matches = Vec::with_capacity(ranges.len());
    let mut cross_paragraph_ranges = Vec::new();
    for range in ranges {
      if range.start.paragraph == range.end.paragraph {
        let (Some(start), Some(end)) = (self.text_anchor_at(range.start), self.text_anchor_at(range.end)) else {
          continue;
        };
        let styles = self
          .document
          .paragraphs
          .get(range.start.paragraph)
          .map(|paragraph| styles_at_byte(paragraph, range.start.byte));
        matches.push(ReplaceMatch { start, end, styles });
      } else {
        cross_paragraph_ranges.push(range);
      }
    }

    let legs = cross_paragraph_ranges.len() + usize::from(!matches.is_empty());
    let grouped = legs > 1;
    if grouped {
      self.begin_undo_group();
    }
    for range in cross_paragraph_ranges.into_iter().rev() {
      self.selection = EditorSelection::range(range.start, range.end);
      if replacement.is_empty() {
        self.write_delete_selection(cx);
      } else {
        self.insert_text(replacement, cx);
      }
    }
    if !matches.is_empty() {
      self.write_replace_matches(matches, replacement, cx);
    }
    if grouped {
      self.end_undo_group();
    }
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

fn styles_at_byte(paragraph: &Paragraph, byte: usize) -> RunStyles {
  let (run_ix, _) = run_containing(paragraph, byte);
  paragraph
    .runs
    .get(run_ix)
    .map_or_else(RunStyles::default, |run| run.styles)
}

