#[hotpath::measure_all]
impl RichTextEditor {
  pub fn toggle_underline(&mut self, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Underline, cx) {
      return;
    }
    self.toggle_underline_kind(None, cx);
  }

  pub fn toggle_strikethrough(&mut self, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Strikethrough, cx) {
      return;
    }
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        self.armed_inline_tool = Some(ArmedInlineTool::Strikethrough);
        cx.notify();
        return;
      };
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| table_cell_range_all_run_styles(paragraph, selection_range.clone(), |styles| styles.strikethrough))
        .unwrap_or(false);
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.paragraph.runs.is_empty() && !paragraph.text.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range, |styles| styles.strikethrough = !all_selected);
      });
      return;
    }
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.strikethrough = !styles.strikethrough;
      self.pending_styles = Some(styles);
      cx.notify();
      return;
    }
    let range = self.selection.normalized();
    let all_selected = selection_all_run_styles(&self.document, range.clone(), |styles| styles.strikethrough);
    let mut styles = run_styles_at_offset(&self.document, range.start);
    styles.strikethrough = !all_selected;
    self.pending_styles = None;
    self.write_set_marks(range, styles, cx);
  }

  /// Toggle any semantic inline style for the current selection or caret.
  ///
  pub fn toggle_semantic_style_for_selection(&mut self, semantic: RunSemanticStyle, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Semantic(semantic), cx) {
      return;
    }
    self.toggle_semantic_style(semantic, cx);
  }

  pub fn set_highlight(&mut self, highlight: HighlightStyle, cx: &mut Context<Self>) {
    self.current_highlight_style = highlight;
    self.current_highlight_choice = Some(highlight);
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Highlight(highlight), cx) {
      return;
    }
    self.set_highlight_internal(Some(highlight), cx);
  }

  /// Set or clear the highlight style for the current selection or caret.
  ///
  /// `None` clears highlights. `Some(...)` applies the requested highlight, or
  /// toggles it off when the whole selection already has that highlight.
  pub fn set_highlight_for_selection(&mut self, highlight: Option<HighlightStyle>, cx: &mut Context<Self>) {
    self.set_highlight_internal(highlight, cx);
  }

  pub fn speech_send_fragment_at_selection_or_hover(
    &mut self,
    section_slots: &[u8],
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<RichClipboardFragment> {
    if !self.selection.is_caret() {
      return Some(selected_rich_fragment(&self.document, self.selection.normalized()));
    }
    let position = self.last_drag_position?;
    let paragraph_ix = self
      .hit_test_document_position(position, window, cx)
      .paragraph;
    let (start_paragraph, end_paragraph_exclusive) = hierarchical_section_bounds(&self.document, paragraph_ix, section_slots)
      .or_else(|| enclosing_section_bounds(&self.document, paragraph_ix, section_slots))
      .unwrap_or((
        paragraph_ix,
        paragraph_ix
          .saturating_add(1)
          .min(self.document.paragraphs.len()),
      ));
    let end_paragraph = end_paragraph_exclusive.saturating_sub(1);
    Some(selected_rich_fragment(
      &self.document,
      DocumentOffset {
        paragraph: start_paragraph,
        byte: 0,
      }..DocumentOffset {
        paragraph: end_paragraph,
        byte: paragraph_text_len(&self.document.paragraphs[end_paragraph]),
      },
    ))
  }

  pub fn fragment_at_selection_or_enclosing_section(&self, section_slots: &[u8]) -> Option<RichClipboardFragment> {
    let range = self.selection_or_enclosing_section_range(section_slots)?;
    Some(selected_rich_fragment(&self.document, range))
  }

  pub fn replace_selection_or_enclosing_section_with_paragraphs(
    &mut self,
    paragraphs: Vec<InputParagraph>,
    section_slots: &[u8],
    cx: &mut Context<Self>,
  ) {
    if paragraphs.is_empty() {
      return;
    }
    let Some(range) = self.selection_or_enclosing_section_range(section_slots) else {
      return;
    };
    self.selection = EditorSelection::range(range.start, range.end);
    let blocks = paragraphs.into_iter().map(FragmentBlock::Paragraph).collect();
    self.write_insert_rich_fragment_at_caret(blocks, cx);
  }

  fn selection_or_enclosing_section_range(&self, section_slots: &[u8]) -> Option<Range<DocumentOffset>> {
    if !self.selection.is_caret() {
      return Some(self.selection.normalized());
    }
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return None;
    }
    let caret = self.selection.head;
    let paragraph_ix = caret.paragraph.min(paragraph_count - 1);
    let (start_paragraph, end_paragraph_exclusive) = hierarchical_section_bounds(&self.document, paragraph_ix, section_slots)
      .or_else(|| enclosing_section_bounds(&self.document, paragraph_ix, section_slots))
      .unwrap_or((paragraph_ix, paragraph_ix.saturating_add(1).min(paragraph_count)));
    let end_paragraph = end_paragraph_exclusive
      .saturating_sub(1)
      .min(paragraph_count - 1);
    if start_paragraph > end_paragraph {
      return None;
    }
    Some(
      DocumentOffset {
        paragraph: start_paragraph,
        byte: 0,
      }..DocumentOffset {
        paragraph: end_paragraph,
        byte: paragraph_text_len(&self.document.paragraphs[end_paragraph]),
      },
    )
  }

  pub fn toggle_enclosing_section_collapsed(&mut self, section_slots: &[u8], cx: &mut Context<Self>) {
    let caret = self.selection.head;
    self.toggle_section_collapsed_at_paragraph(caret.paragraph, section_slots, cx);
  }

  pub(super) fn section_collapse_state_at_paragraph(&self, paragraph_ix: usize, section_slots: &[u8]) -> Option<bool> {
    (self.hovered_collapse_paragraph == Some(paragraph_ix)).then(|| self.section_collapsed_at_heading(paragraph_ix, section_slots))?
  }

  pub(super) fn section_collapsed_at_heading(&self, paragraph_ix: usize, section_slots: &[u8]) -> Option<bool> {
    let heading_kind = collapse_heading_kind(&self.document, paragraph_ix, section_slots)?;
    let Some(section) = section_at_heading(&self.document, paragraph_ix, heading_kind) else {
      return Some(false);
    };
    Some(self.collapsed_section_ids.contains(&section.id))
  }

  pub(super) fn toggle_section_collapsed_at_paragraph(&mut self, paragraph_ix: usize, section_slots: &[u8], cx: &mut Context<Self>) {
    let Some(heading_kind) = collapse_heading_kind(&self.document, paragraph_ix, section_slots) else {
      return;
    };
    let section_id = section_at_heading(&self.document, paragraph_ix, heading_kind)
      .map(|section| section.id)
      .or_else(|| {
        rebuild_document_sections(&mut self.document);
        section_at_heading(&self.document, paragraph_ix, heading_kind).map(|section| section.id)
      });
    let Some(section_id) = section_id else {
      return;
    };
    if !self.collapsed_section_ids.insert(section_id) {
      self.collapsed_section_ids.remove(&section_id);
    }
    self.item_sizes_cache = None;
    self.height_prefix_index = HeightPrefixIndex::default();
    self.pending_item_sizes_patch_range = None;
    cx.notify();
  }

  pub fn set_highlight_from_caret_to_enclosing_section_end(&mut self, highlight: HighlightStyle, section_slots: &[u8], cx: &mut Context<Self>) {
    let caret = self.selection.head;
    let Some((start_paragraph, end_paragraph_exclusive)) = hierarchical_section_bounds(&self.document, caret.paragraph, section_slots)
      .or_else(|| enclosing_section_bounds(&self.document, caret.paragraph, section_slots))
    else {
      return;
    };
    let start = DocumentOffset {
      paragraph: caret.paragraph.max(start_paragraph),
      byte: caret.byte,
    };
    let end_paragraph = end_paragraph_exclusive.saturating_sub(1);
    let end = DocumentOffset {
      paragraph: end_paragraph,
      byte: paragraph_text_len(&self.document.paragraphs[end_paragraph]),
    };
    self.set_highlight_for_document_offsets(start, end, highlight, cx);
  }

  pub fn set_highlight_for_document_offsets(
    &mut self,
    start: DocumentOffset,
    end: DocumentOffset,
    highlight: HighlightStyle,
    cx: &mut Context<Self>,
  ) {
    let range_start = start.min(end);
    let range_end = start.max(end);
    if range_start == range_end
      || range_start.paragraph >= self.document.paragraphs.len()
      || range_end.paragraph >= self.document.paragraphs.len()
    {
      return;
    }
    // Only re-colors runs that already carry a highlight; each maximal
    // already-highlighted span becomes one SetMarks intent, grouped as one
    // undo unit.
    let spans = highlighted_spans_in_range(&self.document, range_start..range_end, highlight);
    if spans.is_empty() {
      return;
    }
    self.pending_styles = None;
    self.begin_undo_group();
    for (range, styles) in spans {
      self.write_set_marks(range, styles, cx);
    }
    self.end_undo_group();
  }

  pub fn clear_highlight(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_internal(None, cx);
  }

  pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        paragraph.paragraph.style = ParagraphStyle::Normal;
        for run in &mut paragraph.paragraph.runs {
          run.styles = RunStyles::default();
        }
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    self.pending_styles = None;
    if self.selection.is_caret() {
      let paragraph_ix = self.selection.head.paragraph;
      self.write_clear_whole_paragraph_formatting(paragraph_ix..paragraph_ix + 1, cx);
    } else {
      let range = self.selection.normalized();
      if selection_contains_whole_paragraph(&self.document, range.clone()) {
        self.write_clear_whole_paragraph_formatting(range.start.paragraph..range.end.paragraph + 1, cx);
      } else {
        self.write_set_marks(range, RunStyles::default(), cx);
      }
    }
  }

  /// Clear-formatting over whole paragraphs: paragraph styles back to Normal
  /// (one intent per changed paragraph) plus one run-style reset over the full
  /// span, grouped as one undo unit.
  fn write_clear_whole_paragraph_formatting(&mut self, paragraphs: Range<usize>, cx: &mut Context<Self>) {
    let end = paragraphs.end.min(self.document.paragraphs.len());
    if paragraphs.start >= end {
      return;
    }
    self.begin_undo_group();
    for paragraph_ix in paragraphs.start..end {
      if self
        .document
        .paragraphs
        .get(paragraph_ix)
        .is_some_and(|paragraph| paragraph.style != ParagraphStyle::Normal)
      {
        self.write_set_paragraph_style(paragraph_ix, ParagraphStyle::Normal, cx);
      }
    }
    let last_paragraph = end - 1;
    let range = DocumentOffset {
      paragraph: paragraphs.start,
      byte: 0,
    }..DocumentOffset {
      paragraph: last_paragraph,
      byte: self.document.paragraphs.get(last_paragraph).map(paragraph_text_len).unwrap_or(0),
    };
    if range.start != range.end {
      self.write_set_marks(range, RunStyles::default(), cx);
    }
    self.end_undo_group();
  }

  pub fn apply_run_style_to_selection(&mut self, style: RunStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| styles.apply(style));
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      return;
    }
    let range = self.selection.normalized();
    let styles = run_styles_at_offset(&self.document, range.start).with(style);
    self.pending_styles = None;
    self.write_set_marks(range, styles, cx);
  }

  pub fn set_paragraph_style_for_selection(&mut self, style: ParagraphStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.paragraph.style != style {
          paragraph.paragraph.style = style;
          paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
        }
      });
      return;
    }
    let range = self.selection.normalized();
    self.begin_undo_group();
    for paragraph_ix in range.start.paragraph..=range.end.paragraph {
      if self
        .document
        .paragraphs
        .get(paragraph_ix)
        .is_some_and(|paragraph| paragraph.style != style)
      {
        self.write_set_paragraph_style(paragraph_ix, style, cx);
      }
    }
    self.end_undo_group();
  }

  // -------- Action handlers (bound to keystrokes in main.rs) -----------
  // Each handler delegates to a movement/edit primitive defined below.
  // The signatures all match what `cx.listener(...)` expects:
  //   fn(&mut Self, &Action, &mut Window, &mut Context<Self>).
}

