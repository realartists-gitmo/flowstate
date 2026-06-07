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
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
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
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| styles.strikethrough = !all_selected);
      editor.after_formatting_mutation(cx);
    });
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

  pub fn toggle_enclosing_section_collapsed(&mut self, section_slots: &[u8], cx: &mut Context<Self>) {
    let caret = self.selection.head;
    self.toggle_section_collapsed_at_paragraph(caret.paragraph, section_slots, cx);
  }

  pub(super) fn section_collapse_state_at_paragraph(&self, paragraph_ix: usize, section_slots: &[u8]) -> Option<bool> {
    (self.hovered_collapse_paragraph == Some(paragraph_ix)).then(|| self.section_collapsed_at_heading(paragraph_ix, section_slots))?
  }

  pub(super) fn section_collapsed_at_heading(&self, paragraph_ix: usize, section_slots: &[u8]) -> Option<bool> {
    if let Some(section) = enclosing_section(&self.document, paragraph_ix, section_slots) {
      let start = paragraph_index_for_id(&self.document, section.start_paragraph)?;
      return (start == paragraph_ix).then(|| self.collapsed_section_ids.contains(&section.id));
    }
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let ParagraphStyle::Custom(slot) = paragraph.style else {
      return None;
    };
    section_slots.contains(&(slot & 0x7f)).then(|| {
      self
        .collapsed_section_ids
        .contains(&SectionId(paragraph_ix as u128))
    })
  }

  pub(super) fn toggle_section_collapsed_at_paragraph(&mut self, paragraph_ix: usize, section_slots: &[u8], cx: &mut Context<Self>) {
    let section_id = if let Some(section) = enclosing_section(&self.document, paragraph_ix, section_slots) {
      section.id
    } else {
      let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
        return;
      };
      let ParagraphStyle::Custom(slot) = paragraph.style else {
        return;
      };
      if !section_slots.contains(&(slot & 0x7f)) {
        return;
      }
      SectionId(paragraph_ix as u128)
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
    self.apply_document_edit(cx, |editor, cx| {
      for paragraph_ix in range_start.paragraph..=range_end.paragraph {
        let paragraph_start = if paragraph_ix == range_start.paragraph { range_start.byte } else { 0 };
        let paragraph_end = if paragraph_ix == range_end.paragraph {
          range_end.byte
        } else {
          paragraph_text_len(&editor.document.paragraphs[paragraph_ix])
        };
        if paragraph_start < paragraph_end {
          apply_highlight_to_existing_highlights_in_paragraph_range(
            &mut editor.document,
            paragraph_ix,
            paragraph_start..paragraph_end,
            highlight,
          );
        }
      }
      editor.after_formatting_mutation(cx);
    });
  }

  pub fn clear_highlight(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_internal(None, cx);
  }

  pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
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
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let paragraph_ix = editor.selection.head.paragraph;
        clear_whole_paragraph_formatting(&mut editor.document, paragraph_ix);
      } else {
        let range = editor.selection.normalized();
        if selection_contains_whole_paragraph(&editor.document, range.clone()) {
          for paragraph_ix in range.start.paragraph..=range.end.paragraph {
            clear_whole_paragraph_formatting(&mut editor.document, paragraph_ix);
          }
        } else {
          mutate_runs_in_range(&mut editor.document, range, |styles| *styles = RunStyles::default());
        }
      }
      rebuild_document_sections(&mut editor.document);
      editor.pending_styles = None;
      editor.after_formatting_mutation(cx);
    });
  }

  pub fn apply_run_style_to_selection(&mut self, style: RunStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
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
    self.apply_document_edit(cx, |editor, cx| {
      let range = editor.selection.normalized();
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
        let end = if paragraph_ix == range.end.paragraph {
          range.end.byte
        } else {
          paragraph_text_len(&editor.document.paragraphs[paragraph_ix])
        };
        apply_style_to_paragraph_range(&mut editor.document, paragraph_ix, start..end, style);
      }
      editor.after_formatting_mutation(cx);
    });
  }

  pub fn set_paragraph_style_for_selection(&mut self, style: ParagraphStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.paragraph.style != style {
          paragraph.paragraph.style = style;
          paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
        }
      });
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      let range = editor.selection.normalized();
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        if let Some(paragraph) = paragraphs_mut(&mut editor.document).get_mut(paragraph_ix)
          && paragraph.style != style
        {
          paragraph.style = style;
          bump_paragraph_version(paragraph);
        }
      }
      rebuild_document_sections(&mut editor.document);
      editor.after_formatting_mutation(cx);
    });
  }

  // -------- Action handlers (bound to keystrokes in main.rs) -----------
  // Each handler delegates to a movement/edit primitive defined below.
  // The signatures all match what `cx.listener(...)` expects:
  //   fn(&mut Self, &Action, &mut Window, &mut Context<Self>).
}

fn apply_highlight_to_existing_highlights_in_paragraph_range(
  document: &mut Document,
  paragraph_ix: usize,
  range: Range<usize>,
  highlight: HighlightStyle,
) {
  mutate_runs_in_range(
    document,
    DocumentOffset {
      paragraph: paragraph_ix,
      byte: range.start,
    }..DocumentOffset {
      paragraph: paragraph_ix,
      byte: range.end,
    },
    |styles| {
      styles.highlight = Some(highlight);
    },
  );
}

fn hierarchical_section_bounds(document: &Document, paragraph_ix: usize, section_slots: &[u8]) -> Option<(usize, usize)> {
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

fn enclosing_section<'a>(document: &'a Document, paragraph_ix: usize, section_slots: &[u8]) -> Option<&'a DocumentSection> {
  document
    .sections
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

fn enclosing_section_bounds(document: &Document, paragraph_ix: usize, section_slots: &[u8]) -> Option<(usize, usize)> {
  let section = enclosing_section(document, paragraph_ix, section_slots)?;
  let start = paragraph_index_for_id(document, section.start_paragraph)?;
  let end = section
    .end_paragraph_exclusive
    .and_then(|id| paragraph_index_for_id(document, id))
    .unwrap_or(document.paragraphs.len());
  Some((start, end))
}
