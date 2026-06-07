use crate::commands::FindInDocumentAction;
#[hotpath::measure]
fn workspace_command_for_keystroke(keystroke: &Keystroke) -> Option<CommandId> {
  COMMAND_SPECS.iter().find_map(|spec| {
    spec
      .default_keys
      .iter()
      .any(|key| {
        KeyBinding::load(key, Box::new(NoAction), None, false, None, &DummyKeyboardMapper)
          .ok()
          .and_then(|binding| binding.match_keystrokes(std::slice::from_ref(keystroke)))
          == Some(false)
      })
      .then_some(spec.id)
  })
}

#[hotpath::measure_all]
impl Workspace {
  fn handle_window_keybinding(&mut self, command: CommandId, window: &mut Window, cx: &mut Context<Self>) -> bool {
    if self.non_document_keybinding_surface_is_open() || self.focused_workspace_input_is_focused(window, cx) {
      return false;
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
      CommandId::FindInDocument => self.open_active_document_search_bar(window, cx),
      CommandId::ZoomIn => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| editor.zoom_in(cx));
          true
        } else {
          false
        }
      },
      CommandId::ZoomOut => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| editor.zoom_out(cx));
          true
        } else {
          false
        }
      },
      CommandId::ToggleRibbon => {
        self.ribbon_collapsed = !self.ribbon_collapsed;
        cx.notify();
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
      CommandId::CondenseSelection => self.condense_active_selection(cx),
      CommandId::MarkCard => {
        if let Some(editor) = self.active_editor.clone() {
          editor.update(cx, |editor, cx| {
            editor.set_highlight_from_caret_to_enclosing_section_end(
              flowstate_document::HIGHLIGHT_MARKED,
              &[0, 1, 2, 3],
              cx,
            );
          });
          true
        } else {
          false
        }
      },
      CommandId::SwitchToTab1 => {
        self.activate_tab_shortcut(0, cx);
        true
      },
      CommandId::SwitchToTab2 => {
        self.activate_tab_shortcut(1, cx);
        true
      },
      CommandId::SwitchToTab3 => {
        self.activate_tab_shortcut(2, cx);
        true
      },
      CommandId::SwitchToTab4 => {
        self.activate_tab_shortcut(3, cx);
        true
      },
      CommandId::SwitchToTab5 => {
        self.activate_tab_shortcut(4, cx);
        true
      },
      CommandId::SwitchToTab6 => {
        self.activate_tab_shortcut(5, cx);
        true
      },
      CommandId::SwitchToTab7 => {
        self.activate_tab_shortcut(6, cx);
        true
      },
      CommandId::SwitchToTab8 => {
        self.activate_tab_shortcut(7, cx);
        true
      },
      CommandId::SwitchToTab9 => {
        self.activate_tab_shortcut(8, cx);
        true
      },
      CommandId::SwitchToTab10 => {
        self.activate_tab_shortcut(9, cx);
        true
      },
      CommandId::ScrollToParagraph => false,
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
    self.settings_overlay.is_some() || self.file_search_overlay.is_some()
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
        .document_panels
        .iter()
        .any(|panel| panel.read(cx).search_bar_focused(window, cx))
  }
}
