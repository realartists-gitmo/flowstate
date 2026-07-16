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
    if self.config.show_section_collapse_controls && self.collapse_gutter_hit(event.position, offset.paragraph) {
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
    let before_selection = self.selection.clone();
    // Mouse placement carries no explicit affinity/gravity (§16): clicks/drags
    // produce neutral endpoints.
    self.selection = match self.drag_granularity {
      SelectionGranularity::Character if event.modifiers.shift => EditorSelection::range(self.selection.anchor, offset),
      SelectionGranularity::Character => EditorSelection::collapsed(offset),
      SelectionGranularity::Word => selection_for_word_at(&self.document, offset),
      SelectionGranularity::Paragraph => selection_for_paragraph_at(&self.document, offset.paragraph),
    };
    self.fidelity_caret_set_from("on_mouse_down", &before_selection);
    self.drag_anchor = Some(self.selection.anchor);
    self.reset_caret_blink(cx);
    if self.selection != before_selection {
      self.note_explicit_selection_movement();
      self.emit_selection_changed(cx);
    }
    cx.notify();
  }

  fn collapse_gutter_hit(&self, position: Point<Pixels>, paragraph_ix: usize) -> bool {
    let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
      return false;
    };
    if self
      .section_collapsed_at_heading(paragraph_ix, &[0, 1, 2, 3])
      .is_none()
    {
      return false;
    }
    let Some(layout) = self.layout_for_offset(DocumentOffset {
      paragraph: paragraph_ix,
      byte: paragraph_text_len(paragraph),
    }) else {
      return false;
    };
    let origin = layout
      .bounds
      .map(|bounds| bounds.origin)
      .unwrap_or(point(px(0.0), px(0.0)));
    let Some(caret) = caret_bounds(
      &layout,
      DocumentOffset {
        paragraph: paragraph_ix,
        byte: paragraph_text_len(paragraph),
      },
      VisualGravity::Neutral,
      origin,
    ) else {
      return false;
    };
    position.x >= caret.right() + px(2.0)
      && position.x <= caret.right() + px(26.0)
      && position.y >= caret.top() - px(10.0)
      && position.y <= caret.bottom() + px(12.0)
  }

  /// M2: right-click in the text body. Standard semantics — a click outside
  /// the current selection moves the caret there first — then the host hook
  /// opens its menu for the resolved target.
  pub(super) fn on_right_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    let Some(hook) = self.context_menu_hook.clone() else {
      return;
    };
    window.focus(&self.focus_handle);
    let offset = self.hit_test_document_position(event.position, window, cx);
    if self.selection.is_caret() || !offset_in_range(offset, self.selection.normalized()) {
      let before_selection = self.selection.clone();
      self.selection = EditorSelection::collapsed(offset);
      self.fidelity_caret_set_from("on_right_mouse_down", &before_selection);
      if self.selection != before_selection {
        self.note_explicit_selection_movement();
        self.emit_selection_changed(cx);
      }
      cx.notify();
    }
    let over_annotation = self
      .annotation_selections()
      .iter()
      .any(|annotation| !annotation.selection.is_caret() && offset_in_range(offset, annotation.selection.normalized()));
    let has_selection = !self.selection.is_caret();
    hook(
      event.position,
      EditorContextTarget::Text {
        offset,
        has_selection,
        over_annotation,
      },
      window,
      cx,
    );
  }

  /// M2: right-click on an object block. The block was just selected by the
  /// wrapper (right-click selects, like left-click); resolve the precise
  /// target from the block kind and hand it to the host.
  pub(super) fn open_block_context_menu(&mut self, block_ix: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let Some(hook) = self.context_menu_hook.clone() else {
      return;
    };
    let target = match self.document.blocks.get(block_ix) {
      Some(Block::Image(_)) => EditorContextTarget::Image { block_ix },
      Some(Block::Table(_)) => EditorContextTarget::Table { block_ix },
      Some(Block::Equation(_)) => EditorContextTarget::Equation { block_ix },
      _ => return,
    };
    hook(position, target, window, cx);
  }

  fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
    self.last_drag_position = Some(event.position);
    if !event.dragging() {
      let hover_offset = self.hit_test_document_position(event.position, window, cx);
      let paragraph_ix = hover_offset.paragraph;
      // C-S4: annotation hover reuses this hit test; only bothers when a
      // comment overlay is actually installed.
      if !self.annotation_selections().is_empty() || self.hovered_annotation.is_some() {
        self.update_annotation_hover(hover_offset, cx);
      }
      let next_hover = self.config.show_section_collapse_controls.then_some(paragraph_ix).filter(|paragraph_ix| {
        self.section_collapsed_at_heading(*paragraph_ix, &[0, 1, 2, 3]).is_some()
      });
      if self.hovered_collapse_paragraph != next_hover {
        self.hovered_collapse_paragraph = next_hover;
        cx.notify();
      }
    }
    if self.update_table_move_drag(event.position, window, cx) {
      return;
    }
    if self.update_table_column_resize_drag(event.position, cx) {
      return;
    }
    if self.update_image_resize_drag(event.position, cx) {
      return;
    }
    if event.dragging()
      && let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block
      && let Some((
        BlockSelection::TableCell {
          row_ix: hit_row,
          cell_ix: hit_cell,
          ..
        },
        paragraph_ix,
        byte,
      )) = self.table_cell_selection_at(block_ix, event.position, window, cx)
    {
      // B-S7: dragging OUT of the anchor cell selects the rectangle; back
      // inside it resumes the in-cell text drag.
      if row_ix != hit_row || cell_ix != hit_cell {
        self.cell_range = Some(CellRangeSelection {
          block_ix,
          anchor: (row_ix, cell_ix),
          head: (hit_row, hit_cell),
        });
        self.last_drag_position = Some(event.position);
        cx.notify();
        return;
      }
      self.cell_range = None;
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
      let before_selection = self.selection.clone();
      self.selection = pending_drag.source_selection;
      self.fidelity_caret_set_from("on_mouse_move/begin-text-drag", &before_selection);
      if self.selection != before_selection {
        self.emit_selection_changed(cx);
      }
      self.pending_text_drag = None;
    }
    if self.active_text_drag.is_some() {
      self.last_drag_position = Some(event.position);
      self.autoscroll_for_drag(event.position);
      self.ensure_drag_autoscroll_task(cx);
      let drop = self.hit_test_document_position(event.position, window, cx);
      let selection = EditorSelection::collapsed(drop);
      if self.selection != selection {
        self.note_explicit_selection_movement();
        let fid_before = self.fidelity_caret_before();
        self.selection = selection;
        self.fidelity_caret_set("on_mouse_move/text-drag-caret", &fid_before);
        self.scroll_head_into_view();
        self.reset_caret_blink(cx);
        self.emit_selection_changed(cx);
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
      self.note_explicit_selection_movement();
      let fid_before = self.fidelity_caret_before();
      self.selection = selection;
      self.fidelity_caret_set("on_mouse_move/drag-select", &fid_before);
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      self.emit_selection_changed(cx);
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
      let before_selection = self.selection.clone();
      self.selection = EditorSelection::collapsed(caret);
      self.fidelity_caret_set_from("on_mouse_up/cancel-text-drag", &before_selection);
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      if self.selection != before_selection {
        self.emit_selection_changed(cx);
      }
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

  /// O-S5: whole-section move (heading + descendants) as the SAME grouped
  /// delete+insert the drag-drop path uses — one undo group, typed intents,
  /// collab-convergent. `target_paragraph` is where the section's first
  /// paragraph should land, in PRE-move coordinates (`paragraph_count` means
  /// "after the last paragraph"). Boundary-inclusive: the paragraph break
  /// travels with the section, so no empty shells are left behind.
  pub fn move_paragraph_range(
    &mut self,
    start_paragraph: usize,
    end_paragraph_exclusive: usize,
    target_paragraph: usize,
    cx: &mut Context<Self>,
  ) -> bool {
    let paragraph_count = self.document.paragraphs.len();
    if start_paragraph >= end_paragraph_exclusive
      || end_paragraph_exclusive > paragraph_count
      || target_paragraph > paragraph_count
      || (target_paragraph >= start_paragraph && target_paragraph <= end_paragraph_exclusive)
      || (start_paragraph == 0 && end_paragraph_exclusive == paragraph_count)
    {
      return false;
    }
    let paragraph_end = |ix: usize| DocumentOffset {
      paragraph: ix,
      byte: self.document.paragraphs.get(ix).map_or(0, paragraph_text_len),
    };
    // Capture with the SECTION's paragraph break included: a trailing break
    // ([...section, ""]) when a following paragraph exists, else a leading
    // one (["", ...section]).
    let (source_range, mut fragment_paragraphs_lead_with_break) = if end_paragraph_exclusive < paragraph_count {
      (
        DocumentOffset {
          paragraph: start_paragraph,
          byte: 0,
        }..DocumentOffset {
          paragraph: end_paragraph_exclusive,
          byte: 0,
        },
        false,
      )
    } else {
      (paragraph_end(start_paragraph - 1)..paragraph_end(end_paragraph_exclusive - 1), true)
    };
    let mut fragment = selected_rich_fragment(&self.document, source_range.clone());
    if fragment.paragraphs.is_empty() {
      return false;
    }
    // The drop wants the break on the matching side: before a paragraph →
    // trailing break; at a paragraph's end (incl. document end) → leading.
    let drop_needs_leading_break = target_paragraph >= paragraph_count;
    let drop = if target_paragraph >= paragraph_count {
      paragraph_end(paragraph_count - 1)
    } else if fragment_paragraphs_lead_with_break {
      // Leading-break fragment lands at the END of the paragraph before the
      // target (a {target, 0} drop would merge the section tail into the
      // target's text).
      if target_paragraph == 0 {
        // Rotate to a trailing break so the section can land before the
        // first paragraph.
        rotate_fragment_break_to_tail(&mut fragment);
        fragment_paragraphs_lead_with_break = false;
        DocumentOffset { paragraph: 0, byte: 0 }
      } else {
        paragraph_end(target_paragraph - 1)
      }
    } else {
      DocumentOffset {
        paragraph: target_paragraph,
        byte: 0,
      }
    };
    if drop_needs_leading_break && !fragment_paragraphs_lead_with_break {
      rotate_fragment_break_to_head(&mut fragment);
    }
    self.move_rich_text_fragment(
      ActiveTextDrag {
        source_range,
        fragment,
      },
      drop,
      cx,
    );
    true
  }

  /// Drag-drop text move, Loro-first (spec §5): ONE undo group of two typed
  /// intents — delete the source range, then insert the dragged fragment at
  /// the drop position. The fragment content was captured at drag start (in
  /// `ActiveTextDrag`), BEFORE the delete; after the delete commits the
  /// projection has changed, so the drop caret is recomputed in post-delete
  /// coordinates and resolved to a `TextAnchor` through the reconciled
  /// identity map inside the write helpers. No direct projection mutation,
  /// no local history record — undo is the authority's grouped undo.
  fn move_rich_text_fragment(&mut self, drag: ActiveTextDrag, drop: DocumentOffset, cx: &mut Context<Self>) {
    if offset_in_range(drop, drag.source_range.clone()) {
      self.clear_drop_preview();
      let before_selection = self.selection.clone();
      self.selection = EditorSelection::range(drag.source_range.start, drag.source_range.end);
      self.fidelity_caret_set_from("move_rich_text_fragment/drop-on-source", &before_selection);
      if self.selection != before_selection {
        self.emit_selection_changed(cx);
      }
      cx.notify();
      return;
    }
    if drag.fragment.paragraphs.is_empty() {
      self.clear_drop_preview();
      return;
    }
    let source_range = drag.source_range.clone();
    // Drop position expressed in post-delete projection coordinates.
    let adjusted_drop = adjust_drop_after_source_delete(drop, source_range.clone());
    self.begin_undo_group();
    if !self.write_delete_offset_range(source_range.clone(), cx) {
      self.end_undo_group();
      self.clear_drop_preview();
      return;
    }
    // The delete committed; the projection (and identity map) advanced. Land
    // the caret at the recomputed drop position so the insert helpers anchor
    // against the post-delete projection — never the stale pre-delete offset.
    let caret = self.clamp_offset_to_document(adjusted_drop);
    let fid_before = self.fidelity_caret_before();
    self.selection = EditorSelection::collapsed(caret);
    self.fidelity_caret_set("move_rich_text_fragment/drop-caret", &fid_before);
    self.emit_selection_changed(cx);
    // Plain single-paragraph unstyled content moves as a plain text insert;
    // anything styled or multi-paragraph moves as a rich fragment intent.
    let plain_text = (drag.fragment.paragraphs.len() == 1)
      .then(|| &drag.fragment.paragraphs[0])
      .filter(|paragraph| paragraph.runs.iter().all(|run| run.styles == RunStyles::default()))
      .map(|paragraph| paragraph.runs.iter().map(|run| run.text.as_str()).collect::<String>());
    match plain_text {
      Some(text) => {
        self.write_insert_text_at_caret(&text, cx);
      },
      None => {
        let blocks = drag
          .fragment
          .paragraphs
          .into_iter()
          .map(FragmentBlock::Paragraph)
          .collect::<Vec<_>>();
        self.write_insert_rich_fragment_at_caret(blocks, cx);
      },
    }
    self.end_undo_group();
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

  /// W-S3: live window handoff — drop the old window's focus subscriptions
  /// so the next interaction re-mints them against the adopting window.
  pub fn clear_focus_subscriptions(&mut self) {
    self.focus_subscriptions = Vec::new();
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
    let Some(caret) = caret_bounds(&layout, self.selection.head, self.selection.head_gravity, bounds.origin) else {
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
              let fid_before = editor.fidelity_caret_before();
              editor.selection.head = head;
              editor.fidelity_caret_set("autoscroll_drag/extend-head", &fid_before);
              editor.emit_selection_changed(cx);
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


/// O-S5: move the fragment's paragraph break from the tail to the head
/// ([...section, ""] → ["", ...section]) so it can land at a paragraph END.
fn rotate_fragment_break_to_head(fragment: &mut RichClipboardFragment) {
  if fragment
    .paragraphs
    .last()
    .is_some_and(|paragraph| paragraph.runs.is_empty())
  {
    fragment.paragraphs.pop();
    fragment.paragraphs.insert(
      0,
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      },
    );
  }
}

/// O-S5: the inverse rotation (["", ...section] → [...section, ""]) for
/// landing BEFORE a paragraph.
fn rotate_fragment_break_to_tail(fragment: &mut RichClipboardFragment) {
  if fragment
    .paragraphs
    .first()
    .is_some_and(|paragraph| paragraph.runs.is_empty())
  {
    fragment.paragraphs.remove(0);
    fragment.paragraphs.push(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    });
  }
}
