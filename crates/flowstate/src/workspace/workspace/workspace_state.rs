#[hotpath::measure_all]
impl Workspace {
  pub fn toggle_ribbon(&mut self, cx: &mut Context<Self>) {
    self.ribbon_collapsed = !self.ribbon_collapsed;
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  pub fn toggle_outline(&mut self, cx: &mut Context<Self>) {
    let width = self
      .body_resizable_state
      .read(cx)
      .sizes()
      .first()
      .copied()
      .unwrap_or(px(240.0));
    let delta = if self.outline_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH - width
    } else {
      width - SIDE_PANEL_COLLAPSED_WIDTH
    };
    self.prepare_active_editor_for_width_delta(delta, cx);
    self.outline_collapsed = !self.outline_collapsed;
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  /// C-S3: open the comments rail (creating the panel if needed) and focus
  /// its composer — the keybinding + Collaborate-menu entry point.
  pub fn open_comments_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.active_toolkit_tool != Some(ToolkitTool::Comments) {
      self.toggle_toolkit_tool(ToolkitTool::Comments, cx);
    }
    if let Some(panel) = self.comments_panel.clone() {
      panel.update(cx, |panel, cx| panel.focus_composer(window, cx));
    } else {
      // First open: the panel is created by the next render pass; focus after.
      cx.on_next_frame(window, |workspace, window, cx| {
        if let Some(panel) = workspace.comments_panel.clone() {
          panel.update(cx, |panel, cx| panel.focus_composer(window, cx));
        }
      });
    }
  }

  /// C-S5: recount unread comment threads for the active document (the rail
  /// badge). Debounced; triggered from the editor observers (the session's
  /// comment nudge lands there too) and on document switches.
  pub(crate) fn schedule_comment_unread_refresh(&mut self, cx: &mut Context<Self>) {
    if self.comment_unread_refresh_pending {
      return;
    }
    self.comment_unread_refresh_pending = true;
    cx.spawn(async move |workspace, cx| {
      cx.background_executor()
        .timer(std::time::Duration::from_millis(400))
        .await;
      let Ok(Some((io, generation))) = workspace.update(cx, |workspace, _| {
        workspace.comment_unread_refresh_pending = false;
        workspace.comment_unread_refresh_generation = workspace.comment_unread_refresh_generation.wrapping_add(1);
        let io = workspace
          .active_document_id
          .and_then(|id| workspace.document_runtimes.get(&id).cloned());
        io.map(|io| (io, workspace.comment_unread_refresh_generation))
      }) else {
        return;
      };
      let result = io.comments().await;
      let _ = workspace.update(cx, |workspace, cx| {
        if workspace.comment_unread_refresh_generation != generation {
          return;
        }
        let Ok(threads) = result else { return };
        let count = threads
          .iter()
          .filter(|thread| !thread.resolved)
          .filter(|thread| comment_thread_latest_activity(thread) > workspace.comment_seen_stamp(thread.comment_id))
          .count();
        if workspace.unread_comment_count != count {
          workspace.unread_comment_count = count;
          cx.notify();
        }
      });
    })
    .detach();
  }

  pub(crate) fn comment_seen_stamp(&self, comment_id: u128) -> i64 {
    self.comment_last_seen.get(&comment_id).copied().unwrap_or(i64::MIN)
  }

  /// C-S5: the panel viewed these threads — record their latest activity as
  /// seen, kill the badge, and persist so the read-state survives restarts.
  pub(crate) fn mark_comment_threads_seen(&mut self, stamps: &[(u128, i64)], cx: &mut Context<Self>) {
    let mut changed = false;
    for (comment_id, stamp) in stamps {
      let entry = self.comment_last_seen.entry(*comment_id).or_insert(i64::MIN);
      if *stamp > *entry {
        *entry = *stamp;
        changed = true;
      }
    }
    if self.unread_comment_count != 0 {
      self.unread_comment_count = 0;
      cx.notify();
    }
    if changed {
      self.persist_temporary_workspace_session(cx);
    }
  }

