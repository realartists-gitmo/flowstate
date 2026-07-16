/// P5-S6: export/import the custom keymap as a JSON exchange file.
fn keymap_exchange_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, _, _cx| {
    let export_workspace = workspace.clone();
    let import_workspace = workspace.clone();
    h_flex()
      .gap_2()
      .child(
        Button::new("keymap-export")
          .xsmall()
          .label("Export keymap…")
          .on_click(move |_, _, cx| {
            let _ = export_workspace.update(cx, |workspace, cx| workspace.export_keymap(cx));
          }),
      )
      .child(
        Button::new("keymap-import")
          .xsmall()
          .label("Import keymap…")
          .on_click(move |_, _, cx| {
            let _ = import_workspace.update(cx, |workspace, cx| workspace.import_keymap(cx));
          }),
      )
      .into_any_element()
  })
}

fn keymap_editor_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, window, cx| {
    div()
      .w_full()
      .flex()
      .flex_col()
      .overflow_hidden()
      .rounded_md()
      .border_1()
      .border_color(cx.theme().border)
      .child(keymap_table_header(cx))
      .children(
        crate::commands::RIBBON_KEYMAP_COMMANDS
          .iter()
          .filter_map(|command| crate::commands::command_spec(*command))
          .enumerate()
          .map(|(ix, spec)| render_keymap_row(workspace.clone(), spec, ix, window, cx)),
      )
      .into_any_element()
  })
}

fn keymap_table_header(cx: &App) -> AnyElement {
  h_flex()
    .w_full()
    .items_center()
    .gap_3()
    .px_3()
    .py_2()
    .bg(cx.theme().secondary)
    .border_b_1()
    .border_color(cx.theme().border)
    .child(
      div()
        .w_48()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Command"),
    )
    .child(
      div()
        .flex_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Shortcut"),
    )
    .into_any_element()
}

fn render_keymap_row(
  workspace: WeakEntity<Workspace>,
  spec: &'static crate::commands::CommandSpec,
  row_ix: usize,
  window: &mut Window,
  cx: &mut App,
) -> AnyElement {
  let current = crate::commands::active_keys_for(spec.id).join(", ");
  let ui_generation = workspace
    .upgrade()
    .map_or(0, |workspace| workspace.read(cx).keymap_ui_generation);
  let state = window.use_keyed_state(SharedString::from(format!("keymap-command-{:?}-{ui_generation}", spec.id)), cx, {
    let workspace = workspace.clone();
    move |window, cx| {
      let input = cx.new(|cx| {
        InputState::new(window, cx)
          .default_value(current.clone())
          .placeholder("unbound")
      });
      let _subscription = cx.subscribe_in(&input, window, move |state: &mut KeymapInputState, input, event: &InputEvent, _, cx| {
        if !matches!(event, InputEvent::Change) {
          return;
        }
        let value = input.read(cx).value().to_string();
        if value == state.initial_value {
          return;
        }
        state.initial_value = value.clone();
        update_keymap_command(cx, &workspace, spec.id, value);
      });
      KeymapInputState {
        input,
        initial_value: current.clone(),
        _subscription,
      }
    }
  });
  let input = state.read(cx).input.clone();
  let binding_error = workspace
    .upgrade()
    .and_then(|workspace| workspace.read(cx).keymap_errors.get(&spec.id).cloned());
  let recording = workspace
    .upgrade()
    .is_some_and(|workspace| workspace.read(cx).keymap_recording == Some(spec.id));
  let conflicts = workspace
    .upgrade()
    .and_then(|workspace| workspace.read(cx).keymap_conflicts.get(&spec.id).cloned())
    .unwrap_or_default();

  h_flex()
    .w_full()
    .items_center()
    .gap_3()
    .px_3()
    .py_2()
    .border_b_1()
    .border_color(cx.theme().border)
    .when(row_ix % 2 == 1, |this| this.bg(cx.theme().secondary.opacity(0.35)))
    .child(div().w_48().min_w_0().text_sm().child(spec.label))
    .child(
      v_flex()
        .flex_1()
        .min_w_0()
        .gap_0p5()
        .child(
          h_flex()
            .gap_1()
            .items_center()
            .child(Input::new(&input).w_full())
            // P5-S6: press-to-record — the next chord pressed lands here.
            .child({
              let record_workspace = workspace.clone();
              Button::new(SharedString::from(format!("keymap-record-{:?}", spec.id)))
                .xsmall()
                .when(recording, |this| this.primary())
                .when(!recording, |this| this.ghost())
                .label(if recording { "Press keys…" } else { "Record" })
                .tooltip("Press the shortcut to bind; Escape cancels")
                .on_click(move |_, _, cx| {
                  let _ = record_workspace.update(cx, |workspace, cx| {
                    workspace.keymap_recording = if workspace.keymap_recording == Some(spec.id) {
                      None
                    } else {
                      Some(spec.id)
                    };
                    cx.notify();
                  });
                })
            }),
        )
        .when_some(binding_error, |this, error| {
          this.child(div().text_xs().text_color(cx.theme().danger).child(error))
        })
        // P5-S6: conflicts speak, with a one-click steal.
        .children(conflicts.into_iter().map(|(key, other)| {
          let other_label = crate::commands::command_spec(other).map_or("another command", |spec| spec.label);
          let steal_workspace = workspace.clone();
          let steal_key = key.clone();
          h_flex()
            .gap_1()
            .items_center()
            .child(
              div()
                .text_xs()
                .text_color(cx.theme().warning)
                .child(format!("{key} is also bound to {other_label}")),
            )
            .child(
              Button::new(SharedString::from(format!("keymap-steal-{:?}-{key}", spec.id)))
                .xsmall()
                .ghost()
                .label("Steal")
                .on_click(move |_, _, cx| {
                  let _ = steal_workspace.update(cx, |workspace, cx| {
                    workspace.steal_keymap_binding(spec.id, other, steal_key.clone(), cx);
                  });
                }),
            )
        })),
    )
    .into_any_element()
}