/// Pure projection scan: the maximal spans inside `range` whose runs already
/// carry a highlight, with each span's styles recolored to `highlight`.
/// Adjacent run spans that converge on the same styles are merged so the
/// resulting `SetMarks` intents stay minimal.
fn highlighted_spans_in_range(
  document: &DocumentProjection,
  range: Range<DocumentOffset>,
  highlight: HighlightStyle,
) -> Vec<(Range<DocumentOffset>, RunStyles)> {
  let mut spans: Vec<(Range<DocumentOffset>, RunStyles)> = Vec::new();
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    let Some(paragraph) = document.paragraphs.get(paragraph_ix) else {
      continue;
    };
    let paragraph_start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let paragraph_end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    let mut offset = 0;
    for run in &paragraph.runs {
      let run_start = offset;
      let run_end = offset + run.len;
      offset = run_end;
      if run_end <= paragraph_start || run_start >= paragraph_end || run.styles.highlight.is_none() {
        continue;
      }
      let mut styles = run.styles;
      styles.highlight = Some(highlight);
      let start = DocumentOffset {
        paragraph: paragraph_ix,
        byte: run_start.max(paragraph_start),
      };
      let end = DocumentOffset {
        paragraph: paragraph_ix,
        byte: run_end.min(paragraph_end),
      };
      if let Some((last_range, last_styles)) = spans.last_mut()
        && last_range.end == start
        && *last_styles == styles
      {
        last_range.end = end;
      } else {
        spans.push((start..end, styles));
      }
    }
  }
  spans
}

