// P4-S5/P5-S3: the omni palette's action registry. Lives inside the
// workspace module so it can reach the private command dispatch and the
// settings update paths; the palette overlay itself is a sibling module that
// executes through these.

/// One palette entry's action. Commands route through THE dispatch
/// (`handle_window_keybinding`); settings toggles route through the same
/// update paths the settings UI uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaletteAction {
  Command(CommandId),
  ToggleAutosave,
  ToggleSmartWordSelection,
  ToggleReduceMotion,
  /// R10-A: the go-to navigation provider — peek a heading's paragraph.
  GoToParagraph(usize),
}

/// A palette entry: display label, the action, and the shortcut hint.
#[derive(Clone, Debug)]
pub(crate) struct PaletteEntry {
  pub(crate) label: String,
  pub(crate) action: PaletteAction,
  pub(crate) shortcut: Option<String>,
}

impl Workspace {
  /// Every action the palette can run: all registered commands plus the
  /// settings quick-toggles (S3 decision). Labels for toggles show the state
  /// they will SET, so the entry reads as the outcome.
  pub(crate) fn palette_entries(&self, cx: &App) -> Vec<PaletteEntry> {
    let mut entries: Vec<PaletteEntry> = crate::commands::COMMAND_SPECS
      .iter()
      .filter(|spec| spec.id != CommandId::OpenCommandPalette)
      .map(|spec| PaletteEntry {
        label: spec.label.to_string(),
        action: PaletteAction::Command(spec.id),
        shortcut: crate::commands::active_keys_for(spec.id).first().cloned(),
      })
      .collect();

    let settings = crate::app_settings::load_app_settings();
    entries.push(PaletteEntry {
      label: if settings.editor.autosave {
        "Autosave: turn off".to_string()
      } else {
        "Autosave: turn on".to_string()
      },
      action: PaletteAction::ToggleAutosave,
      shortcut: None,
    });
    entries.push(PaletteEntry {
      label: if settings.editor.smart_word_selection {
        "Smart word selection: turn off".to_string()
      } else {
        "Smart word selection: turn on".to_string()
      },
      action: PaletteAction::ToggleSmartWordSelection,
      shortcut: None,
    });
    entries.push(PaletteEntry {
      label: if settings.editor.reduce_motion {
        "Reduce motion: turn off".to_string()
      } else {
        "Reduce motion: turn on".to_string()
      },
      action: PaletteAction::ToggleReduceMotion,
      shortcut: None,
    });
    // R10-A: the go-to navigation provider — every heading in the active
    // document, peekable by fuzzy title. ScrollToParagraph finally has a
    // caller.
    if let Some(editor) = &self.active_editor {
      let document = editor.read(cx).document();
      for node in document.outline.iter() {
        let Some(paragraph_ix) = paragraph_index_for_id(document, node.heading_paragraph) else {
          continue;
        };
        let heading = crate::rich_text_element::paragraph_text(document, paragraph_ix);
        let heading = heading.trim();
        if heading.is_empty() {
          continue;
        }
        let mut label = format!("Go to: {heading}");
        if label.len() > 72 {
          label.truncate(69);
          label.push('…');
        }
        entries.push(PaletteEntry {
          label,
          action: PaletteAction::GoToParagraph(paragraph_ix),
          shortcut: None,
        });
      }
    }

    entries
  }

  pub(crate) fn execute_palette_action(&mut self, action: PaletteAction, window: &mut Window, cx: &mut Context<Self>) {
    match action {
      PaletteAction::Command(command) => {
        self.handle_window_keybinding(command, window, cx);
      },
      PaletteAction::ToggleAutosave => {
        let enabled = !crate::app_settings::load_app_settings().editor.autosave;
        let workspace = cx.entity().downgrade();
        update_autosave(cx, &workspace, enabled);
      },
      PaletteAction::ToggleSmartWordSelection => {
        let enabled = !crate::app_settings::load_app_settings().editor.smart_word_selection;
        let workspace = cx.entity().downgrade();
        update_smart_word_selection(cx, &workspace, enabled);
      },
      PaletteAction::GoToParagraph(paragraph_ix) => {
        self.peek_active_editor_paragraph(paragraph_ix, window, cx);
      },
      PaletteAction::ToggleReduceMotion => {
        let enabled = !crate::app_settings::load_app_settings().editor.reduce_motion;
        save_setting_reporting(
          cx.entity().downgrade(),
          "the reduce-motion setting",
          move || crate::app_settings::save_reduce_motion(enabled),
          cx,
        );
      },
    }
  }

  pub fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(palette) = self.command_palette.clone() {
      palette.update(cx, |palette, cx| palette.focus_search(window, cx));
      return;
    }
    let workspace = cx.entity().downgrade();
    let palette = cx.new(|cx| crate::workspace::command_palette::CommandPalette::new(workspace, window, cx));
    palette.update(cx, |palette, cx| palette.focus_search(window, cx));
    self.command_palette = Some(palette);
    cx.notify();
  }

  pub fn close_command_palette(&mut self, cx: &mut Context<Self>) {
    self.command_palette = None;
    cx.notify();
  }
}