  fn toggle_toolkit_tool(&mut self, tool: ToolkitTool, cx: &mut Context<Self>) {
    let was_expanded = self.active_toolkit_tool.is_some();
    self.active_toolkit_tool = if self.active_toolkit_tool == Some(tool) { None } else { Some(tool) };

    // C-S4: leaving the Comments tool ends review mode — the panel drops its
    // editor observation and clears the review marks.
    if self.active_toolkit_tool != Some(ToolkitTool::Comments)
      && let Some(panel) = self.comments_panel.clone()
    {
      panel.update(cx, |panel, cx| panel.detach(cx));
    }

    let is_expanded = self.active_toolkit_tool.is_some();
    if was_expanded != is_expanded {
      let delta = if is_expanded { px(40.0) - px(380.0) } else { px(380.0) - px(40.0) };
      self.prepare_active_editor_for_width_delta(delta, cx);
    }
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  fn prepare_active_editor_for_width_delta(&mut self, delta: Pixels, cx: &mut Context<Self>) {
    if delta == px(0.0) {
      return;
    }
    if let Some(editor) = self.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.prepare_for_workspace_width_delta(delta, cx));
    }
  }

  fn refresh_outline_tree(&mut self, cx: &mut Context<Self>) {
    let Some(active_id) = self.active_document_id else {
      if self.outline_cache.is_some() {
        self.outline_cache = None;
        self
          .outline_tree
          .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
      }
      return;
    };
    let Some(editor) = &self.active_editor else {
      self.outline_cache = None;
      self
        .outline_tree
        .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
      return;
    };
    let editor = editor.read(cx);
    let edit_generation = editor.edit_generation();
    if self.outline_cache.as_ref().is_some_and(|cache| {
      cache.document_id == active_id && cache.edit_generation == edit_generation && cache.visible_revision == self.outline_revision
    }) {
      return;
    }
    if let Some(cache) = self
      .outline_cache
      .as_mut()
      .filter(|cache| cache.document_id == active_id)
    {
      if cache.edit_generation != edit_generation {
        let structure_changed = cache.update_signature(editor.document(), edit_generation);
        if !structure_changed && cache.visible_revision == self.outline_revision {
          return;
        }
      }
    } else {
      self.outline_cache = Some(OutlineCache::new(active_id, edit_generation, outline_signature(editor.document())));
    }
    let Some(cache) = self.outline_cache.as_mut() else {
      return;
    };
    if cache.visible_revision != self.outline_revision {
      cache.rebuild_visible(self.outline_revision, &self.collapsed_outline_items);
    }
    let items = cache.tree_items.clone();
    self
      .outline_tree
      .update(cx, |tree, cx| tree.set_items(items, cx));
    if let Some(active_paragraph) = self.outline_active_paragraph_for_viewport(self.outline_viewport_paragraph) {
      self.outline_active_paragraph = Some(active_paragraph);
    }
  }

  pub fn scroll_active_editor_to_paragraph(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = &self.active_editor {
      editor.update(cx, |editor, cx| editor.scroll_to_paragraph(paragraph_ix, window, cx));
    }
  }

  /// O-S2 peek: scroll + arrival flash, the caret stays where the user left
  /// it. Single-click semantics for outline/tub navigation.
  pub fn peek_active_editor_paragraph(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = &self.active_editor {
      editor.update(cx, |editor, cx| {
        editor.peek_paragraph(paragraph_ix, crate::rich_text_element::DEFAULT_JUMP_FLASH_RGB, window, cx);
      });
    }
  }

  /// O-S2 land: move the caret to the paragraph start, scroll, focus the
  /// editor. Double-click/Enter semantics.
  pub fn land_active_editor_on_paragraph(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = &self.active_editor {
      editor.update(cx, |editor, cx| {
        editor.set_selection(
          crate::rich_text_element::EditorSelection::collapsed(crate::rich_text_element::DocumentOffset {
            paragraph: paragraph_ix,
            byte: 0,
          }),
          cx,
        );
        editor.scroll_to_paragraph(paragraph_ix, window, cx);
        editor.focus_handle(cx).focus(window);
      });
    }
  }

  fn save_current_outline_state(&mut self, cx: &mut Context<Self>) {
    let Some(active_id) = self.active_document_id else { return };
    let Some(panel) = self
      .document_panels
      .iter()
      .find(|p| p.read(cx).id() == active_id)
    else {
      return;
    };
    panel.update(cx, |panel, _| {
      panel.collapsed_outline_items = Some(self.collapsed_outline_items.clone());
      panel.outline_revision = self.outline_revision;
      panel.outline_scrolled_paragraph = self.outline_scrolled_paragraph;
    });
  }

  fn restore_outline_state_for_document(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    let Some(panel) = self
      .document_panels
      .iter()
      .find(|p| p.read(cx).id() == panel_id)
    else {
      return;
    };
    let panel = panel.read(cx);
    match &panel.collapsed_outline_items {
      Some(items) => self.collapsed_outline_items = items.clone(),
      None => {
        if let Some(editor) = self.active_editor.as_ref() {
          let editor = editor.read(cx);
          let signature = outline_signature(editor.document());
          self.collapsed_outline_items = signature
            .entries
            .iter()
            .filter(|entry| entry.level == 2)
            .map(|entry| entry.paragraph_ix)
            .collect();
        }
      },
    }
    self.outline_revision = panel.outline_revision.wrapping_add(1);
    self.outline_scrolled_paragraph = panel.outline_scrolled_paragraph;
    self.outline_viewport_paragraph = self.active_editor_viewport_paragraph(cx);
    self.outline_active_paragraph = None;
  }

  fn toggle_outline_item(&mut self, paragraph_ix: usize, cx: &mut Context<Self>) {
    if !self.collapsed_outline_items.insert(paragraph_ix) {
      self.collapsed_outline_items.remove(&paragraph_ix);
    }
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.refresh_outline_tree(cx);
    self.save_current_outline_state(cx);
    cx.notify();
  }

  pub(super) fn toggle_outline_level(&mut self, level: usize, cx: &mut Context<Self>) {
    let Some(editor) = self.active_editor.as_ref() else {
      return;
    };
    let editor = editor.read(cx);
    let signature = outline_signature(editor.document());
    let target_entries: HashSet<usize> = signature
      .entries
      .iter()
      .filter(|entry| entry.level == level)
      .map(|entry| entry.paragraph_ix)
      .collect();

    if target_entries.is_empty() {
      return;
    }

    let any_expanded = target_entries
      .iter()
      .any(|ix| !self.collapsed_outline_items.contains(ix));
    if any_expanded {
      self.collapsed_outline_items.extend(target_entries);
    } else {
      for ix in target_entries {
        self.collapsed_outline_items.remove(&ix);
      }
    }
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.refresh_outline_tree(cx);
    self.save_current_outline_state(cx);
    cx.notify();
  }

  pub(super) fn show_outline_context_menu(
    &mut self,
    level: usize,
    paragraph_ix: Option<usize>,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    let workspace = cx.entity().downgrade();
    let menu = PopupMenu::build(window, cx, move |menu, _, _| {
      let toggle_workspace = workspace.clone();
      let expand_workspace = workspace.clone();
      let collapse_workspace = workspace.clone();
      let copy_workspace = workspace.clone();
      let select_workspace = workspace.clone();
      let comment_workspace = workspace.clone();
      let speech_workspace = workspace.clone();
      let siblings_workspace = workspace.clone();
      let move_up_workspace = workspace.clone();
      let move_down_workspace = workspace.clone();
      menu
        .min_w(px(180.0))
        .item(
          PopupMenuItem::new(format!("Toggle all {}", outline_level_plural(level))).on_click(move |_, _, cx| {
            let _ = toggle_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              workspace.toggle_outline_level(level, cx);
              cx.notify();
            });
          }),
        )
        // O-S4: whole-tree fold controls.
        .item(PopupMenuItem::new("Expand all").on_click(move |_, _, cx| {
          let _ = expand_workspace.update(cx, |workspace, cx| {
            workspace.outline_context_menu = None;
            workspace.expand_all_outline_items(cx);
          });
        }))
        .item(PopupMenuItem::new("Collapse all").on_click(move |_, _, cx| {
          let _ = collapse_workspace.update(cx, |workspace, cx| {
            workspace.outline_context_menu = None;
            workspace.collapse_all_outline_items(cx);
          });
        }))
        // O-S4: copy the section's text (heading + everything under it).
        .item(
          PopupMenuItem::new("Copy section text").on_click(move |_, _, cx| {
            let _ = copy_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.copy_outline_section_text(paragraph_ix, cx);
              }
            });
          }),
        )
        // O-S6: the section toolkit.
        .item(
          PopupMenuItem::new("Select in editor").on_click(move |_, window, cx| {
            let _ = select_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.select_outline_section_in_editor(paragraph_ix, window, cx);
              }
            });
          }),
        )
        .item(
          PopupMenuItem::new("Comment on section").on_click(move |_, window, cx| {
            let _ = comment_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.comment_on_outline_section(paragraph_ix, window, cx);
              }
            });
          }),
        )
        .item(
          PopupMenuItem::new("Send section to speech").on_click(move |_, window, cx| {
            let _ = speech_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.send_outline_section_to_speech(paragraph_ix, window, cx);
              }
            });
          }),
        )
        // O-S5: whole-section restructure (menu-driven; the Living Grid drag
        // rides the sidebar drag machinery when it lands).
        .item(
          PopupMenuItem::new("Move section up").on_click(move |_, _, cx| {
            let _ = move_up_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.move_outline_section(paragraph_ix, true, cx);
              }
            });
          }),
        )
        .item(
          PopupMenuItem::new("Move section down").on_click(move |_, _, cx| {
            let _ = move_down_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.move_outline_section(paragraph_ix, false, cx);
              }
            });
          }),
        )
        .item(
          PopupMenuItem::new("Collapse siblings").on_click(move |_, _, cx| {
            let _ = siblings_workspace.update(cx, |workspace, cx| {
              workspace.outline_context_menu = None;
              if let Some(paragraph_ix) = paragraph_ix {
                workspace.collapse_outline_siblings(paragraph_ix, cx);
              }
            });
          }),
        )
    });

    let _subscription = cx.subscribe(&menu, |workspace, _, _: &DismissEvent, cx| {
      workspace.outline_context_menu = None;
      cx.notify();
    });

    self.outline_context_menu = Some(OutlineContextMenu {
      position,
      menu_view: menu,
      _subscription,
    });
    cx.notify();
  }

  /// O-S4: unfold everything.
  pub(super) fn expand_all_outline_items(&mut self, cx: &mut Context<Self>) {
    if self.collapsed_outline_items.is_empty() {
      return;
    }
    self.collapsed_outline_items.clear();
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.refresh_outline_tree(cx);
    self.save_current_outline_state(cx);
    cx.notify();
  }

  /// O-S4: fold every heading that has children.
  pub(super) fn collapse_all_outline_items(&mut self, cx: &mut Context<Self>) {
    let Some(cache) = self.outline_cache.as_ref() else { return };
    let folders: Vec<usize> = outline_folder_paragraphs(&cache.nodes);
    if folders.is_empty() {
      return;
    }
    self.collapsed_outline_items.extend(folders);
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.refresh_outline_tree(cx);
    self.save_current_outline_state(cx);
    cx.notify();
  }

  /// O-S4: copy a section (heading through the last paragraph before the next
  /// heading at the same-or-higher level) to the clipboard as plain text.
  /// O-S4/O-S6: a heading's section extent — the heading through the last
  /// paragraph before the next same-or-higher heading.
  pub(super) fn outline_section_range(&self, paragraph_ix: usize, cx: &App) -> Option<(usize, usize)> {
    let editor = self.active_editor.as_ref()?;
    let cache = self.outline_cache.as_ref()?;
    let level = cache.levels.get(&paragraph_ix).copied()?;
    let end = cache
      .levels
      .iter()
      .filter(|(ix, lvl)| **ix > paragraph_ix && **lvl <= level)
      .map(|(ix, _)| *ix)
      .min()
      .unwrap_or(editor.read(cx).document().paragraphs.len());
    Some((paragraph_ix, end))
  }

  pub(super) fn copy_outline_section_text(&mut self, paragraph_ix: usize, cx: &mut Context<Self>) {
    let Some((paragraph_ix, end)) = self.outline_section_range(paragraph_ix, cx) else { return };
    let Some(editor) = self.active_editor.as_ref() else { return };
    let editor = editor.read(cx);
    let document = editor.document();
    let text: Vec<String> = (paragraph_ix..end)
      .filter_map(|ix| {
        document
          .paragraphs
          .get(ix)
          .map(|_| flowstate_document::paragraph_text(document, ix).to_string())
      })
      .collect();
    if text.is_empty() {
      return;
    }
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.join("\n")));
  }

  /// O-S6: select the whole section in the editor (and focus it).
  pub(super) fn select_outline_section_in_editor(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some((start, end)) = self.outline_section_range(paragraph_ix, cx) else { return };
    let Some(editor) = self.active_editor.clone() else { return };
    editor.update(cx, |editor, cx| {
      let last_paragraph = end.saturating_sub(1);
      let end_byte = editor
        .document()
        .paragraphs
        .get(last_paragraph)
        .map_or(0, crate::rich_text_element::paragraph_text_len);
      editor.set_selection(
        crate::rich_text_element::EditorSelection::range(
          crate::rich_text_element::DocumentOffset {
            paragraph: start,
            byte: 0,
          },
          crate::rich_text_element::DocumentOffset {
            paragraph: last_paragraph,
            byte: end_byte,
          },
        ),
        cx,
      );
      editor.scroll_to_paragraph(start, window, cx);
      editor.focus_handle(cx).focus(window);
    });
  }

  /// O-S6: comment on the section — select it, open the comments rail; the
  /// composer reads the live selection (C-S3's law).
  pub(super) fn comment_on_outline_section(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    self.select_outline_section_in_editor(paragraph_ix, window, cx);
    self.open_comments_panel(window, cx);
  }

  /// O-S6: send the section to the speech document via the existing path.
  pub(super) fn send_outline_section_to_speech(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    self.select_outline_section_in_editor(paragraph_ix, window, cx);
    self.send_selection_to_speech_document(window, cx);
  }

  /// O-S5: move a section above its previous sibling / below its next one.
  /// Sibling = the nearest same-level heading with no higher-level heading
  /// between (stays inside the same parent).
  pub(super) fn move_outline_section(&mut self, paragraph_ix: usize, up: bool, cx: &mut Context<Self>) {
    let Some((start, end)) = self.outline_section_range(paragraph_ix, cx) else { return };
    let Some(cache) = self.outline_cache.as_ref() else { return };
    let Some(level) = cache.levels.get(&paragraph_ix).copied() else { return };
    let target = if up {
      // The previous same-level sibling's start.
      let mut candidate = None;
      for (ix, lvl) in cache.levels.iter() {
        if *ix >= start {
          continue;
        }
        if *lvl < level && candidate.is_some_and(|best| *ix > best) {
          // A higher-level heading between us and the candidate: parent
          // boundary crossed — the candidate is not a sibling.
          candidate = None;
        }
        if *lvl == level && candidate.is_none_or(|best| *ix > best) {
          candidate = Some(*ix);
        }
      }
      candidate
    } else {
      // The end of the NEXT same-level sibling's section.
      let next_sibling = cache
        .levels
        .iter()
        .filter(|(ix, lvl)| **ix >= end && **lvl <= level)
        .min_by_key(|(ix, _)| **ix)
        .filter(|(_, lvl)| **lvl == level)
        .map(|(ix, _)| *ix);
      next_sibling.and_then(|sibling| self.outline_section_range(sibling, cx).map(|(_, sibling_end)| sibling_end))
    };
    let Some(target) = target else { return };
    if let Some(editor) = self.active_editor.clone() {
      editor.update(cx, |editor, cx| {
        editor.move_paragraph_range(start, end, target, cx);
      });
    }
  }

  /// O-S6: collapse every sibling heading at this level, leaving this one
  /// open — the "focus on my section" fold.
  pub(super) fn collapse_outline_siblings(&mut self, paragraph_ix: usize, cx: &mut Context<Self>) {
    let Some(cache) = self.outline_cache.as_ref() else { return };
    let Some(level) = cache.levels.get(&paragraph_ix).copied() else { return };
    let siblings: Vec<usize> = cache
      .levels
      .iter()
      .filter(|(ix, lvl)| **lvl == level && **ix != paragraph_ix)
      .map(|(ix, _)| *ix)
      .collect();
    if siblings.is_empty() {
      return;
    }
    self.collapsed_outline_items.extend(siblings);
    self.collapsed_outline_items.remove(&paragraph_ix);
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.refresh_outline_tree(cx);
    self.save_current_outline_state(cx);
    cx.notify();
  }

  /// M2: the editor context menu — the verb layer everywhere, with the top
  /// of the menu changing by hit target. Every item routes through the same
  /// dispatch the keybindings/palette use (parity law); items land in the
  /// shared anchored-menu slot the outline menu uses.
  pub fn show_editor_context_menu(
    &mut self,
    position: Point<Pixels>,
    target: crate::rich_text_element::EditorContextTarget,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    use crate::rich_text_element::EditorContextTarget;
    let workspace = cx.entity().downgrade();
    let editor = self.active_editor.clone();
    let menu = PopupMenu::build(window, cx, move |menu, _, _| {
      let close = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
        workspace.outline_context_menu = None;
        cx.notify();
      };
      let with_editor = move |action: fn(&mut RichTextEditor, &mut Context<RichTextEditor>)| {
        let editor = editor.clone();
        move |workspace: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>| {
          workspace.outline_context_menu = None;
          if let Some(editor) = editor.clone() {
            editor.update(cx, action);
          }
          cx.notify();
        }
      };
      match target {
        EditorContextTarget::Text {
          has_selection,
          over_annotation,
          ..
        } => {
          let menu = menu
            .min_w(px(220.0))
            .when(over_annotation, |menu| {
              menu.item(command_menu_item(workspace.clone(), "Open Thread", None, false, |workspace, window, cx| {
                workspace.outline_context_menu = None;
                // The right-click placed the caret inside the marked span;
                // the panel's reverse link highlights the thread.
                workspace.open_comments_panel(window, cx);
                cx.notify();
              }))
            })
            .item(command_menu_item(
              workspace.clone(),
              if has_selection { "Comment on Selection" } else { "Add General Note" },
              Some(crate::commands::CommandId::OpenComments),
              false,
              |workspace, window, cx| {
                workspace.outline_context_menu = None;
                workspace.open_comments_panel(window, cx);
                cx.notify();
              },
            ))
            .item(command_menu_item(
              workspace.clone(),
              "Send to Speech",
              None,
              !has_selection,
              |workspace, window, cx| {
                workspace.outline_context_menu = None;
                workspace.send_selection_to_speech_document(window, cx);
                cx.notify();
              },
            ))
            .separator();
          menu
            .item(command_menu_item(
              workspace.clone(),
              "Cut",
              Some(crate::commands::CommandId::Cut),
              !has_selection,
              with_editor(|editor, cx| editor.cut(cx)),
            ))
            .item(command_menu_item(
              workspace.clone(),
              "Copy",
              Some(crate::commands::CommandId::Copy),
              !has_selection,
              with_editor(|editor, cx| editor.copy(cx)),
            ))
            .item(command_menu_item(
              workspace.clone(),
              "Paste",
              Some(crate::commands::CommandId::Paste),
              false,
              with_editor(|editor, cx| editor.paste(cx)),
            ))
            .item(command_menu_item(
              workspace.clone(),
              "Copy as Plain Text",
              None,
              !has_selection,
              with_editor(|editor, cx| editor.copy_selection_as_plain_text(cx)),
            ))
        },
        EditorContextTarget::Image { .. } => menu
          .min_w(px(200.0))
          // B-S10: the dead trio ships — alt text and alignment had complete
          // intent/CRDT/fuzz plumbing and zero UI callers.
          .item({
            let alt_workspace = workspace.clone();
            PopupMenuItem::new("Edit Alt Text…").on_click(move |_, window, cx| {
              let _ = alt_workspace.update(cx, |workspace, cx| workspace.open_alt_text_editor(window, cx));
            })
          })
          .item(command_menu_item(workspace.clone(), "Align Left", None, false, with_editor(|editor, cx| {
            editor.set_selected_image_alignment(crate::rich_text_element::BlockAlignment::Left, cx);
          })))
          .item(command_menu_item(workspace.clone(), "Align Center", None, false, with_editor(|editor, cx| {
            editor.set_selected_image_alignment(crate::rich_text_element::BlockAlignment::Center, cx);
          })))
          .item(command_menu_item(workspace.clone(), "Align Right", None, false, with_editor(|editor, cx| {
            editor.set_selected_image_alignment(crate::rich_text_element::BlockAlignment::Right, cx);
          })))
          .separator()
          .item(command_menu_item(workspace.clone(), "Fit Width", None, false, with_editor(|editor, cx| {
            editor.set_selected_image_fit_width(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Intrinsic Size", None, false, with_editor(|editor, cx| {
            editor.set_selected_image_intrinsic_size(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Widen", None, false, with_editor(|editor, cx| {
            editor.widen_selected_image(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Narrow", None, false, with_editor(|editor, cx| {
            editor.narrow_selected_image(cx);
          })))
          .separator()
          // Cut, not a bare delete: removal with a clipboard safety net.
          .item(command_menu_item(workspace.clone(), "Remove Image", None, false, with_editor(|editor, cx| {
            editor.cut(cx);
          }))),
        EditorContextTarget::Table { .. } => menu
          .min_w(px(220.0))
          .item(command_menu_item(workspace.clone(), "Insert Row After", None, false, with_editor(|editor, cx| {
            editor.insert_row_after_selected_table(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Insert Column After", None, false, with_editor(|editor, cx| {
            editor.insert_column_after_selected_table(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Delete Row", None, false, with_editor(|editor, cx| {
            editor.delete_row_from_selected_table(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Delete Column", None, false, with_editor(|editor, cx| {
            editor.delete_column_from_selected_table(cx);
          })))
          .separator()
          // B-S7: the buried SetCellSpan op finally gets its UI.
          .item(command_menu_item(workspace.clone(), "Merge Selected Cells", None, false, with_editor(|editor, cx| {
            editor.merge_cell_range(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Split Cell", None, false, with_editor(|editor, cx| {
            editor.split_selected_cell(cx);
          })))
          .separator()
          .item(command_menu_item(workspace.clone(), "Widen Column", None, false, with_editor(|editor, cx| {
            editor.widen_selected_table_column(cx);
          })))
          .item(command_menu_item(workspace.clone(), "Narrow Column", None, false, with_editor(|editor, cx| {
            editor.narrow_selected_table_column(cx);
          }))),
        EditorContextTarget::Equation { .. } => menu.min_w(px(200.0)).item(command_menu_item(
          workspace.clone(),
          "Copy Equation Source",
          None,
          false,
          with_editor(|editor, cx| {
            // B-S1: copies the LaTeX itself, not a rich block fragment.
            editor.copy_equation_source(cx);
          }),
        )),
      }
      .separator()
      .item({
        let workspace = workspace.clone();
        PopupMenuItem::new("Dismiss").on_click(move |_, _, cx| {
          let _ = workspace.update(cx, |workspace, cx| close(workspace, cx));
        })
      })
    });

    let _subscription = cx.subscribe(&menu, |workspace, _, _: &DismissEvent, cx| {
      workspace.outline_context_menu = None;
      cx.notify();
    });
    self.outline_context_menu = Some(OutlineContextMenu {
      position,
      menu_view: menu,
      _subscription,
    });
    cx.notify();
  }

  pub fn dirty_editors(&self, cx: &App) -> Vec<Entity<RichTextEditor>> {
    self
      .document_panels
      .iter()
      .filter_map(|panel| {
        let editor = panel.read(cx).editor();
        editor.read(cx).has_unsaved_changes().then_some(editor)
      })
      .collect()
  }

  fn dirty_panels(&self, cx: &App) -> Vec<PanelKind> {
    let mut panels = self
      .document_panels
      .iter()
      .filter_map(|panel| {
        let panel_state = panel.read(cx);
        if !panel_state.is_dirty(cx) {
          return None;
        }
        Some(PanelKind::Document {
          panel: panel.clone(),
          editor: panel_state.editor(),
        })
      })
      .collect::<Vec<_>>();
    panels.extend(self.flow_panels.iter().filter_map(|panel| {
      let panel_state = panel.read(cx);
      if !panel_state.is_dirty(cx) {
        return None;
      }
      Some(PanelKind::Flow {
        panel: panel.clone(),
        editor: panel_state.editor(),
      })
    }));
    panels
  }

  /// W-S4: pull the mirrors (`active_document_id`/`active_editor`/
  /// `active_flow`) from the tree's focused pane. The tree is the source;
  /// everything else reads the mirrors it always read.
  pub(crate) fn sync_active_from_tree(&mut self, cx: &mut Context<Self>) {
    match self.pane_tree.focused_active() {
      Some(id) => {
        if self.active_document_id != Some(id) {
          self.activate_document_id(id, cx);
        }
      },
      None => {
        if self.active_document_id.is_some() {
          self.save_current_outline_state(cx);
          self.active_document_id = None;
          self.active_editor = None;
          self.active_flow = None;
          self.outline_cache = None;
          self.refresh_outline_tree(cx);
        }
      },
    }
    cx.notify();
  }

  /// W-S4: split the focused pane; its active tab MOVES into the new pane
  /// ("split right with this tab"), which takes focus.
  pub(crate) fn split_focused_pane(&mut self, axis: SplitAxis, cx: &mut Context<Self>) {
    let focused = self.pane_tree.focused;
    if self.pane_tree.split(focused, axis).is_some() {
      self.sync_active_from_tree(cx);
      self.persist_temporary_workspace_session(cx);
    }
  }

  /// W-S4: close the focused pane — surviving tabs move to the neighbor;
  /// documents never close implicitly. The last pane refuses out loud.
  pub(crate) fn close_focused_pane(&mut self, cx: &mut Context<Self>) {
    let focused = self.pane_tree.focused;
    if self.pane_tree.close_pane(focused) {
      self.sync_active_from_tree(cx);
      self.persist_temporary_workspace_session(cx);
    } else {
      self.report_failure("This is the only pane — close tabs instead.", None, cx);
    }
  }

  /// W-S4: cycle focus through panes in layout order.
  pub(crate) fn focus_next_pane(&mut self, cx: &mut Context<Self>) {
    if self.pane_tree.pane_count() > 1 {
      self.pane_tree.focus_next();
      self.sync_active_from_tree(cx);
    }
  }

  /// W-S4: focus a specific pane (tab-strip clicks in unfocused panes).
  pub(crate) fn focus_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
    if self.pane_tree.focused != pane && self.pane_tree.leaf(pane).is_some() {
      self.pane_tree.focused = pane;
      self.sync_active_from_tree(cx);
    }
  }

  fn activate_document_id(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    // W-S4: activating a tab focuses its owning pane (one doc, one pane).
    self.pane_tree.activate_tab(panel_id);
    self.save_current_outline_state(cx);
    let editor = self
      .document_panels
      .iter()
      .find(|p| p.read(cx).id() == panel_id)
      .map(|p| p.read(cx).editor());
    if let Some(editor) = editor {
      self.active_document_id = Some(panel_id);
      self.active_editor = Some(editor);
      self.active_flow = None;
      self.outline_cache = None;
      self.restore_outline_state_for_document(panel_id, cx);
      self.refresh_outline_tree(cx);
      self.persist_temporary_workspace_session(cx);
      cx.notify();
      return;
    }
    if let Some(panel) = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      self.active_document_id = Some(panel_id);
      self.active_editor = None;
      self.active_flow = Some(panel.read(cx).editor());
      self.outline_cache = None;
      self.outline_viewport_paragraph = None;
      self.outline_active_paragraph = None;
      self.outline_scrolled_paragraph = None;
      self.persist_temporary_workspace_session(cx);
      cx.notify();
    }
  }

  fn activate_document_at_index(&mut self, index: usize, cx: &mut Context<Self>) {
    let panel_id = self.document_tabs(cx).get(index).map(|tab| tab.id);
    if let Some(panel_id) = panel_id {
      self.activate_document_id(panel_id, cx);
    }
  }

  fn navigate_active_tab(&mut self, offset: isize, cx: &mut Context<Self>) {
    let tabs = self.document_tabs(cx);
    let Some(active_id) = self.active_document_id else {
      return;
    };
    let Some(active_index) = tabs.iter().position(|tab| tab.id == active_id) else {
      return;
    };
    let len = tabs.len();
    if len == 0 {
      return;
    }
    let target = if offset.is_negative() {
      // usize::MAX % len (the wrapping_sub shortcut) only wraps correctly
      // when len is a power of two; add len before subtracting instead.
      (active_index + len - (offset.unsigned_abs() % len)) % len
    } else {
      (active_index + offset as usize) % len
    };
    let target_id = tabs[target].id;
    self.activate_document_id(target_id, cx);
    // W-S4: scroll the OWNING pane's strip to the activated tab.
    if let Some(pane) = self.pane_tree.pane_of(target_id)
      && let Some(leaf) = self.pane_tree.leaf(pane)
      && let Some(handle) = self.pane_tab_scrolls.get(&pane.0)
      && let Some(ix) = leaf.tab_order.iter().position(|tab| *tab == target_id)
    {
      handle.scroll_to_item(ix);
    }
  }

  fn toggle_active_tab_pin(&mut self, cx: &mut Context<Self>) {
    let Some(active_id) = self.active_document_id else {
      return;
    };
    if let Some(ix) = self
      .pinned_document_ids
      .iter()
      .position(|id| *id == active_id)
    {
      self.pinned_document_ids.remove(ix);
    } else if self.pinned_document_ids.len() < 10 {
      self.pinned_document_ids.push(active_id);
    }
    cx.notify();
    self.persist_temporary_workspace_session(cx);
  }

  /// B-S10: open the inline alt-text editor prefilled with the selected
  /// image's current description.
  pub(crate) fn open_alt_text_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(editor) = self.active_editor.clone() else {
      return;
    };
    let Some(current) = editor.read(cx).selected_image_alt_text() else {
      self.report_failure("Select an image first — alt text lives on the image.", None, cx);
      return;
    };
    let input = cx.new(|cx| {
      InputState::new(window, cx)
        .default_value(current.to_string())
        .placeholder("Describe the image (screen readers + exported descr)")
    });
    cx.subscribe_in(&input, window, move |workspace: &mut Self, input, event: &InputEvent, _, cx| {
      if matches!(event, InputEvent::PressEnter { .. }) {
        let value = input.read(cx).value().to_string();
        if let Some(editor) = workspace.active_editor.clone() {
          editor.update(cx, |editor, cx| editor.set_selected_image_alt_text(value, cx));
        }
        workspace.alt_text_editor = None;
        cx.notify();
      }
    })
    .detach();
    input.update(cx, |input, cx| input.focus(window, cx));
    self.alt_text_editor = Some(input);
    cx.notify();
  }

  pub(crate) fn close_alt_text_editor(&mut self, cx: &mut Context<Self>) {
    if self.alt_text_editor.take().is_some() {
      cx.notify();
    }
  }

  /// Unix milliseconds — the speech-target marker's cross-doc tiebreaker.
  fn unix_time_ms() -> i64 {
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
  }

  pub(crate) fn toggle_speech_document(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    // CT-S1: the flow-speech trap dies here. A flow used to accept the "S"
    // designation and then silently swallow every send (sends only look up
    // document panels) — the badge was a lie. Refuse out loud instead.
    if self.flow_panels.iter().any(|panel| panel.read(cx).id() == panel_id) {
      self.report_failure(
        "A flow can't be the speech document — cards are sent into a rich-text document.",
        None,
        cx,
      );
      return;
    }
    let designating = self.speech_document_id != Some(panel_id);
    self.speech_document_id = designating.then_some(panel_id);
    cx.notify();
    self.persist_temporary_workspace_session(cx);
    // CT-S3 (CT5-B): the designation is the doc's replicated SELF-MARKER —
    // every peer's backtick targets the same doc and every peer's tab wears
    // the "S". The write is undo-inert and rides the normal publish queue;
    // solo docs carry the marker too (same path solo and collab).
    let Some(io) = self.document_runtimes.get(&panel_id).cloned() else {
      return;
    };
    // Monotonic per workspace: a re-designation in the same millisecond as
    // the previous one must still read as NEWER, or the cross-doc winner
    // rule flips a coin (real double-toggles and headless tests both hit
    // millisecond ties).
    let designated_at_ms = Self::unix_time_ms().max(self.last_speech_designation_ms + 1);
    self.last_speech_designation_ms = designated_at_ms;
    let workspace = cx.entity().downgrade();
    cx.spawn(async move |_, cx| {
      let (user_id, display_name) = cx
        .background_executor()
        .spawn(async { crate::app_settings::load_local_user_identity() })
        .await;
      let marker = flowstate_document::loro_schema::SpeechTargetMarker {
        active: designating,
        designated_by: display_name.unwrap_or_else(|| user_id.to_string()),
        designated_at_ms,
      };
      if let Err(error) = io.set_speech_target(marker).await {
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.report_failure(format!("Marking the speech document for the session failed: {error:#}"), None, cx);
        });
      }
    })
    .detach();
  }

  /// CT-S3: re-read every open doc's speech-target self-marker (debounced —
  /// editor activity includes remote imports, which is how a peer's
  /// designation arrives). The open doc with the greatest `designated_at_ms`
  /// wins the local designation; open losers get their markers cleared so
  /// the session converges on ONE speech doc.
  pub(crate) fn schedule_speech_target_reconcile(&mut self, cx: &mut Context<Self>) {
    if self.speech_target_reconcile_pending {
      return;
    }
    self.speech_target_reconcile_pending = true;
    cx.spawn(async move |workspace, cx| {
      cx.background_executor()
        .timer(std::time::Duration::from_millis(400))
        .await;
      let Ok(Some((runtimes, generation))) = workspace.update(cx, |workspace, cx| {
        workspace.speech_target_reconcile_pending = false;
        workspace.speech_target_reconcile_generation = workspace.speech_target_reconcile_generation.wrapping_add(1);
        let runtimes: Vec<(Uuid, flowstate_collab::doc_io::DocIoHandle)> = workspace
          .document_panels
          .iter()
          .filter_map(|panel| {
            let id = panel.read(cx).id();
            workspace
              .document_runtimes
              .get(&id)
              .cloned()
              .map(|io| (id, io))
          })
          .collect();
        (!runtimes.is_empty()).then_some((runtimes, workspace.speech_target_reconcile_generation))
      }) else {
        return;
      };
      let mut marked: Vec<(Uuid, flowstate_collab::doc_io::DocIoHandle, flowstate_document::loro_schema::SpeechTargetMarker)> = Vec::new();
      let mut cleared: Vec<Uuid> = Vec::new();
      for (id, io) in runtimes {
        match io.speech_target().await {
          Ok(Some(marker)) if marker.active => marked.push((id, io, marker)),
          Ok(Some(_)) => cleared.push(id),
          _ => {},
        }
      }
      // Winner = greatest (designated_at_ms, designated_by) — deterministic
      // on every peer, so everyone clears the same losers.
      let winner = marked
        .iter()
        .max_by(|a, b| {
          (a.2.designated_at_ms, a.2.designated_by.as_str()).cmp(&(b.2.designated_at_ms, b.2.designated_by.as_str()))
        })
        .map(|(id, _, marker)| (*id, marker.designated_at_ms));
      for (id, io, marker) in &marked {
        if Some(*id) != winner.map(|(id, _)| id) {
          // Clear with the LOSER's own identity/timestamp so racing peers
          // write the identical register value (idempotent convergence).
          let clear = flowstate_document::loro_schema::SpeechTargetMarker {
            active: false,
            designated_by: marker.designated_by.clone(),
            designated_at_ms: marker.designated_at_ms,
          };
          let _ = io.set_speech_target(clear).await;
        }
      }
      let _ = workspace.update(cx, |workspace, cx| {
        if workspace.speech_target_reconcile_generation != generation {
          return;
        }
        let Some((winner, winner_at)) = winner else {
          // No marked doc is open. A designation whose doc carries an
          // explicitly-cleared marker was toggled off (possibly by a peer) —
          // drop it. A doc with NO marker at all keeps any local (legacy
          // session-restored) designation.
          if let Some(current) = workspace.speech_document_id
            && cleared.contains(&current)
          {
            workspace.speech_document_id = None;
            cx.notify();
            workspace.persist_temporary_workspace_session(cx);
          }
          return;
        };
        // Track the observed maximum so this workspace's next designation
        // outbids it even inside the same millisecond.
        workspace.last_speech_designation_ms = workspace.last_speech_designation_ms.max(winner_at);
        if workspace.speech_document_id != Some(winner) {
          workspace.speech_document_id = Some(winner);
          cx.notify();
          workspace.persist_temporary_workspace_session(cx);
        }
      });
    })
    .detach();
  }

  fn toggle_tab_pin(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    if let Some(ix) = self
      .pinned_document_ids
      .iter()
      .position(|id| *id == panel_id)
    {
      self.pinned_document_ids.remove(ix);
    } else if self.pinned_document_ids.len() < 10 {
      self.pinned_document_ids.push(panel_id);
    }
    cx.notify();
    self.persist_temporary_workspace_session(cx);
  }

  fn activate_tab_shortcut(&mut self, index: usize, cx: &mut Context<Self>) {
    // §perf: build the set of live panel ids once instead of rebuilding the entire
    // labeled tab Vec for every pinned id. A tab exists for each document/flow panel,
    // so membership in this set is equivalent to matching some tab.id.
    let live_ids: FxHashSet<Uuid> = self
      .document_panels
      .iter()
      .map(|panel| panel.read(cx).id())
      .chain(self.flow_panels.iter().map(|panel| panel.read(cx).id()))
      .collect();
    let pinned = self
      .pinned_document_ids
      .iter()
      .copied()
      .filter(|id| live_ids.contains(id))
      .collect::<Vec<_>>();
    if let Some(id) = pinned.get(index).copied() {
      self.activate_document_id(id, cx);
    } else if pinned.is_empty() {
      self.activate_document_at_index(index, cx);
    }
  }

  fn condense_active_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(editor) = self.active_editor.clone() else {
      return false;
    };
    editor.update(cx, |editor, cx| {
      if !editor.focus_handle(cx).is_focused(window) {
        return false;
      }
      let Some(fragment) = editor.fragment_at_selection_or_enclosing_section(flowstate_document::CARD_BOUNDARY_STYLE_SLOTS) else {
        return false;
      };
      // CT-S4 (CT4-A): pilcrow-join is the DEFAULT — condense is always
      // invertible (uncondense splits on the marker). Space-join survives
      // only as the ribbon's explicit "Condense flat (irreversible)".
      let separator = crate::ribbon::CONDENSE_PILCROW_MARKER;
      let paragraphs = if editor.selection().is_caret() {
        condense_card_fragment_paragraphs(fragment.paragraphs, separator)
      } else {
        condense_fragment_paragraphs(fragment.paragraphs, separator)
      };
      if paragraphs.is_empty() {
        return false;
      }
      editor.replace_selection_or_enclosing_section_with_paragraphs(paragraphs, flowstate_document::CARD_BOUNDARY_STYLE_SLOTS, cx);
      true
    })
  }

  fn empty_input_paragraph_with_style(style: ParagraphStyle) -> InputParagraph {
    InputParagraph {
      style,
      runs: vec![InputRun {
        text: String::new(),
        styles: crate::rich_text_element::RunStyles::default(),
      }],
    }
  }

  fn wrap_with_newline_paragraphs(mut paragraphs: Vec<InputParagraph>, target_style: ParagraphStyle) -> Vec<InputParagraph> {
    let mut wrapped = Vec::with_capacity(paragraphs.len() + 2);
    wrapped.push(Self::empty_input_paragraph_with_style(target_style));
    wrapped.append(&mut paragraphs);
    wrapped.push(Self::empty_input_paragraph_with_style(target_style));
    wrapped
  }

  /// CT-S1: the send guards refuse OUT LOUD. `None` = refused (already
  /// reported); `Some` = (source, target) editors ready.
  fn speech_send_editors(&mut self, cx: &mut Context<Self>) -> Option<(Entity<RichTextEditor>, Entity<RichTextEditor>)> {
    let Some(speech_document_id) = self.speech_document_id else {
      self.report_failure(
        "No speech document is set — right-click a document tab (or use the ribbon) and mark it as the speech document first.",
        None,
        cx,
      );
      return None;
    };
    if self.active_document_id == Some(speech_document_id) {
      self.report_failure("This IS the speech document — send from the document you're cutting.", None, cx);
      return None;
    }
    let source_editor = self.active_editor.clone()?;
    let Some(speech_editor) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == speech_document_id)
      .map(|panel| panel.read(cx).editor())
    else {
      self.report_failure(
        "The speech document's tab is gone — mark another document as the speech document.",
        None,
        cx,
      );
      return None;
    };
    Some((source_editor, speech_editor))
  }

  pub(crate) fn send_selection_to_speech_document(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some((source_editor, speech_editor)) = self.speech_send_editors(cx) else {
      return false;
    };
    let fragment = source_editor.update(cx, |editor, cx| {
      editor
        .speech_send_fragment_at_selection_or_hover(flowstate_document::CARD_BOUNDARY_STYLE_SLOTS, window, cx)
        .unwrap_or_else(|| selected_fragment_or_enclosing_section(editor.document(), editor.selection()))
    });
    if fragment.paragraphs.is_empty() && fragment.blocks.is_empty() {
      return false;
    }
    speech_editor.update(cx, |editor, cx| {
      // CT-S2 (CT5-B): appends serialize at DOCUMENT END — the old
      // caret-line-end landing raced under concurrent team sends.
      editor.move_document_end(cx);
      let target_style = editor.caret_paragraph_style();
      let paragraphs = Self::wrap_with_newline_paragraphs(fragment.paragraphs, target_style);
      editor.insert_toolkit_text_at_caret(paragraphs, cx);
    });
    self.note_speech_send(cx);
    true
  }

  /// CT-S2 (CT2-A as amended by Adam): success feedback is the badge — a
  /// transient sent-count on the speech tab. NO activity-log line ("that's
  /// clutter"); refusals remain exceptional and may speak.
  fn note_speech_send(&mut self, cx: &mut Context<Self>) {
    self.speech_sent_recent = self.speech_sent_recent.saturating_add(1);
    self.speech_sent_clear_generation = self.speech_sent_clear_generation.wrapping_add(1);
    let generation = self.speech_sent_clear_generation;
    cx.spawn(async move |workspace, cx| {
      cx.background_executor()
        .timer(std::time::Duration::from_millis(2500))
        .await;
      let _ = workspace.update(cx, |workspace, cx| {
        if workspace.speech_sent_clear_generation == generation {
          workspace.speech_sent_recent = 0;
          cx.notify();
        }
      });
    })
    .detach();
    cx.notify();
  }

  pub(crate) fn send_selection_to_speech_document_end(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some((source_editor, speech_editor)) = self.speech_send_editors(cx) else {
      return false;
    };
    let fragment = source_editor.update(cx, |editor, cx| {
      editor
        .speech_send_fragment_at_selection_or_hover(flowstate_document::CARD_BOUNDARY_STYLE_SLOTS, window, cx)
        .unwrap_or_else(|| selected_fragment_or_enclosing_section(editor.document(), editor.selection()))
    });
    if fragment.paragraphs.is_empty() && fragment.blocks.is_empty() {
      return false;
    }
    speech_editor.update(cx, |editor, cx| {
      editor.move_document_end(cx);
      let target_style = editor.caret_paragraph_style();
      let paragraphs = Self::wrap_with_newline_paragraphs(fragment.paragraphs, target_style);
      editor.insert_toolkit_text_at_caret(paragraphs, cx);
    });
    self.note_speech_send(cx);
    true
  }

  fn apply_document_theme_to_open_editors(&mut self, theme: DocumentTheme, cx: &mut Context<Self>) {
    for panel in &self.document_panels {
      let editor = panel.read(cx).editor();
      let theme = theme.clone();
      editor.update(cx, |editor, cx| {
        editor.update_document_theme(|document_theme| *document_theme = theme, cx);
      });
    }
    cx.notify();
  }

  /// CO-S2/CO-S3: the active rich-text document's save path (scope grants
  /// are minted against it).
  pub fn active_document_path(&self, cx: &App) -> Option<PathBuf> {
    self
      .active_editor
      .as_ref()
      .and_then(|editor| editor.read(cx).document_path().cloned())
  }

  /// TB-S2: what the drag area says — the active document's title, with an
  /// honest unsaved marker.
  pub(super) fn active_document_drag_title(&self, cx: &App) -> SharedString {
    let Some(active_id) = self.active_document_id else {
      return SharedString::default();
    };
    let title = self
      .document_panels
      .iter()
      .map(|panel| panel.read(cx))
      .find(|panel| panel.id() == active_id)
      .map(|panel| (panel.title_text(), panel.is_dirty(cx)))
      .or_else(|| {
        self
          .flow_panels
          .iter()
          .map(|panel| panel.read(cx))
          .find(|panel| panel.id() == active_id)
          .map(|panel| (panel.title_text(), panel.is_dirty(cx)))
      });
    match title {
      Some((title, true)) => format!("{title} — unsaved").into(),
      Some((title, false)) => title,
      None => SharedString::default(),
    }
  }

  /// TB-S3: move a tab left/right within its zone (pinned tabs reorder
  /// inside the pin list; unpinned tabs reorder in panel order). The pinned
  /// zone stays a zone — moves never cross the boundary.
  pub(super) fn move_document_tab(&mut self, panel_id: Uuid, delta: isize, cx: &mut Context<Self>) {
    if let Some(pin_ix) = self.pinned_document_ids.iter().position(|id| *id == panel_id) {
      let target = pin_ix.saturating_add_signed(delta).min(self.pinned_document_ids.len() - 1);
      if target != pin_ix {
        self.pinned_document_ids.swap(pin_ix, target);
        self.persist_temporary_workspace_session(cx);
        cx.notify();
      }
      return;
    }
    // Unpinned: reorder the underlying panel lists (documents then flows —
    // the tab strip shows them in that concatenated order).
    let doc_count = self.document_panels.len();
    let doc_ix = self.document_panels.iter().position(|panel| panel.read(cx).id() == panel_id);
    let flow_ix = self.flow_panels.iter().position(|panel| panel.read(cx).id() == panel_id);
    match (doc_ix, flow_ix) {
      (Some(ix), _) => {
        let target = ix.saturating_add_signed(delta).min(doc_count.saturating_sub(1));
        if target != ix {
          self.document_panels.swap(ix, target);
          self.persist_temporary_workspace_session(cx);
          cx.notify();
        }
      },
      (_, Some(ix)) => {
        let target = ix.saturating_add_signed(delta).min(self.flow_panels.len().saturating_sub(1));
        if target != ix {
          self.flow_panels.swap(ix, target);
          self.persist_temporary_workspace_session(cx);
          cx.notify();
        }
      },
      _ => {},
    }
  }

  /// TB-S3 tear-off: reopen a path-backed document in its own window, then
  /// close the tab here. W-S1: untitled tabs refuse OUT LOUD (the menu item is
  /// also disabled for them) — the old silent early-return was a lie.
  pub(super) fn tear_off_document_tab(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
    // W-S3: rich-text tabs move LIVE — the panel ENTITY (runtime, undo,
    // caret, collab session) transplants to the new window; nothing reloads
    // and pathless tabs move too. Flows still reopen by path (their live
    // handoff is the recorded follow-up).
    if self
      .document_panels
      .iter()
      .any(|panel| panel.read(cx).id() == panel_id)
    {
      let Some(handoff) = self.hand_off_document_panel(panel_id, cx) else {
        return;
      };
      let target = crate::workspace::open_workspace_window(None, cx);
      Self::deliver_handoff(target, handoff, cx);
      return;
    }
    let path = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .and_then(|panel| panel.read(cx).editor().read(cx).document_path().cloned());
    let Some(path) = path else {
      self.report_failure(
        "Save the flow first — an unsaved flow tab has no file to move to a new window.",
        None,
        cx,
      );
      return;
    };
    let _ = crate::workspace::open_workspace_window(Some(path), cx);
    self.close_document_panel(panel_id, window, cx);
  }

  /// W-S3: re-dock — move a live rich-text tab into an EXISTING window.
  pub(crate) fn move_document_tab_to_window(&mut self, panel_id: Uuid, target: WeakEntity<Workspace>, cx: &mut Context<Self>) {
    if !self
      .document_panels
      .iter()
      .any(|panel| panel.read(cx).id() == panel_id)
    {
      self.report_failure("Only rich-text tabs move between windows live (flows reopen by path).", None, cx);
      return;
    }
    let Some(handoff) = self.hand_off_document_panel(panel_id, cx) else {
      return;
    };
    Self::deliver_handoff(target, handoff, cx);
  }

  /// Adopt `handoff` inside `target`'s own window context (found through the
  /// live-window registry; entities are app-scoped but focus needs the
  /// adopting window).
  fn deliver_handoff(target: WeakEntity<Workspace>, handoff: DocumentPanelHandoff, cx: &mut gpui::App) {
    let Some(target_entity) = target.upgrade() else {
      tracing::warn!("live tab handoff dropped: the target window is gone");
      return;
    };
    let handle = crate::workspace::live_workspace_windows(cx)
      .into_iter()
      .find(|(_, workspace)| *workspace == target_entity)
      .map(|(handle, _)| handle);
    let Some(handle) = handle else {
      tracing::warn!("live tab handoff dropped: the target window is not registered");
      return;
    };
    let _ = handle.update(cx, |_, window, cx| {
      target_entity.update(cx, |workspace, cx| workspace.adopt_document_panel(handoff, window, cx));
    });
  }

  fn document_tabs(&self, cx: &App) -> Vec<DocumentTab> {
    let mut tabs = self
      .document_panels
      .iter()
      .map(|panel| {
        let panel = panel.read(cx);
        let title = panel.title_text();
        let dirty = panel.is_dirty(cx);
        let pathless = panel.editor().read(cx).document_path().is_none();
        let title = truncate_tab_title(&title, 32);
        let id = panel.id();
        DocumentTab {
          id,
          label: title.into(),
          active: Some(id) == self.active_document_id,
          pinned: false,
          pin_index: None,
          speech: self.speech_document_id == Some(id),
          dirty,
          pathless,
          flow: false,
        }
      })
      .collect::<Vec<_>>();
    tabs.extend(self.flow_panels.iter().map(|panel| {
      let panel = panel.read(cx);
      let title = panel.title_text();
      let dirty = panel.is_dirty(cx);
      let pathless = panel.editor().read(cx).document_path().is_none();
      let title = truncate_tab_title(&title, 32);
      let id = panel.id();
      DocumentTab {
        id,
        label: title.into(),
        active: Some(id) == self.active_document_id,
        pinned: false,
        pin_index: None,
        // CT-S1: a flow can never be the speech document (designation refuses),
        // so its tab never wears the badge — legacy state included.
        speech: false,
        dirty,
        pathless,
        flow: true,
      }
    }));
    ordered_document_tabs(tabs, &self.pinned_document_ids)
  }

  fn active_outline_paragraph(&self, _: &App) -> Option<usize> {
    self.outline_active_paragraph
  }

  /// O-S2 hybrid tracking: the outline follows the CARET while it is on
  /// screen, and the viewport once the user scrolls the caret away.
  fn active_editor_viewport_paragraph(&self, cx: &App) -> Option<usize> {
    let editor = self.active_editor.as_ref()?.read(cx);
    let caret_paragraph = editor.selection().head.paragraph;
    if let Some((top, bottom)) = editor.viewport_paragraph_range()
      && caret_paragraph >= top
      && caret_paragraph <= bottom
    {
      return Some(caret_paragraph);
    }
    editor.viewport_anchor_paragraph()
  }

  fn refresh_outline_viewport(&mut self, cx: &mut Context<Self>) {
    let viewport_paragraph = self.active_editor_viewport_paragraph(cx);
    self.update_outline_viewport_paragraph(viewport_paragraph, cx);
  }

  fn update_outline_viewport_paragraph(&mut self, viewport_paragraph: Option<usize>, cx: &mut Context<Self>) {
    let mut changed = false;
    if self.outline_viewport_paragraph != viewport_paragraph {
      self.outline_viewport_paragraph = viewport_paragraph;
      changed = true;
    }
    if let Some(active_paragraph) = self.outline_active_paragraph_for_viewport(viewport_paragraph)
      && self.outline_active_paragraph != Some(active_paragraph)
    {
      self.outline_active_paragraph = Some(active_paragraph);
      changed = true;
    }
    if changed {
      cx.notify();
    }
  }

  fn outline_active_paragraph_for_viewport(&self, viewport_paragraph: Option<usize>) -> Option<usize> {
    let viewport_paragraph = viewport_paragraph?;
    let cache = self.outline_cache.as_ref()?;
    active_visible_outline_paragraph_from_visible(&cache.visible_paragraphs, viewport_paragraph)
  }

  fn scroll_outline_item_into_view(&mut self, paragraph_ix: Option<usize>, cx: &mut Context<Self>) {
    let Some(paragraph_ix) = paragraph_ix else {
      return;
    };
    if self.outline_scrolled_paragraph == Some(paragraph_ix) {
      return;
    }
    let id = outline_item_id(paragraph_ix);
    self.outline_tree.update(cx, |tree, _| {
      if let Some(ix) = tree.item_index_by_id(&id) {
        tree.scroll_to_item(ix, gpui::ScrollStrategy::Center);
      }
    });
    self.outline_scrolled_paragraph = Some(paragraph_ix);
  }
}