fn hierarchical_section_bounds(document: &DocumentProjection, paragraph_ix: usize, section_slots: &[u8]) -> Option<(usize, usize)> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  let ParagraphStyle::Custom(slot) = paragraph.style else {
    return None;
  };
  if !section_slots.contains(&(slot & 0x7f)) {
    return None;
  }
  let level = document
    .theme
    .custom_paragraph_styles
    .get(&(slot & 0x7f))
    .and_then(|style| style.section_level)?;
  let mut end = document.paragraphs.len();
  for next_ix in paragraph_ix.saturating_add(1)..document.paragraphs.len() {
    let ParagraphStyle::Custom(next_slot) = document.paragraphs[next_ix].style else {
      continue;
    };
    if document
      .theme
      .custom_paragraph_styles
      .get(&(next_slot & 0x7f))
      .and_then(|style| style.section_level)
      .is_some_and(|next_level| next_level <= level)
    {
      end = next_ix;
      break;
    }
  }
  Some((paragraph_ix, end))
}

fn collapse_heading_kind(document: &DocumentProjection, paragraph_ix: usize, section_slots: &[u8]) -> Option<u8> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  let ParagraphStyle::Custom(slot) = paragraph.style else {
    return None;
  };
  let slot = slot & 0x7f;
  if !section_slots.contains(&slot) {
    return None;
  }
  let style = document.theme.custom_paragraph_styles.get(&slot)?;
  style.section_level?;
  Some(style.section_kind.unwrap_or(slot))
}