fn update_keymap_command(cx: &mut App, workspace: &WeakEntity<Workspace>, command: crate::commands::CommandId, value: String) {
  let mut keymap = crate::app_settings::load_keymap();
  keymap.entries.retain(|entry| entry.command != command);
  let context = crate::commands::command_spec(command).and_then(|spec| spec.context.map(str::to_string));
  let mut invalid_keys: Vec<String> = Vec::new();
  for key in value
    .split(',')
    .map(str::trim)
    .filter(|key| !key.is_empty())
  {
    if gpui::KeyBinding::load(key, Box::new(gpui::NoAction), None, false, None, &gpui::DummyKeyboardMapper).is_err() {
      invalid_keys.push(key.to_string());
      continue;
    }
    keymap.entries.push(crate::commands::KeymapEntry {
      command,
      key: key.to_string(),
      context: context.clone(),
    });
  }
  // P5-S6: conflict detection — the same key on another command is flagged
  // under the row with a one-click steal.
  let new_keys: Vec<String> = keymap
    .entries
    .iter()
    .filter(|entry| entry.command == command)
    .map(|entry| entry.key.clone())
    .collect();
  let conflicts: Vec<(String, crate::commands::CommandId)> = keymap
    .entries
    .iter()
    .filter(|entry| entry.command != command && new_keys.contains(&entry.key))
    .map(|entry| (entry.key.clone(), entry.command))
    .collect();
  // P5-S1 (Law 2): parse failures render inline under the row instead of
  // dying in stderr while the binding silently drops.
  let _ = workspace.update(cx, |workspace, cx| {
    if conflicts.is_empty() {
      workspace.keymap_conflicts.remove(&command);
    } else {
      workspace.keymap_conflicts.insert(command, conflicts);
    }
    if invalid_keys.is_empty() {
      if workspace.keymap_errors.remove(&command).is_some() {
        cx.notify();
      }
    } else {
      workspace
        .keymap_errors
        .insert(command, format!("Not a valid binding: {}", invalid_keys.join(", ")));
      cx.notify();
    }
  });
  let entries = keymap.entries.clone();
  save_setting_reporting(workspace.clone(), "the keymap", move || crate::app_settings::save_keymap_entries(entries), cx);
  let _ = workspace.update(cx, |_, cx| cx.notify());
}


