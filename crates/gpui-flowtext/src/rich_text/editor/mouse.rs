#[hotpath::measure_all]
impl RichTextEditor {
  fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    window.focus(&self.focus_handle);
    self.image_resize_drag = None;
    self.table_column_resize_drag = None;
    self.clear_drop_preview();
    self.clear_block_selection();
    self.last_drag_position = Some(event.position);
    self.goal_x = None;
    let offset = self.hit_test_document_position(event.position, window, cx);
    if self.collapse_gutter_hit(event.position, offset.paragraph) {
      self.toggle_section_collapsed_at_paragraph(offset.paragraph, &[0, 1, 2, 3], cx);
      return;
    }
    self.drag_anchor = None;
    self.smart_selection_left_anchor_word = false;
    self.smart_selection_exact_override = false;
    if event.click_count <= 1 && !event.modifiers.shift && !self.selection.is_caret() && offset_in_range(offset, self.selection.normalized()) {
      self.selecting = false;
      self.pending_text_drag = Some(PendingTextDrag {
        start_position: event.position,
        source_selection: self.selection.clone(),
      });
      self.active_text_drag = None;
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.selecting = true;
    self.drag_granularity = match event.click_count {
      0 | 1 => SelectionGranularity::Character,
      2 => SelectionGranularity::Word,
      _ => SelectionGranularity::Paragraph,
    };
    self.selection = match self.drag_granularity {
      SelectionGranularity::Character if event.modifiers.shift => EditorSelection {
        anchor: self.selection.anchor,
        head: offset,
      },
      SelectionGranularity::Character => EditorSelection {
        anchor: offset,
        head: offset,
      },
      SelectionGranularity::Word => selection_for_word_at(&self.document, offset),
      SelectionGranularity::Paragraph => selection_for_paragraph_at(&self.document, offset.paragraph),
    };
    self.drag_anchor = Some(self.selection.anchor);
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn collapse_gutter_hit(&self, position: Point<Pixels>, paragraph_ix: usize) -> bool {
    let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
      return false;
    };
    if !matches!(paragraph.style, ParagraphStyle::Custom(0..=3)) {
      return false;
    }
    let Some(layout) = self.layout_for_offset(DocumentOffset {
      paragraph: paragraph_ix,
      byte: paragraph_text_len(paragraph),
    }) else {
      return false;
    };
    let origin = layout.bounds.map(|bounds| bounds.origin).unwrap_or(point(px(0.0), px(0.0)));
    let Some(caret) = caret_bounds(
      &layout,
      DocumentOffset {
        paragraph: paragraph_ix,
        byte: paragraph_text_len(paragraph),
      },
      origin,
    ) else {
      return false;
    };
    position.x >= caret.right() + px(2.0)
      && position.x <= caret.right() + px(26.0)
      && position.y >= caret.top() - px(10.0)
      && position.y <= caret.bottom() + px(12.0)
  }

  fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
    self.last_drag_position = Some(event.position);
    if !event.dragging() {
      let paragraph_ix = self.hit_test_document_position(event.position, window, cx).paragraph;
      let next_hover = self
        .section_collapsed_at_heading(paragraph_ix, &[0, 1, 2, 3])
        .is_some()
        .then_some(paragraph_ix);
      if self.hovered_collapse_paragraph != next_hover {
        self.hovered_collapse_paragraph = next_hover;
        cx.notify();
      }
    }
    if self.update_table_column_resize_drag(event.position, cx) {
      return;
    }
    if self.update_image_resize_drag(event.position, cx) {
      return;
    }
    if event.dragging()
      && let Some(BlockSelection::Equation(block_ix)) = self.selected_block
      && let Some(byte) = self.equation_source_byte_at(block_ix, event.position, window, cx)
    {
      self.equation_source_caret = byte;
      self.last_drag_position = Some(event.position);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    if event.dragging()
      && let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block
      && let Some((
        BlockSelection::TableCell {
          row_ix: hit_row,
          cell_ix: hit_cell,
          ..
        },
        paragraph_ix,
        byte,
      )) = self.table_cell_selection_at(block_ix, event.position, window, cx)
      && row_ix == hit_row
      && cell_ix == hit_cell
    {
      self.table_cell_block_ix = paragraph_ix;
      self.table_cell_caret = byte;
      self.last_drag_position = Some(event.position);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    if let Some(pending_drag) = self.pending_text_drag.clone() {
      self.last_drag_position = Some(event.position);
      if point_distance_squared(pending_drag.start_position, event.position) < 16.0 {
        return;
      }
      let source_range = pending_drag.source_selection.normalized();
      self.active_text_drag = Some(ActiveTextDrag {
        source_range: source_range.clone(),
        fragment: selected_rich_fragment(&self.document, source_range),
      });
      self.selection = pending_drag.source_selection;
      self.pending_text_drag = None;
    }
    if self.active_text_drag.is_some() {
      self.last_drag_position = Some(event.position);
      self.autoscroll_for_drag(event.position);
      self.ensure_drag_autoscroll_task(cx);
      let drop = self.hit_test_document_position(event.position, window, cx);
      let selection = EditorSelection { anchor: drop, head: drop };
      if self.selection != selection {
        self.selection = selection;
        self.scroll_head_into_view();
        self.reset_caret_blink(cx);
      }
      cx.notify();
      return;
    }
    if !self.selecting {
      return;
    }
    self.last_drag_position = Some(event.position);
    self.autoscroll_for_drag(event.position);
    self.ensure_drag_autoscroll_task(cx);
    let head = self.hit_test_document_position(event.position, window, cx);
    let anchor = self.drag_anchor.unwrap_or(self.selection.anchor);
    if self.config.smart_word_selection && self.drag_granularity == SelectionGranularity::Character && !event.modifiers.alt {
      if !offset_is_in_same_word_as(&self.document, anchor, head) {
        self.smart_selection_left_anchor_word = true;
      } else if self.smart_selection_left_anchor_word {
        self.smart_selection_exact_override = true;
      }
    }
    let selection = expand_mouse_selection(
      &self.document,
      anchor,
      head,
      self.drag_granularity,
      MouseSelectionOptions {
        smart_word_selection: self.config.smart_word_selection,
        exact: event.modifiers.alt || self.smart_selection_exact_override,
      },
    );
    if self.selection != selection {
      self.selection = selection;
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    } else {
      cx.notify();
    }
  }

  fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
    if self.finish_table_column_resize_drag(cx) {
      self.selecting = false;
      self.drag_granularity = SelectionGranularity::Character;
      self.drag_anchor = None;
      self.smart_selection_left_anchor_word = false;
      self.smart_selection_exact_override = false;
      self.last_drag_position = None;
      self.autoscroll_active = false;
      return;
    }
    if self.finish_image_resize_drag(cx) {
      self.selecting = false;
      self.drag_granularity = SelectionGranularity::Character;
      self.drag_anchor = None;
      self.smart_selection_left_anchor_word = false;
      self.smart_selection_exact_override = false;
      self.last_drag_position = None;
      self.autoscroll_active = false;
      return;
    }
    if let Some(active_drag) = self.active_text_drag.take() {
      let drop = self.hit_test_document_position(event.position, window, cx);
      self.clear_drop_preview();
      self.move_rich_text_fragment(active_drag, drop, cx);
    } else if self.pending_text_drag.take().is_some() {
      self.clear_drop_preview();
      let caret = self.hit_test_document_position(event.position, window, cx);
      self.selection = EditorSelection { anchor: caret, head: caret };
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    }
    if self.selecting {
      self.apply_armed_inline_tool_to_selection(cx);
    }
    self.selecting = false;
    self.drag_granularity = SelectionGranularity::Character;
    self.drag_anchor = None;
    self.smart_selection_left_anchor_word = false;
    self.smart_selection_exact_override = false;
    self.last_drag_position = None;
    self.autoscroll_active = false;
    self.clear_drop_preview();
  }

