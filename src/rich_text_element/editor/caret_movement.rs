impl RichTextEditor {
  fn move_horizontal(&mut self, dir: HDir, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
    if matches!(self.selected_block, Some(BlockSelection::Equation(_))) {
      let source = self.selected_equation_source().unwrap_or_default();
      let caret = self.equation_source_caret.min(source.len());
      let next = match dir {
        HDir::Left if caret > 0 => source[..caret]
          .char_indices()
          .next_back()
          .map(|(byte, _)| byte)
          .unwrap_or(0),
        HDir::Left => 0,
        HDir::Right if caret < source.len() => source[caret..]
          .char_indices()
          .nth(1)
          .map(|(byte, _)| caret + byte)
          .unwrap_or(source.len()),
        HDir::Right => source.len(),
      };
      if extend {
        self.equation_source_caret = next;
      } else {
        self.equation_source_caret = next;
        self.equation_source_anchor = next;
      }
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    if !extend && matches!(self.selected_block, Some(BlockSelection::TableCell { .. })) {
      let text = self.selected_table_cell_text().unwrap_or_default();
      match dir {
        HDir::Left if self.table_cell_caret > 0 => {
          self.table_cell_caret = text[..self.table_cell_caret.min(text.len())]
            .char_indices()
            .next_back()
            .map(|(byte, _)| byte)
            .unwrap_or(0);
          cx.notify();
          return;
        },
        HDir::Left => {
          if let Some((paragraph_ix, len)) = self.adjacent_selected_table_cell_paragraph(false) {
            self.table_cell_block_ix = paragraph_ix;
            self.table_cell_caret = len;
            cx.notify();
            return;
          }
        },
        HDir::Right if self.table_cell_caret < text.len() => {
          let caret = self.table_cell_caret.min(text.len());
          self.table_cell_caret = text[caret..]
            .char_indices()
            .nth(1)
            .map(|(byte, _)| caret + byte)
            .unwrap_or(text.len());
          cx.notify();
          return;
        },
        HDir::Right => {
          if let Some((paragraph_ix, _)) = self.adjacent_selected_table_cell_paragraph(true) {
            self.table_cell_block_ix = paragraph_ix;
            self.table_cell_caret = 0;
            cx.notify();
            return;
          }
        },
      }
    }
    if !extend && self.selected_block.is_some() && self.collapse_object_selection(dir, cx) {
      return;
    }
    if !extend && self.selection.is_caret() {
      let head = self.selection.head;
      let object = match dir {
        HDir::Left if head.byte == 0 => self.immediate_object_before_paragraph(head.paragraph),
        HDir::Right if head.byte == paragraph_text_len(&self.document.paragraphs[head.paragraph]) => {
          self.immediate_object_after_paragraph(head.paragraph)
        },
        _ => None,
      };
      if let Some(object) = object {
        self.select_block(object, cx);
        return;
      }
    }
    let new_head = match dir {
      HDir::Left => {
        // Collapsing a selection leftwards jumps to its start without moving.
        if !extend && !self.selection.is_caret() {
          self.selection.normalized().start
        } else {
          self.step_left(self.selection.head)
        }
      },
      HDir::Right => {
        if !extend && !self.selection.is_caret() {
          self.selection.normalized().end
        } else {
          self.step_right(self.selection.head)
        }
      },
    };
    let anchor = if extend { self.selection.anchor } else { new_head };
    let selection = EditorSelection { anchor, head: new_head };
    if self.selection == selection {
      self.goal_x = None;
      return;
    }
    self.selection = selection;
    self.goal_x = None;
    let width = self.current_layout_width();
    let _ = self.ensure_paragraph_chunk_containing_byte(new_head.paragraph, new_head.byte, width, window, cx);
    let _ = self.paragraph_item_sizes(window, cx);
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn step_left(&self, off: DocumentOffset) -> DocumentOffset {
    if off.byte == 0 {
      // At the start of a paragraph: hop to end of previous paragraph (or
      // stay if we're at the document start).
      if off.paragraph == 0 {
        return off;
      }
      let prev = off.paragraph - 1;
      let byte = paragraph_text_len(&self.document.paragraphs[prev]);
      return DocumentOffset { paragraph: prev, byte };
    }
    DocumentOffset {
      paragraph: off.paragraph,
      byte: prev_grapheme_boundary_in_paragraph(&self.document, off.paragraph, off.byte),
    }
  }

  fn step_right(&self, off: DocumentOffset) -> DocumentOffset {
    let len = paragraph_text_len(&self.document.paragraphs[off.paragraph]);
    if off.byte >= len {
      if off.paragraph + 1 >= self.document.paragraphs.len() {
        return off;
      }
      return DocumentOffset {
        paragraph: off.paragraph + 1,
        byte: 0,
      };
    }
    DocumentOffset {
      paragraph: off.paragraph,
      byte: next_grapheme_boundary_in_paragraph(&self.document, off.paragraph, off.byte),
    }
  }

  fn move_vertical(&mut self, dir: VDir, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
    self.pending_snap_to_paragraph = None;
    let head = self.selection.head;
    let width = self.current_layout_width();
    self.ensure_vertical_navigation_chunks(head, dir, width, window, cx);
    let _ = self.paragraph_item_sizes(window, cx);
    // Compute the new head while only reading layout snapshots. Use a local
    // scope so we can mutate selection afterwards without borrow conflicts.
    let (new_head, used_goal_x) = {
      let Some(layout) = self.layout_for_offset(head) else {
        return;
      };
      let Some((p_ix, l_ix)) = locate_line(&layout, head) else {
        cx.notify();
        return;
      };
      let cur_line = &layout.paragraphs[p_ix].lines[l_ix];
      let cur_x = self
        .goal_x
        .unwrap_or_else(|| x_for_byte(cur_line, head.byte));
      let next = match dir {
        VDir::Up => find_line_above(&layout, p_ix, l_ix),
        VDir::Down => find_line_below(&layout, p_ix, l_ix),
      };
      let Some((np, nl)) = next else {
        return self.move_to_adjacent_unmounted_paragraph(dir, extend, cur_x, window, cx);
      };
      let target_line = &layout.paragraphs[np].lines[nl];
      let new_byte = target_line.hit_test_x(cur_x);
      let new_head = DocumentOffset {
        paragraph: layout.paragraphs[np].index,
        byte: new_byte,
      };
      (new_head, cur_x)
    };
    let anchor = if extend { self.selection.anchor } else { new_head };
    let selection = EditorSelection { anchor, head: new_head };
    if self.selection == selection {
      self.goal_x = Some(used_goal_x);
      return;
    }
    self.selection = selection;
    // Preserve the goal x across the move so repeated Up/Down stays on a
    // straight column.
    self.goal_x = Some(used_goal_x);
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn move_to_adjacent_unmounted_paragraph(&mut self, dir: VDir, extend: bool, goal_x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let Some(target_paragraph) = self.adjacent_document_paragraph(head.paragraph, dir) else {
      return;
    };
    let width = self.current_layout_width();
    match dir {
      VDir::Up => {
        while self
          .valid_chunk_cache_entry(target_paragraph, width)
          .is_none_or(|entry| !entry.complete)
        {
          if !self.ensure_next_paragraph_chunk(target_paragraph, width, window, cx) {
            break;
          }
        }
      },
      VDir::Down => {
        self.ensure_next_paragraph_chunk(target_paragraph, width, window, cx);
      },
    }
    let target_byte = match self.layout_for_offset(DocumentOffset {
      paragraph: target_paragraph,
      byte: 0,
    }) {
      Some(layout) => {
        let Some(paragraph) = paragraph_layout(&layout, target_paragraph) else {
          return;
        };
        let line = match dir {
          VDir::Up => paragraph.lines.last(),
          VDir::Down => paragraph.lines.first(),
        };
        line
          .map(|line| line.hit_test_x(goal_x))
          .unwrap_or_else(|| match dir {
            VDir::Up => paragraph_text_len(&self.document.paragraphs[target_paragraph]),
            VDir::Down => 0,
          })
      },
      None => match dir {
        VDir::Up => paragraph_text_len(&self.document.paragraphs[target_paragraph]),
        VDir::Down => 0,
      },
    };
    let new_head = DocumentOffset {
      paragraph: target_paragraph,
      byte: target_byte,
    };
    let anchor = if extend { self.selection.anchor } else { new_head };
    self.selection = EditorSelection { anchor, head: new_head };
    self.goal_x = Some(goal_x);
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn adjacent_document_paragraph(&self, paragraph_ix: usize, dir: VDir) -> Option<usize> {
    match dir {
      VDir::Up => paragraph_ix.checked_sub(1),
      VDir::Down => (paragraph_ix + 1 < self.document.paragraphs.len()).then_some(paragraph_ix + 1),
    }
  }

}
