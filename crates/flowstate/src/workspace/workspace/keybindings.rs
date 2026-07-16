use crate::commands::FindInDocumentAction;
#[hotpath::measure]
fn workspace_command_for_keystroke(keystroke: &Keystroke) -> Option<CommandId> {
  crate::app_settings::load_keymap()
    .entries
    .iter()
    .find_map(|entry| {
      KeyBinding::load(&entry.key, Box::new(NoAction), None, false, None, &DummyKeyboardMapper)
        .ok()
        .and_then(|binding| binding.match_keystrokes(std::slice::from_ref(keystroke)))
        .is_some_and(|matched| !matched)
        .then_some(entry.command)
    })
}

fn tab_index(command: &CommandId) -> Option<usize> {
  match command {
    CommandId::SwitchToTab1 => Some(0),
    CommandId::SwitchToTab2 => Some(1),
    CommandId::SwitchToTab3 => Some(2),
    CommandId::SwitchToTab4 => Some(3),
    CommandId::SwitchToTab5 => Some(4),
    CommandId::SwitchToTab6 => Some(5),
    CommandId::SwitchToTab7 => Some(6),
    CommandId::SwitchToTab8 => Some(7),
    CommandId::SwitchToTab9 => Some(8),
    CommandId::SwitchToTab10 => Some(9),
    _ => None,
  }
}