fn ordered_document_tabs(mut tabs: Vec<DocumentTab>, pinned_document_ids: &[Uuid]) -> Vec<DocumentTab> {
  for tab in &mut tabs {
    tab.pin_index = pinned_document_ids
      .iter()
      .position(|pinned_id| *pinned_id == tab.id);
    tab.pinned = tab.pin_index.is_some();
  }
  tabs.sort_by_key(|tab| (tab.pin_index.is_none(), tab.pin_index.unwrap_or(usize::MAX)));
  tabs
}

fn pin_shortcut_label(pin_index: usize) -> Option<&'static str> {
  match pin_index {
    0 => Some("1"),
    1 => Some("2"),
    2 => Some("3"),
    3 => Some("4"),
    4 => Some("5"),
    5 => Some("6"),
    6 => Some("7"),
    7 => Some("8"),
    8 => Some("9"),
    9 => Some("0"),
    _ => None,
  }
}

fn condense_fragment_paragraphs(paragraphs: Vec<InputParagraph>, separator: char) -> Vec<InputParagraph> {
  condense_paragraph_group(paragraphs, separator)
    .map(|paragraph| {
      vec![
        paragraph,
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: Vec::new(),
        },
      ]
    })
    .unwrap_or_default()
}

fn condense_card_fragment_paragraphs(paragraphs: Vec<InputParagraph>, separator: char) -> Vec<InputParagraph> {
  let mut output = Vec::with_capacity(paragraphs.len());
  let mut group = Vec::new();
  let mut transformed_any = false;
  for paragraph in paragraphs {
    if card_paragraph_excluded_from_condense(&paragraph) {
      if !group.is_empty()
        && let Some(paragraph) = condense_paragraph_group(std::mem::take(&mut group), separator)
      {
        transformed_any = true;
        output.push(paragraph);
      }
      output.push(paragraph);
    } else {
      group.push(paragraph);
    }
  }
  if !group.is_empty()
    && let Some(paragraph) = condense_paragraph_group(group, separator)
  {
    transformed_any = true;
    output.push(paragraph);
  }
  if transformed_any { output } else { Vec::new() }
}