fn section_at_heading(document: &DocumentProjection, paragraph_ix: usize, heading_kind: u8) -> Option<&DocumentOutlineNode> {
  document.outline.iter().find(|section| {
    let SectionKind::Custom(section_kind) = section.kind;
    section_kind == heading_kind && paragraph_index_for_id(document, section.start_paragraph) == Some(paragraph_ix)
  })
}

fn enclosing_section<'a>(document: &'a DocumentProjection, paragraph_ix: usize, section_slots: &[u8]) -> Option<&'a DocumentOutlineNode> {
  document
    .outline
    .iter()
    .filter(|section| {
      let SectionKind::Custom(slot) = section.kind;
      if !section_slots.contains(&slot) {
        return false;
      }
      let Some(start) = paragraph_index_for_id(document, section.start_paragraph) else {
        return false;
      };
      let end = section
        .end_paragraph_exclusive
        .and_then(|id| paragraph_index_for_id(document, id))
        .unwrap_or(document.paragraphs.len());
      start <= paragraph_ix && paragraph_ix < end
    })
    .min_by_key(|section| {
      let start = paragraph_index_for_id(document, section.start_paragraph).unwrap_or(0);
      let end = section
        .end_paragraph_exclusive
        .and_then(|id| paragraph_index_for_id(document, id))
        .unwrap_or(document.paragraphs.len());
      end - start
    })
}

fn enclosing_section_bounds(document: &DocumentProjection, paragraph_ix: usize, section_slots: &[u8]) -> Option<(usize, usize)> {
  let section = enclosing_section(document, paragraph_ix, section_slots)?;
  let start = paragraph_index_for_id(document, section.start_paragraph)?;
  let end = section
    .end_paragraph_exclusive
    .and_then(|id| paragraph_index_for_id(document, id))
    .unwrap_or(document.paragraphs.len());
  Some((start, end))
}
