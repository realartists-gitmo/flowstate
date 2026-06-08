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
    .child(div().flex_1().min_w_0().child(Input::new(&input).w_full()))
    .into_any_element()
}

fn update_keymap_command(cx: &mut App, workspace: &WeakEntity<Workspace>, command: crate::commands::CommandId, value: String) {
  let mut keymap = crate::app_settings::load_keymap();
  keymap.entries.retain(|entry| entry.command != command);
  let context = crate::commands::command_spec(command).and_then(|spec| spec.context.map(str::to_string));
  for key in value
    .split(',')
    .map(str::trim)
    .filter(|key| !key.is_empty())
  {
    keymap.entries.push(crate::commands::KeymapEntry {
      command,
      key: key.to_string(),
      context: context.clone(),
    });
  }
  let entries = keymap.entries.clone();
  cx.background_executor()
    .spawn(async move {
      if let Err(error) = crate::app_settings::save_keymap_entries(entries) {
        eprintln!("failed to save keymap: {error}");
      }
    })
    .detach();
  let _ = workspace.update(cx, |_, cx| cx.notify());
}
