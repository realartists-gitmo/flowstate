impl Drop for RichTextEditor {
  fn drop(&mut self) {
    self.clear_document_equation_caches();
    self.release_transient_memory();
  }
}

impl Focusable for RichTextEditor {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for RichTextEditor {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.ensure_focus_subscriptions(window, cx);
    if self.image_resize_drag.is_some() {
      let editor = cx.entity();
      window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
        if phase.bubble() {
          editor.update(cx, |editor, cx| {
            editor.finish_image_resize_drag(cx);
          });
        }
      });
    }
    let render_layout = self.prepare_render_layout(window, cx);
    let width = render_layout.width;
    let item_sizes = render_layout.item_sizes.clone();
    let scroll_handle = self.scroll_handle.clone();
    let render_item_sizes = item_sizes.clone();
    let render_items = render_layout.items.clone();
    let hide_initial_layout = render_layout.hide_initial_layout;
    div()
      .size_full()
      .id("rich-text-editor")
      .relative()
      .bg(self.document.theme.document_background_color)
      .track_focus(&self.focus_handle(cx))
      .key_context("RichTextEditor")
      .cursor(CursorStyle::IBeam)
      // Action handlers — these resolve via the keymap registered in main.rs
      // because the focused element's context matches "RichTextEditor".
      .on_action(cx.listener(Self::on_move_left))
      .on_action(cx.listener(Self::on_move_right))
      .on_action(cx.listener(Self::on_move_up))
      .on_action(cx.listener(Self::on_move_down))
      .on_action(cx.listener(Self::on_move_line_start))
      .on_action(cx.listener(Self::on_move_line_end))
      .on_action(cx.listener(Self::on_select_left))
      .on_action(cx.listener(Self::on_select_right))
      .on_action(cx.listener(Self::on_select_up))
      .on_action(cx.listener(Self::on_select_down))
      .on_action(cx.listener(Self::on_select_line_start))
      .on_action(cx.listener(Self::on_select_line_end))
      .on_action(cx.listener(Self::on_select_all))
      .on_action(cx.listener(Self::on_move_word_left))
      .on_action(cx.listener(Self::on_move_word_right))
      .on_action(cx.listener(Self::on_select_word_left))
      .on_action(cx.listener(Self::on_select_word_right))
      .on_action(cx.listener(Self::on_delete_word_backward))
      .on_action(cx.listener(Self::on_delete_word_forward))
      .on_action(cx.listener(Self::on_page_up))
      .on_action(cx.listener(Self::on_page_down))
      .on_action(cx.listener(Self::on_select_page_up))
      .on_action(cx.listener(Self::on_select_page_down))
      .on_action(cx.listener(Self::on_move_document_start))
      .on_action(cx.listener(Self::on_move_document_end))
      .on_action(cx.listener(Self::on_select_document_start))
      .on_action(cx.listener(Self::on_select_document_end))
      .on_action(cx.listener(Self::on_copy))
      .on_action(cx.listener(Self::on_cut))
      .on_action(cx.listener(Self::on_paste))
      .on_action(cx.listener(Self::on_undo))
      .on_action(cx.listener(Self::on_redo))
      .on_action(cx.listener(Self::on_set_paragraph_pocket))
      .on_action(cx.listener(Self::on_set_paragraph_hat))
      .on_action(cx.listener(Self::on_set_paragraph_block))
      .on_action(cx.listener(Self::on_set_paragraph_tag))
      .on_action(cx.listener(Self::on_set_paragraph_analytic))
      .on_action(cx.listener(Self::on_set_paragraph_undertag))
      .on_action(cx.listener(Self::on_toggle_cite))
      .on_action(cx.listener(Self::on_toggle_underline))
      .on_action(cx.listener(Self::on_toggle_strikethrough))
      .on_action(cx.listener(Self::on_toggle_emphasis))
      .on_action(cx.listener(Self::on_set_highlight_spoken))
      .on_action(cx.listener(Self::on_apply_highlight_to_selection))
      .on_action(cx.listener(Self::on_clear_formatting))
      .on_action(cx.listener(Self::on_clear_highlight))
      .on_action(cx.listener(Self::on_insert_image))
      .on_action(cx.listener(Self::on_insert_table))
      .on_action(cx.listener(Self::on_insert_equation))
      .on_action(cx.listener(Self::on_backspace))
      .on_action(cx.listener(Self::on_delete))
      .on_action(cx.listener(Self::on_insert_newline))
      .on_action(cx.listener(Self::on_insert_soft_line_break))
      // Catch printable characters (anything with a `key_char`) and insert
      // them as text. Action keys (arrows, Enter, etc.) have `key_char = None`
      // so they fall through to the action system above.
      .on_key_down(cx.listener(Self::on_key_down_event))
      .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
      .on_mouse_move(cx.listener(Self::on_mouse_move))
      .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
      .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
      .drag_over::<ExternalPaths>(|style, _, _, _| style)
      .on_drop(cx.listener(Self::on_file_drop))
      .child(
        v_virtual_list(cx.entity(), "rich-text-virtual-document", item_sizes, move |editor, range, window, cx| {
          let generation = editor.begin_visible_layout(range.clone());
          range
            .map(|item_ix| {
              let Some(item) = render_items.get(item_ix).cloned() else {
                return EmptyVirtualItemElement.into_any_element();
              };
              match item {
                VirtualItem::HiddenBlock { .. } => EmptyVirtualItemElement.into_any_element(),
                VirtualItem::ParagraphChunk {
                  paragraph_ix,
                  chunk_ix,
                  ..
                } => VirtualParagraphChunkElement {
                  editor: cx.entity(),
                  item_ix,
                  paragraph_ix,
                  chunk_ix,
                  generation,
                  layout: WordElementLayout::default(),
                }
                .into_any_element(),
                VirtualItem::ParagraphRemainder { paragraph_ix, .. } => {
                  let width = editor.current_layout_width();
                  let chunk_ix = editor.materialize_paragraph_remainder_for_render(paragraph_ix, width, window, cx);
                  if let Some(chunk_ix) = chunk_ix {
                    VirtualParagraphChunkElement {
                      editor: cx.entity(),
                      item_ix,
                      paragraph_ix,
                      chunk_ix,
                      generation,
                      layout: WordElementLayout::default(),
                    }
                    .into_any_element()
                  } else {
                    EmptyVirtualItemElement.into_any_element()
                  }
                },
                VirtualItem::StructuralBlock { block_ix } => {
                let editor_entity = cx.entity();
                let selection = match editor.document.blocks.get(block_ix) {
                  Some(Block::Image(_)) => Some(BlockSelection::Image(block_ix)),
                  Some(Block::Equation(_)) => Some(BlockSelection::Equation(block_ix)),
                  Some(Block::Table(_)) => Some(BlockSelection::Table(block_ix)),
                  Some(Block::Paragraph(_)) | None => None,
                };
                let editor_for_down = editor_entity.clone();
                div()
                  .size_full()
                  .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    cx.stop_propagation();
                    editor_for_down.update(cx, |editor, cx| {
                      if editor.start_table_column_resize_if_hit(block_ix, event.position, window, cx) {
                        return;
                      }
                      if let Some(selection) = editor.selection_for_object_block(block_ix) {
                        editor.select_block_from_click(block_ix, selection, event.position, window, cx);
                      }
                    });
                  })
                  .when_some(selection, |this, selection| {
                    let editor_entity = editor_entity.clone();
                    this.on_mouse_up(MouseButton::Left, move |event, window, cx| {
                      cx.stop_propagation();
                      editor_entity.update(cx, |editor, cx| {
                        if editor.finish_table_column_resize_drag(cx) {
                          return;
                        }
                        if !matches!(
                          editor.selected_block,
                          Some(BlockSelection::TableCell { .. } | BlockSelection::Equation(_))
                        ) {
                          editor.select_block_from_click(block_ix, selection, event.position, window, cx);
                        }
                      });
                    })
                  })
                  .child(match editor.document.blocks.get(block_ix) {
                    Some(Block::Image(image)) => render_image_block(
                      &editor.document,
                      image,
                      block_ix,
                      render_item_sizes.get(item_ix).copied().unwrap_or_else(|| size(px(900.0), px(1.0))),
                      editor.selected_block,
                      editor_entity.clone(),
                    ),
                    Some(Block::Equation(equation)) => render_equation_block(
                      &editor.document,
                      equation,
                      block_ix,
                      render_item_sizes.get(item_ix).copied().unwrap_or_else(|| size(px(900.0), px(1.0))),
                      editor.selected_block == Some(BlockSelection::Equation(block_ix)) || editor.block_is_inside_text_selection(block_ix),
                      editor.equation_source_selection_for_render(block_ix),
                    ),
                    Some(Block::Table(_)) | Some(Block::Paragraph(_)) | None => VirtualBlockElement {
                      editor: editor_entity,
                      block_ix,
                      layout: WordElementLayout::default(),
                    }
                    .into_any_element(),
                  })
                  .into_any_element()
                },
              }
            })
            .collect::<Vec<_>>()
        })
        .with_fixed_cross_axis_size(width)
        .track_scroll(&scroll_handle)
        .when(hide_initial_layout, |this| this.opacity(0.0)),
      )
      .child(
        div()
          .absolute()
          .top_0()
          .left_0()
          .right_0()
          .bottom_0()
          .child(
            Scrollbar::vertical(&self.scroll_handle)
              .scrollbar_show(ScrollbarShow::Always)
              .when(!DISABLE_SCROLL_LIMITING_FUNCTIONS, |this| this.max_fps(SCROLLBAR_DRAG_MAX_FPS)),
          ),
      )
  }
}
