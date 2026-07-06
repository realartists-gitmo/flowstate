#[hotpath::measure_all]
impl RichTextEditor {
  fn hit_test_document_position(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> DocumentOffset {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return DocumentOffset::default();
    }
    let viewport = self.scroll_handle.bounds();
    let width = if viewport.size.width > px(1.0) { viewport.size.width } else { px(900.0) };
    self.ensure_exact_interaction_chunks(width, window, cx);
    let _ = self.paragraph_item_sizes(window, cx);
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    let (paragraph_ix, chunk_ix) = match self.virtual_text_item_at_content_y(content_y, width, window, cx) {
      Some((paragraph_ix, chunk_ix)) => (paragraph_ix, chunk_ix),
      None => {
        let fallback = self.selection.head.paragraph.min(paragraph_count - 1);
        (
          fallback,
          self.ensure_paragraph_chunk_containing_byte(fallback, self.selection.head.byte, width, window, cx),
        )
      },
    };
    if let Some(chunk_ix) = chunk_ix
      && let Some(layout) = self.paragraph_chunk_layout_state(paragraph_ix, chunk_ix, width)
    {
      let row_top = self
        .item_top_for_paragraph_chunk(paragraph_ix, chunk_ix)
        .unwrap_or(px(0.0));
      let bounds = Bounds::new(
        point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + row_top),
        size(width, layout.size.height),
      );
      return layout.hit_test_at_bounds(position, bounds);
    }
    self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx);
    let Some(layout) = self.paragraph_chunk_layout_state(paragraph_ix, 0, width) else {
      return DocumentOffset {
        paragraph: paragraph_ix,
        byte: 0,
      };
    };
    let row_top = self
      .item_top_for_paragraph_chunk(paragraph_ix, 0)
      .unwrap_or(px(0.0));
    let bounds = Bounds::new(
      point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + row_top),
      size(width, layout.size.height),
    );
    layout.hit_test_at_bounds(position, bounds)
  }

  fn virtual_text_item_at_content_y(
    &mut self,
    content_y: Pixels,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<(usize, Option<usize>)> {
    for _ in 0..2 {
      let cache = self.item_sizes_cache.as_ref()?;
      if self.height_prefix_index.len() != cache.item_count {
        return None;
      }
      let item_ix = self.height_prefix_index.lower_bound(content_y);
      match cache.items.get(item_ix).cloned() {
        Some(VirtualItem::ParagraphChunk { paragraph_ix, chunk_ix, .. }) => return Some((paragraph_ix, Some(chunk_ix))),
        Some(VirtualItem::ParagraphRemainder { paragraph_ix, .. }) => {
          self.ensure_next_paragraph_chunk(paragraph_ix, width, window, cx);
          let _ = self.paragraph_item_sizes(window, cx);
          continue;
        },
        Some(VirtualItem::HiddenBlock { block_ix } | VirtualItem::StructuralBlock { block_ix }) => {
          return self
            .paragraph_ix_for_block(block_ix)
            .map(|paragraph_ix| (paragraph_ix, None));
        },
        None => return None,
      }
    }
    None
  }

  // Home / End: jump to the start or end of the current visual (wrapped) line.
  // We resolve which `LaidOutLine` the caret sits on, then snap to its byte
  // range endpoints. This is why Home/End work correctly across soft wraps
  // without any renderer changes.
  fn move_line_edge(&mut self, start: bool, extend: bool, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let new_byte = {
      let Some(layout) = self.layout_for_offset(head) else {
        return;
      };
      // Resolve the caret's current visual line using its stored gravity so the
      // line edges we snap to match where the caret is actually painted.
      let Some((p_ix, l_ix)) = locate_line(&layout, head, self.selection.head_gravity) else {
        return;
      };
      let line = &layout.paragraphs[p_ix].lines[l_ix];
      if start { line.start_byte } else { line.end_byte }
    };
    let new = DocumentOffset {
      paragraph: head.paragraph,
      byte: new_byte,
    };
    // §16: line-start gravitates downstream (start of the lower visual line),
    // line-end gravitates upstream (end of the upper visual line). This is the
    // canonical wrap-seam case — without Upstream gravity, "End" on a wrapped
    // line would otherwise paint at the start of the next visual line.
    let (affinity, gravity) = if start {
      (SelectionAffinity::Before, VisualGravity::Downstream)
    } else {
      (SelectionAffinity::After, VisualGravity::Upstream)
    };
    let selection = self.selection.moved(new, affinity, gravity, extend);
    if self.selection.same_positions(&selection) {
      self.goal_x = None;
      return;
    }
    self.note_explicit_selection_movement();
    let fid_before = self.fidelity_caret_before();
    self.selection = selection;
    self.fidelity_caret_set("move_to_line_edge", &fid_before);
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    self.emit_selection_changed(cx);
    cx.notify();
  }

  // -------- Edit primitives --------------------------------------------

  fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
    if text.is_empty() {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
    }
    let caret = self.selection.head;
    if self.invisibility_mode
      && self
        .document
        .paragraphs
        .get(caret.paragraph)
        .is_some_and(|paragraph| matches!(paragraph.style, ParagraphStyle::Normal))
    {
      if let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(caret.paragraph) {
        paragraph.style = ParagraphStyle::Custom(4);
        bump_paragraph_version(paragraph);
      }
      update_paragraph_block(&mut self.document, caret.paragraph);
    }
    // Inherit styles from the run that contains the caret. With left-bias at
    // run boundaries this matches Word's "type continues the previous run's
    // styling" behavior.
    let styles = if let Some(styles) = self.pending_styles {
      styles
    } else {
      let paragraph = &self.document.paragraphs[caret.paragraph];
      let (run_ix, _) = run_containing(paragraph, caret.byte);
      paragraph
        .runs
        .get(run_ix)
        .map(|r| r.styles)
        .unwrap_or_default()
    };
    insert_text_at(&mut self.document, caret.paragraph, caret.byte, text, styles);
    let new = DocumentOffset {
      paragraph: caret.paragraph,
      byte: caret.byte + text.len(),
    };
    let fid_before = self.fidelity_caret_before();
    self.selection = EditorSelection::collapsed(new);
    self.fidelity_caret_set("insert_text", &fid_before);
    self.after_text_mutation(cx);
  }

  // Helper for shared selection-deletion logic. Does NOT call `cx.notify()`.
  fn delete_selection_internal(&mut self) -> bool {
    if self.selection.is_caret() {
      return false;
    }
    let range = self.selection.normalized();
    if range.start.paragraph == range.end.paragraph {
      delete_range_in_paragraph(&mut self.document, range.start.paragraph, range.start.byte..range.end.byte);
    } else {
      // Cross-paragraph selection: delete the tail of the start paragraph,
      // the head of the end paragraph, then merge the end paragraph's
      // remaining runs onto the end of the start paragraph. Intermediate
      // paragraphs are dropped wholesale.
      delete_cross_paragraph_range(&mut self.document, range.clone());
    }
    let fid_before = self.fidelity_caret_before();
    self.selection = EditorSelection::collapsed(range.start);
    self.fidelity_caret_set("delete_selection_internal", &fid_before);
    true
  }

  fn backspace(&mut self, cx: &mut Context<Self>) {
    if self.backspace_selected_table_cell(cx) {
      return;
    }
    if self.backspace_selected_equation(cx) {
      return;
    }
    if self.delete_selected_block(cx) {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
      self.after_text_mutation(cx);
      return;
    }
    let caret = self.selection.head;
    if caret.byte == 0 {
      if let Some(object) = self.immediate_object_before_paragraph(caret.paragraph) {
        self.select_block(object, cx);
        return;
      }
      // Joining backwards: merge this paragraph onto the previous one. The
      // caret lands at the join seam.
      if caret.paragraph == 0 {
        return;
      }
      let prev_ix = caret.paragraph - 1;
      let prev_len = paragraph_text_len(&self.document.paragraphs[prev_ix]);
      delete_cross_paragraph_range(
        &mut self.document,
        DocumentOffset {
          paragraph: prev_ix,
          byte: prev_len,
        }..caret,
      );
      let new = DocumentOffset {
        paragraph: prev_ix,
        byte: prev_len,
      };
      let fid_before = self.fidelity_caret_before();
      self.selection = EditorSelection::collapsed(new);
      self.fidelity_caret_set("backspace/join-previous", &fid_before);
    } else {
      let prev = prev_grapheme_boundary_in_paragraph(&self.document, caret.paragraph, caret.byte);
      delete_range_in_paragraph(&mut self.document, caret.paragraph, prev..caret.byte);
      let new = DocumentOffset {
        paragraph: caret.paragraph,
        byte: prev,
      };
      let fid_before = self.fidelity_caret_before();
      self.selection = EditorSelection::collapsed(new);
      self.fidelity_caret_set("backspace/within-paragraph", &fid_before);
    }
    self.after_text_mutation(cx);
  }

  fn delete_forward(&mut self, cx: &mut Context<Self>) {
    if self.delete_forward_selected_table_cell(cx) {
      return;
    }
    if self.delete_selected_block(cx) {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
      self.after_text_mutation(cx);
      return;
    }
    let caret = self.selection.head;
    let para_len = paragraph_text_len(&self.document.paragraphs[caret.paragraph]);
    if caret.byte == para_len {
      if let Some(object) = self.immediate_object_after_paragraph(caret.paragraph) {
        self.select_block(object, cx);
        return;
      }
      // Joining forwards: pull the next paragraph's runs onto this one.
      if caret.paragraph + 1 >= self.document.paragraphs.len() {
        return;
      }
      delete_cross_paragraph_range(
        &mut self.document,
        caret..DocumentOffset {
          paragraph: caret.paragraph + 1,
          byte: 0,
        },
      );
    } else {
      let next = next_grapheme_boundary_in_paragraph(&self.document, caret.paragraph, caret.byte);
      delete_range_in_paragraph(&mut self.document, caret.paragraph, caret.byte..next);
    }
    self.after_text_mutation(cx);
  }

  fn insert_paragraph_break(&mut self, cx: &mut Context<Self>) {
    if !self.selection.is_caret() {
      self.delete_selection_internal();
    }
    let caret = self.selection.head;
    let starts_empty_paragraph = caret.byte == paragraph_text_len(&self.document.paragraphs[caret.paragraph]);
    split_paragraph_at(&mut self.document, caret.paragraph, caret.byte);
    let new = DocumentOffset {
      paragraph: caret.paragraph + 1,
      byte: 0,
    };
    if starts_empty_paragraph {
      // Pressing Enter at the end starts a genuinely fresh paragraph. Reset it
      // so header/inline/highlight styling does not leak into the next line.
      clear_whole_paragraph_formatting(&mut self.document, new.paragraph);
      rebuild_document_sections(&mut self.document);
    }
    let fid_before = self.fidelity_caret_before();
    self.selection = EditorSelection::collapsed(new);
    self.fidelity_caret_set("insert_paragraph_break", &fid_before);
    self.after_text_mutation(cx);
  }
}