#[hotpath::measure_all]
impl Workspace {
  fn handle_window_keybinding(&mut self, command: CommandId, window: &mut Window, cx: &mut Context<Self>) -> bool {
    if self.non_document_keybinding_surface_is_open() || self.focused_workspace_input_is_focused(window, cx) {
      return false;
    }
    if let Some(flow) = self.active_flow.clone() {
      let handled = match command {
        CommandId::Undo => {
          flow.update(cx, |editor, cx| editor.undo(cx));
          true
        },
        CommandId::Redo => {
          flow.update(cx, |editor, cx| editor.redo(cx));
          true
        },
        CommandId::InsertNewline => {
          flow.update(cx, |editor, cx| {
            editor.add_sibling(flowstate_flow::RelativePosition::After, cx);
            editor.focus_active_cell(window, cx);
          });
          true
        },
        CommandId::InsertSoftLineBreak => {
          flow.update(cx, |editor, cx| {
            editor.add_response(cx);
            editor.focus_active_cell(window, cx);
          });
          true
        },
        CommandId::FlowAddSiblingAbove => {
          flow.update(cx, |editor, cx| {
            editor.add_sibling(flowstate_flow::RelativePosition::Before, cx);
            editor.focus_active_cell(window, cx);
          });
          true
        },
        CommandId::Backspace if flow.read(cx).active_cell_is_empty() => {
          flow.update(cx, |editor, cx| editor.delete_selected(window, cx));
          true
        },
        CommandId::DeleteWordForward | CommandId::FlowDeleteSelected => {
          flow.update(cx, |editor, cx| editor.delete_selected(window, cx));
          true
        },
        CommandId::ToggleStrikethrough | CommandId::FlowStrike => {
          flow.update(cx, |editor, cx| editor.strike_selected(cx));
          true
        },
        CommandId::FlowNewFamily => {
          flow.update(cx, |editor, cx| {
            editor.add_new_family(cx);
            editor.focus_active_cell(window, cx);
          });
          true
        },
        CommandId::FlowNavigateUp | CommandId::FlowNavigateDown | CommandId::FlowNavigateLeft | CommandId::FlowNavigateRight => {
          let direction = match command {
            CommandId::FlowNavigateUp => crate::flow::editor::GridDirection::Up,
            CommandId::FlowNavigateDown => crate::flow::editor::GridDirection::Down,
            CommandId::FlowNavigateLeft => crate::flow::editor::GridDirection::Left,
            _ => crate::flow::editor::GridDirection::Right,
          };
          flow.update(cx, |editor, cx| editor.navigate(direction, cx));
          true
        },
        CommandId::FlowMoveUp | CommandId::FlowMoveDown | CommandId::FlowMoveLeft | CommandId::FlowMoveRight => {
          let direction = match command {
            CommandId::FlowMoveUp => crate::flow::editor::GridDirection::Up,
            CommandId::FlowMoveDown => crate::flow::editor::GridDirection::Down,
            CommandId::FlowMoveLeft => crate::flow::editor::GridDirection::Left,
            _ => crate::flow::editor::GridDirection::Right,
          };
          flow.update(cx, |editor, cx| editor.move_active_cell(direction, cx));
          true
        },
        _ => false,
      };
      if handled {
        return true;
      }
    }
    match command {
      CommandId::Save => {
        self.save_active(window, cx);
        true
      },
      CommandId::NewDocument => {
        self.new_document(window, cx);
        true
      },
      CommandId::OpenDocument => {
        self.prompt_open_document(window, cx);
        true
      },
      CommandId::CloseDocument => {
        self.close_active_document(window, cx);
        true
      },
      CommandId::ShareDocument => {
        self.open_collaboration_dialog(window, cx);
        true
      },
      CommandId::JoinSession => {
        self.open_join_collaboration_dialog(window, cx);
        true
      },
      CommandId::StartCollaboration => self.start_collaboration_on_active_document(cx).is_some(),
      CommandId::CopyCollaborationTicket => self.copy_active_collaboration_ticket(window, cx),
      CommandId::JoinCollaborationFromClipboard => self.join_collaboration_from_clipboard(window, cx),
      CommandId::LeaveCollaboration => self.confirm_leave_collaboration_on_active_document(window, cx),
      CommandId::FindInDocument => self.open_active_document_search_bar(window, cx),
      CommandId::OpenCommandPalette => {
        self.open_command_palette(window, cx);
        true
      },
      CommandId::OpenComments => {
        self.open_comments_panel(window, cx);
        true
      },
      CommandId::OpenHistory => {
        self.open_history_takeover(window, cx);
        true
      },
      CommandId::ToggleTubTool => {
        self.toggle_toolkit_tool(ToolkitTool::Tub, cx);
        true
      },
      CommandId::FocusTubSearch => {
        if self.active_toolkit_tool != Some(ToolkitTool::Tub) {
          self.toggle_toolkit_tool(ToolkitTool::Tub, cx);
        }
        self.toolkit_search_input.focus_handle(cx).focus(window);
        true
      },
      CommandId::SwapLeftNav => {
        self.left_nav_mode = match self.left_nav_mode {
          LeftNavMode::Outline => LeftNavMode::Tub,
          LeftNavMode::Tub => LeftNavMode::Outline,
        };
        self.persist_temporary_workspace_session(cx);
        cx.notify();
        true
      },
      CommandId::ZoomIn => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| editor.zoom_in(cx));
          true
        } else if let Some(flow) = self.active_flow.clone() {
          flow.update(cx, |flow, cx| flow.zoom_in(cx));
          true
        } else {
          false
        }
      },
      CommandId::ZoomOut => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| editor.zoom_out(cx));
          true
        } else if let Some(flow) = self.active_flow.clone() {
          flow.update(cx, |flow, cx| flow.zoom_out(cx));
          true
        } else {
          false
        }
      },
      CommandId::ToggleRibbon => {
        self.toggle_ribbon(cx);
        true
      },
      CommandId::NextTab => {
        self.navigate_active_tab(1, cx);
        true
      },
      CommandId::PreviousTab => {
        self.navigate_active_tab(-1, cx);
        true
      },
      CommandId::TogglePinTab => {
        self.toggle_active_tab_pin(cx);
        true
      },
      CommandId::SendToSpeechDocument => self.send_selection_to_speech_document(window, cx),
      CommandId::SendToSpeechDocumentEnd => self.send_selection_to_speech_document_end(window, cx),
      CommandId::CondenseSelection => self.condense_active_selection(window, cx),
      CommandId::CondensedSelection => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| {
            if !editor.selection().is_caret() || editor.focus_handle(cx).is_focused(window) {
              editor.toggle_inline_tool(ArmedInlineTool::Semantic(flowstate_document::SEMANTIC_CONDENSED), cx);
            }
          });
          true
        } else {
          false
        }
      },
      CommandId::MarkCard => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| {
            if editor.focus_handle(cx).is_focused(window) {
              editor.set_highlight_from_caret_to_enclosing_section_end(flowstate_document::HIGHLIGHT_MARKED, &[0, 1, 2, 3], cx);
            }
          });
          true
        } else {
          false
        }
      },
      CommandId::ToggleSpeechDocument => {
        if let Some(panel_id) = self.active_document_id {
          self.toggle_speech_document(panel_id, cx);
          true
        } else {
          false
        }
      },
      CommandId::ExportFormat => {
        if let Some(editor) = self.active_editor.clone() {
          let task = editor.update(cx, |editor, cx| {
            editor.export_document_format(crate::rich_text_element::DocumentExportFormat::Docx, cx)
          });
          cx.spawn(async move |_, _| {
            if let Err(error) = task.await {
              eprintln!("format export failed: {error}");
            }
          })
          .detach();
          true
        } else {
          false
        }
      },
      CommandId::ExportSend => {
        if let Some(editor) = self.active_editor.clone() {
          let task = editor.update(cx, |editor, cx| {
            editor.send_document(crate::rich_text_element::DocumentExportFormat::Docx, cx)
          });
          cx.spawn(async move |_, _| {
            if let Err(error) = task.await {
              eprintln!("send export failed: {error}");
            }
          })
          .detach();
          true
        } else {
          false
        }
      },
      CommandId::ToggleInvisibility => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| editor.toggle_invisibility_mode(cx));
          true
        } else {
          false
        }
      },
      command if tab_index(&command).is_some() => {
        self.activate_tab_shortcut(tab_index(&command).unwrap(), cx);
        true
      },
      CommandId::ScrollToParagraph => false,
      CommandId::FlowAddSiblingAbove | CommandId::FlowDeleteSelected | CommandId::FlowStrike => false,
      command => {
        if let Some(editor) = self.active_editor.clone() {
          if let Some(command) = crate::rich_text_element::flowstate_command_to_rich_text(command) {
            editor.update(cx, |editor, cx| editor.dispatch_window_command(command, window, cx));
            true
          } else {
            false
          }
        } else {
          false
        }
      },
    }
  }

  fn on_find_in_document(&mut self, _: &FindInDocumentAction, window: &mut Window, cx: &mut Context<Self>) {
    self.open_active_document_search_bar(window, cx);
  }

  fn on_fidelity_mark(&mut self, _: &crate::commands::FidelityMarkAction, _window: &mut Window, _cx: &mut Context<Self>) {
    flowstate_fidelity::marker("keybinding");
  }

  fn open_active_document_search_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(active_document_id) = self.active_document_id else {
      return false;
    };
    self.open_document_search_bar(active_document_id, window, cx)
  }

  pub(super) fn open_document_search_bar(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .cloned()
    else {
      return false;
    };

    panel.update(cx, |panel, cx| panel.open_search_bar(window, cx));
    cx.notify();
    true
  }

  fn non_document_keybinding_surface_is_open(&self) -> bool {
    self.settings_overlay.is_some()
      || self.file_search_overlay.is_some()
      || self.collaboration_dialog.is_some()
  }

  fn focused_workspace_input_is_focused(&self, window: &Window, cx: &App) -> bool {
    self
      .toolkit_search_input
      .read(cx)
      .focus_handle(cx)
      .is_focused(window)
      || self
        .tub_file_search_input
        .read(cx)
        .focus_handle(cx)
        .is_focused(window)
      || self
        .file_search_overlay
        .as_ref()
        .is_some_and(|overlay| overlay.read(cx).focus_handle(cx).is_focused(window))
      || self
        .collaboration_dialog
        .as_ref()
        .is_some_and(|dialog| dialog.read(cx).focus_handle(cx).is_focused(window))
      || self
        .document_panels
        .iter()
        .any(|panel| panel.read(cx).search_bar_focused(window, cx))
  }
}