  fn move_rich_text_fragment(&mut self, drag: ActiveTextDrag, drop: DocumentOffset, cx: &mut Context<Self>) {
    if self.reject_projection_first_edit("Collaborative text move", cx) {
      self.clear_drop_preview();
      return;
    }
    if offset_in_range(drop, drag.source_range.clone()) {
      self.clear_drop_preview();
      self.selection = EditorSelection {
        anchor: drag.source_range.start,
        head: drag.source_range.end,
      };
      cx.notify();
      return;
    }
    let before_selection = EditorSelection {
      anchor: drag.source_range.start,
      head: drag.source_range.end,
    };
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let source_range = drag.source_range.clone();
    let adjusted_drop = adjust_drop_after_source_delete(drop, source_range.clone());
    self.selection = before_selection.clone();
    self.delete_selection_internal();
    let inserted_start = adjusted_drop;
    let inserted_end = insert_rich_fragment_at(&mut self.document, inserted_start, &drag.fragment);
    self.selection = EditorSelection {
      anchor: inserted_end,
      head: inserted_end,
    };
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::MoveRichText {
        source_range,
        adjusted_drop,
        inserted_range: inserted_start..inserted_end,
        fragment: drag.fragment,
      }],
      canonical_operations: vec![CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph: paragraph_id_at(&self.document, inserted_start.paragraph),
        before: capture_document_span(
          &self.document,
          inserted_start.paragraph..(inserted_start.paragraph + 1).min(self.document.paragraphs.len()),
        ),
        after: capture_document_span(
          &self.document,
          inserted_start.paragraph..(inserted_end.paragraph + 1).min(self.document.paragraphs.len()),
        ),
      }],
    });
    self.redo_stack.clear();
    self.after_text_mutation(cx);
    self.mark_document_changed(after_generation, cx);
    self.clear_drop_preview();
  }

  pub(super) fn reset_caret_blink(&mut self, cx: &mut Context<Self>) {
    if self.disposed {
      self.caret_visible = false;
      self.caret_blink_active = false;
      return;
    }
    self.caret_visible = true;
    self.ensure_caret_blink_task(cx);
  }

  fn ensure_caret_blink_task(&mut self, cx: &mut Context<Self>) {
    if self.disposed {
      self.caret_blink_active = false;
      return;
    }
    if self.caret_blink_active {
      return;
    }
    self.caret_blink_active = true;
    cx.spawn(async move |editor, cx| {
      loop {
        Timer::after(Duration::from_millis(530)).await;
        let keep_running = editor
          .update(cx, |editor, cx| {
            if editor.disposed || !editor.caret_blink_active {
              editor.caret_blink_active = false;
              editor.caret_visible = false;
              return false;
            }
            editor.caret_visible = !editor.caret_visible;
            cx.notify();
            true
          })
          .unwrap_or(false);
        if !keep_running {
          break;
        }
      }
    })
    .detach();
  }

  fn ensure_focus_subscriptions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.disposed {
      self.focus_subscriptions = Vec::new();
      return;
    }
    if !self.focus_subscriptions.is_empty() {
      return;
    }
    let focus_handle = self.focus_handle.clone();
    self
      .focus_subscriptions
      .push(cx.on_focus(&focus_handle, window, |editor, _, cx| {
        editor.reset_caret_blink(cx);
        cx.notify();
      }));
    let focus_handle = self.focus_handle.clone();
    self
      .focus_subscriptions
      .push(cx.on_blur(&focus_handle, window, |editor, _, cx| {
        editor.caret_blink_active = false;
        editor.caret_visible = false;
        cx.notify();
      }));
  }

  fn scroll_head_into_view(&self) {
    let Some(layout) = self.layout_for_offset(self.selection.head) else {
      return;
    };
    let Some(bounds) = layout.bounds else {
      return;
    };
    let Some(caret) = caret_bounds(&layout, self.selection.head, bounds.origin) else {
      return;
    };
    scroll_rect_into_view(&self.scroll_handle, caret, px(4.0));
  }

  fn autoscroll_for_drag(&self, position: Point<Pixels>) -> bool {
    let viewport = self.scroll_handle.bounds();
    let step = drag_autoscroll_step(viewport, position);
    step != px(0.0) && scroll_by(&self.scroll_handle, step)
  }

  fn ensure_drag_autoscroll_task(&mut self, cx: &mut Context<Self>) {
    if self.disposed {
      self.autoscroll_active = false;
      return;
    }
    if self.autoscroll_active || !self.selecting {
      return;
    }
    let Some(position) = self.last_drag_position else {
      return;
    };
    if drag_autoscroll_step(self.scroll_handle.bounds(), position) == px(0.0) {
      return;
    }

    self.autoscroll_active = true;
    cx.spawn(async move |editor, cx| {
      loop {
        Timer::after(Duration::from_millis(16)).await;
        let keep_running = editor
          .update(cx, |editor, cx| {
            if editor.disposed {
              editor.autoscroll_active = false;
              return false;
            }
            let Some(position) = editor.last_drag_position else {
              editor.autoscroll_active = false;
              return false;
            };
            if !editor.selecting {
              editor.autoscroll_active = false;
              return false;
            }

            if !editor.autoscroll_for_drag(position) {
              editor.autoscroll_active = false;
              return false;
            }

            if let Some(head) = editor.hit_test_cached_position(position)
              && editor.selection.head != head
            {
              editor.selection.head = head;
            }
            cx.notify();
            true
          })
          .unwrap_or(false);
        if !keep_running {
          break;
        }
      }
    })
    .detach();
  }
}
