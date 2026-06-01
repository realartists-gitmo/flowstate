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
    if self.focused_workspace_input_is_focused(window, cx) {
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
  }
}