fn condense_paragraph_group(paragraphs: Vec<InputParagraph>, separator: char) -> Option<InputParagraph> {
  let mut runs = Vec::new();
  for paragraph in paragraphs {
    let mut paragraph_runs = paragraph
      .runs
      .into_iter()
      .filter(|run| !run.text.is_empty())
      .peekable();
    if paragraph_runs.peek().is_none() {
      continue;
    }
    if !runs.is_empty() {
      runs.push(InputRun {
        text: separator.to_string(),
        styles: crate::rich_text_element::RunStyles::default(),
      });
    }
    runs.extend(paragraph_runs);
  }
  (!runs.is_empty()).then_some(InputParagraph {
    style: ParagraphStyle::Normal,
    runs,
  })
}

fn card_paragraph_excluded_from_condense(paragraph: &InputParagraph) -> bool {
  paragraph.style == flowstate_document::PARAGRAPH_TAG
    || paragraph
      .runs
      .iter()
      .any(|run| run.styles.semantic == flowstate_document::SEMANTIC_CITE)
}

fn selected_fragment_or_enclosing_section(
  document: &DocumentProjection,
  selection: &crate::rich_text_element::EditorSelection,
) -> crate::rich_text_element::RichClipboardFragment {
  if selection.anchor != selection.head {
    return crate::rich_text_element::selected_rich_fragment(
      document,
      selection.anchor.min(selection.head)..selection.anchor.max(selection.head),
    );
  }
  let caret = selection.head;
  let (start_paragraph, end_paragraph_exclusive) = enclosing_section_bounds(document, caret.paragraph, flowstate_document::CARD_BOUNDARY_STYLE_SLOTS).unwrap_or((
    caret.paragraph,
    caret
      .paragraph
      .saturating_add(1)
      .min(document.paragraphs.len()),
  ));
  let end_paragraph = end_paragraph_exclusive.saturating_sub(1);
  crate::rich_text_element::selected_rich_fragment(
    document,
    crate::rich_text_element::DocumentOffset {
      paragraph: start_paragraph,
      byte: 0,
    }..crate::rich_text_element::DocumentOffset {
      paragraph: end_paragraph,
      byte: paragraph_byte_range(document, end_paragraph).len(),
    },
  )
}

fn enclosing_section_bounds(document: &DocumentProjection, paragraph_ix: usize, section_slots: &[u8]) -> Option<(usize, usize)> {
  document
    .outline
    .iter()
    .filter_map(|section| {
      let SectionKind::Custom(slot) = section.kind;
      if !section_slots.contains(&slot) {
        return None;
      }
      let start = paragraph_index_for_id(document, section.start_paragraph)?;
      let end = section
        .end_paragraph_exclusive
        .and_then(|id| paragraph_index_for_id(document, id))
        .unwrap_or(document.paragraphs.len());
      (start <= paragraph_ix && paragraph_ix < end).then_some((start, end))
    })
    .min_by_key(|(start, end)| end - start)
}

/// C-S5: a thread's newest activity stamp — thread metadata or any message,
/// whichever moved last. This is what "unread" is measured against.
fn comment_thread_latest_activity(thread: &flowstate_collab::crdt_runtime::RuntimeCommentThread) -> i64 {
  thread
    .messages
    .iter()
    .map(|message| message.updated_at_unix_secs.max(message.created_at_unix_secs))
    .fold(thread.updated_at_unix_secs.max(thread.created_at_unix_secs), i64::max)
}