#[hotpath::measure_all]
impl Workspace {
  /// P5-S6: a chord pressed while a row is recording becomes its binding.
  /// Escape cancels; bare modifier presses are ignored.
  pub(super) fn handle_keymap_recording_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) -> bool {
    let Some(command) = self.keymap_recording else { return false };
    let key = event.keystroke.key.as_str();
    if key == "escape" {
      self.keymap_recording = None;
      cx.notify();
      return true;
    }
    if matches!(key, "shift" | "control" | "alt" | "platform" | "function" | "capslock" | "") {
      return true;
    }
    let binding = event.keystroke.unparse();
    self.keymap_recording = None;
    let workspace = cx.entity().downgrade();
    update_keymap_command(cx, &workspace, command, binding);
    self.keymap_ui_generation = self.keymap_ui_generation.wrapping_add(1);
    cx.notify();
    true
  }

  /// P5-S6: take a conflicted key away from the other command.
  pub(super) fn steal_keymap_binding(
    &mut self,
    winner: crate::commands::CommandId,
    loser: crate::commands::CommandId,
    key: String,
    cx: &mut Context<Self>,
  ) {
    let mut keymap = crate::app_settings::load_keymap();
    keymap
      .entries
      .retain(|entry| !(entry.command == loser && entry.key == key));
    let entries = keymap.entries.clone();
    let workspace = cx.entity().downgrade();
    save_setting_reporting(workspace, "the keymap", move || crate::app_settings::save_keymap_entries(entries), cx);
    self.keymap_conflicts.remove(&winner);
    self.keymap_ui_generation = self.keymap_ui_generation.wrapping_add(1);
    cx.notify();
  }

  /// P5-S6 export: the custom keymap entries as a JSON file.
  pub(super) fn export_keymap(&mut self, cx: &mut Context<Self>) {
    let entries = crate::app_settings::load_keymap_entries();
    let receiver = cx.prompt_for_new_path(std::path::Path::new(""), Some("flowstate-keymap.json"));
    cx.spawn(async move |workspace, cx| {
      let Ok(Ok(Some(path))) = receiver.await else { return };
      let result = serde_json::to_vec_pretty(&entries)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&path, bytes));
      let _ = workspace.update(cx, |workspace, cx| match result {
        Ok(()) => workspace.report_activity("Keymap exported", cx),
        Err(error) => workspace.report_failure(format!("Exporting the keymap failed: {error}"), None, cx),
      });
    })
    .detach();
  }

  /// P5-S6 import: replace the custom entries with a previously exported file
  /// (each binding re-validated on load).
  pub(super) fn import_keymap(&mut self, cx: &mut Context<Self>) {
    let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Import".into()),
    });
    cx.spawn(async move |workspace, cx| {
      let Ok(Ok(Some(paths))) = receiver.await else { return };
      let Some(path) = paths.first().cloned() else { return };
      let loaded: Result<Vec<crate::commands::KeymapEntry>, String> = std::fs::read(&path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));
      let _ = workspace.update(cx, |workspace, cx| match loaded {
        Ok(entries) => {
          let invalid: Vec<&str> = entries
            .iter()
            .filter(|entry| {
              gpui::KeyBinding::load(&entry.key, Box::new(gpui::NoAction), None, false, None, &gpui::DummyKeyboardMapper).is_err()
            })
            .map(|entry| entry.key.as_str())
            .collect();
          if !invalid.is_empty() {
            workspace.report_failure(format!("Keymap import refused: invalid bindings ({})", invalid.join(", ")), None, cx);
            return;
          }
          let to_save = entries.clone();
          let weak = cx.entity().downgrade();
          save_setting_reporting(weak, "the keymap", move || crate::app_settings::save_keymap_entries(to_save), cx);
          workspace.keymap_ui_generation = workspace.keymap_ui_generation.wrapping_add(1);
          workspace.report_activity("Keymap imported", cx);
          cx.notify();
        },
        Err(error) => workspace.report_failure(format!("Importing the keymap failed: {error}"), None, cx),
      });
    })
    .detach();
  }
}
