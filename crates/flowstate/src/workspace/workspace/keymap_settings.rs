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
  let state = window.use_keyed_state(SharedString::from(format!("keymap-command-{:?}", spec.id)), cx, {
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
        .child(Input::new(&input).w_full())
        .when_some(binding_error, |this, error| {
          this.child(div().text_xs().text_color(cx.theme().danger).child(error))
        }),
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
  // P5-S1 (Law 2): parse failures render inline under the row instead of
  // dying in stderr while the binding silently drops.
  let _ = workspace.update(cx, |workspace, cx| {
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
