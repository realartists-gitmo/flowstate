#[hotpath::measure]
fn style_number_item(
  workspace: WeakEntity<Workspace>,
  title: &'static str,
  min: f64,
  max: f64,
  step: f64,
  get: fn(&DocumentTheme) -> f64,
  set: fn(&mut DocumentTheme, f64),
) -> SettingItem {
  SettingItem::render(move |_, window, cx| {
    let key = title.to_ascii_lowercase().replace([' ', '(', ')'], "-");
    let value = active_theme_value(cx, &workspace, get).unwrap_or_default();
    let state = window.use_keyed_state(SharedString::from(format!("style-number-{key}")), cx, {
      let workspace = workspace.clone();
      move |window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(format!("{value:.2}")));
        let _subscriptions = vec![
          cx.subscribe_in(&input, window, move |_, input, event: &NumberInputEvent, window, cx| {
            let NumberInputEvent::Step(action) = event;
            input.update(cx, |input, cx| {
              if let Ok(value) = input.value().parse::<f64>() {
                let next = match action {
                  StepAction::Increment => value + step,
                  StepAction::Decrement => value - step,
                }
                .clamp(min, max);
                input.set_value(SharedString::from(format!("{next:.2}")), window, cx);
              }
            });
          }),
          cx.subscribe_in(&input, window, {
            let workspace = workspace.clone();
            move |state: &mut StyleNumberInputState, input, event: &InputEvent, window, cx| {
              if let InputEvent::Change = event {
                input.update(cx, |input, cx| {
                  if let Ok(value) = input.value().parse::<f64>() {
                    let value = value.clamp(min, max);
                    if value == state.initial_value {
                      return;
                    }
                    update_active_document_theme(cx, &workspace, move |theme| set(theme, value));
                    state.initial_value = value;
                    if input.value().parse::<f64>().ok() != Some(value) {
                      input.set_value(SharedString::from(format!("{value:.2}")), window, cx);
                    }
                  }
                });
              }
            }
          }),
        ];

        StyleNumberInputState {
          input,
          initial_value: value,
          _subscriptions,
        }
      }
    });
    let input = state.read(cx).input.clone();

    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_3()
      .child(div().w_40().text_sm().child(title))
      .child(div().w(px(180.0)).ml_auto().child(NumberInput::new(&input).w_full()))
      .into_any_element()
  })
}

#[hotpath::measure]
fn font_family_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, window, cx| render_font_family_row(workspace.clone(), window, cx))
}

#[hotpath::measure]
fn render_font_family_row(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) -> AnyElement {
  let current = active_theme_value(cx, &workspace, |theme| theme.default_font_family.clone()).unwrap_or_else(|| SharedString::from("Carlito"));
  let fonts = system_font_families(cx, current.clone());
  let select_state = window.use_keyed_state("style-font-family-select", cx, {
    let workspace = workspace.clone();
    let current = current.clone();
    let fonts = fonts.clone();
    move |window, cx| {
      let select = cx.new(|cx| {
        let mut select = SelectState::new(SearchableVec::new(fonts), None, window, cx).searchable(true);
        select.set_selected_value(&current, window, cx);
        select
      });
      let _subscription = cx.subscribe_in(&select, window, {
        let workspace = workspace.clone();
        move |_, _, event: &SelectEvent<FontFamilySelectDelegate>, _, cx| {
          if let SelectEvent::Confirm(Some(font_family)) = event {
            let font_family = font_family.clone();
            update_active_document_theme(cx, &workspace, move |theme| {
              theme.default_font_family = font_family;
            });
          }
        }
      });

      FontFamilySelectState { select, _subscription }
    }
  });

  let select = select_state.read(cx).select.clone();
  let selected_matches_theme = select
    .read(cx)
    .selected_value()
    .map(|selected| selected == &current)
    .unwrap_or(false);
  if !selected_matches_theme {
    select.update(cx, |select, cx| select.set_selected_value(&current, window, cx));
  }

  h_flex()
    .w_full()
    .items_center()
    .justify_between()
    .gap_3()
    .child(div().w_40().text_sm().child("Font family"))
    .child(
      div().w(px(180.0)).ml_auto().child(
        Select::new(&select)
          .placeholder("Font family")
          .search_placeholder("Search fonts")
          .menu_width(px(180.0))
          .w_full(),
      ),
    )
    .into_any_element()
}

#[hotpath::measure]
fn system_font_families(cx: &App, current: SharedString) -> Vec<SharedString> {
  let mut fonts = cx
    .text_system()
    .all_font_names()
    .into_iter()
    .map(SharedString::from)
    .collect::<Vec<_>>();

  fonts.sort_by_key(|font| font.to_lowercase());
  fonts.dedup();
  if !fonts.iter().any(|font| font == &current) {
    fonts.insert(0, current);
  }

  fonts
}

#[hotpath::measure]
fn style_face_item(
  workspace: WeakEntity<Workspace>,
  label: &'static str,
  get: fn(&DocumentTheme) -> (bool, bool, ThemeUnderline),
  set: fn(&mut DocumentTheme, bool, bool, ThemeUnderline),
) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let key = label.to_ascii_lowercase().replace(' ', "-");
    let (bold, italic, underline) = active_theme_value(cx, &workspace, get).unwrap_or_default();

    h_flex()
      .w_full()
      .items_center()
      .gap_3()
      .child(div().w_40().text_sm().child(label))
      .child(
        Button::new(SharedString::from(format!("style-bold-{key}")))
          .label("B")
          .small()
          .outline()
          .selected(bold)
          .on_click({
            let workspace = workspace.clone();
            move |_, _, cx| {
              update_active_document_theme(cx, &workspace, move |theme| {
                let (_, italic, underline) = get(theme);
                set(theme, !bold, italic, underline);
              });
            }
          }),
      )
      .child(
        Button::new(SharedString::from(format!("style-italic-{key}")))
          .label("I")
          .small()
          .outline()
          .selected(italic)
          .on_click({
            let workspace = workspace.clone();
            move |_, _, cx| {
              update_active_document_theme(cx, &workspace, move |theme| {
                let (bold, _, underline) = get(theme);
                set(theme, bold, !italic, underline);
              });
            }
          }),
      )
      .child(
        Button::new(SharedString::from(format!("style-underline-{key}")))
          .label(match underline {
            ThemeUnderline::None => "U: None",
            ThemeUnderline::Single => "U: Single",
            ThemeUnderline::Double => "U: Double",
          })
          .small()
          .outline()
          .on_click({
            let workspace = workspace.clone();
            move |_, _, cx| {
              update_active_document_theme(cx, &workspace, move |theme| {
                let (bold, italic, underline) = get(theme);
                let next = match underline {
                  ThemeUnderline::None => ThemeUnderline::Single,
                  ThemeUnderline::Single => ThemeUnderline::Double,
                  ThemeUnderline::Double => ThemeUnderline::None,
                };
                set(theme, bold, italic, next);
              });
            }
          }),
      )
      .into_any_element()
  })
}

#[hotpath::measure]
fn style_bold_italic_item(
  workspace: WeakEntity<Workspace>,
  label: &'static str,
  get: fn(&DocumentTheme) -> (bool, bool),
  set: fn(&mut DocumentTheme, bool, bool),
) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let key = label.to_ascii_lowercase().replace(' ', "-");
    let (bold, italic) = active_theme_value(cx, &workspace, get).unwrap_or_default();

    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_3()
      .child(div().w_40().text_sm().child(label))
      .child(
        h_flex()
          .ml_auto()
          .gap_3()
          .child(
            Button::new(SharedString::from(format!("style-bold-{key}")))
              .label("B")
              .small()
              .outline()
              .selected(bold)
              .on_click({
                let workspace = workspace.clone();
                move |_, _, cx| {
                  update_active_document_theme(cx, &workspace, move |theme| {
                    let (_, italic) = get(theme);
                    set(theme, !bold, italic);
                  });
                }
              }),
          )
          .child(
            Button::new(SharedString::from(format!("style-italic-{key}")))
              .label("I")
              .small()
              .outline()
              .selected(italic)
              .on_click({
                let workspace = workspace.clone();
                move |_, _, cx| {
                  update_active_document_theme(cx, &workspace, move |theme| {
                    let (bold, _) = get(theme);
                    set(theme, bold, !italic);
                  });
                }
              }),
          ),
      )
      .into_any_element()
  })
}

#[hotpath::measure]
fn style_color_item(
  workspace: WeakEntity<Workspace>,
  title: &'static str,
  get: fn(&DocumentTheme) -> Hsla,
  set: fn(&mut DocumentTheme, Hsla),
) -> SettingItem {
  SettingItem::render(move |_, window, cx| {
    let key = title.to_ascii_lowercase().replace(' ', "-");
    let picker_state = window.use_keyed_state(SharedString::from(format!("style-color-picker-{key}")), cx, |window, cx| {
      let value = active_theme_value(cx, &workspace, get).unwrap_or_else(black);
      ColorPickerState::new(window, cx).default_value(value)
    });
    let picker_state = picker_state.clone();
    h_flex()
      .w_full()
      .items_center()
      .gap_2()
      .child(div().w_48().text_sm().child(title))
      .child(
        ColorPicker::new(&picker_state)
          .small()
          .anchor(Corner::TopRight),
      )
      .child(
        Button::new(SharedString::from(format!("style-apply-color-{key}")))
          .icon(IconName::Check)
          .small()
          .ghost()
          .tooltip("Apply color")
          .on_click({
            let workspace = workspace.clone();
            move |_, _, cx| {
              if let Some(color) = picker_state.read(cx).value() {
                update_active_document_theme(cx, &workspace, move |theme| set(theme, color));
              }
            }
          }),
      )
      .into_any_element()
  })
}

#[hotpath::measure]
fn active_theme_value<T>(cx: &App, workspace: &WeakEntity<Workspace>, get: fn(&DocumentTheme) -> T) -> Option<T> {
  let workspace = workspace.upgrade()?;
  let workspace = workspace.read(cx);
  if let Some(editor) = workspace.active_editor.clone() {
    Some(get(&editor.read(cx).document().theme))
  } else {
    Some(get(&load_document_theme()))
  }
}

#[hotpath::measure]
fn update_active_document_theme(cx: &mut App, workspace: &WeakEntity<Workspace>, update: impl FnOnce(&mut DocumentTheme)) {
  let _ = workspace.update(cx, |workspace, cx| {
    let mut theme = workspace
      .active_editor
      .as_ref()
      .map(|editor| editor.read(cx).document().theme.clone())
      .unwrap_or_else(load_document_theme);
    update(&mut theme);

    let theme_for_save = theme.clone();
    cx.background_executor()
      .spawn(async move {
        if let Err(error) = save_document_theme(&theme_for_save) {
          eprintln!("failed to save document style settings: {error}");
        }
      })
      .detach();

    workspace.apply_document_theme_to_open_editors(theme, cx);
  });
}

#[hotpath::measure]
fn smart_word_selection_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let enabled = active_smart_word_selection(cx, &workspace);
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_4()
      .child(
        div()
          .flex_1()
          .min_w_0()
          .child(div().text_sm().child("Smart word selection")),
      )
      .child(
        Checkbox::new("document-style-smart-word-selection")
          .small()
          .checked(enabled)
          .on_click({
            let workspace = workspace.clone();
            move |checked, _, cx| {
              update_smart_word_selection(cx, &workspace, *checked);
            }
          }),
      )
      .into_any_element()
  })
}

fn autosave_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let enabled = active_autosave(cx, &workspace);
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_4()
      .child(
        div()
          .flex_1()
          .min_w_0()
          .child(div().text_sm().child("Autosave")),
      )
      .child(
        Checkbox::new("workspace-autosave")
          .small()
          .checked(enabled)
          .on_click({
            let workspace = workspace.clone();
            move |checked, _, cx| {
              update_autosave(cx, &workspace, *checked);
            }
          }),
      )
      .into_any_element()
  })
}

#[hotpath::measure]
fn active_smart_word_selection(cx: &App, workspace: &WeakEntity<Workspace>) -> bool {
  workspace
    .upgrade()
    .and_then(|workspace| workspace.read(cx).active_editor.clone())
    .map(|editor| editor.read(cx).config().smart_word_selection)
    .unwrap_or_else(load_smart_word_selection)
}

#[hotpath::measure]
fn update_smart_word_selection(cx: &mut App, workspace: &WeakEntity<Workspace>, enabled: bool) {
  cx.background_executor()
    .spawn(async move {
      if let Err(error) = save_smart_word_selection(enabled) {
        eprintln!("failed to save smart word selection setting: {error}");
      }
    })
    .detach();

  let _ = workspace.update(cx, |workspace, cx| {
    for panel in &workspace.document_panels {
      let editor = panel.read(cx).editor();
      editor.update(cx, |editor, cx| {
        editor.set_smart_word_selection(enabled, cx);
      });
    }
    cx.notify();
  });
}

#[hotpath::measure]
fn active_autosave(cx: &App, workspace: &WeakEntity<Workspace>) -> bool {
  workspace
    .upgrade()
    .map(|workspace| workspace.read(cx).autosave_enabled)
    .unwrap_or_else(load_autosave)
}

fn update_autosave(cx: &mut App, workspace: &WeakEntity<Workspace>, enabled: bool) {
  cx.background_executor()
    .spawn(async move {
      if let Err(error) = save_autosave(enabled) {
        eprintln!("failed to save autosave setting: {error}");
      }
    })
    .detach();

  let _ = workspace.update(cx, |workspace, cx| {
    workspace.autosave_enabled = enabled;
    if !enabled {
      workspace.autosave_document_generations.clear();
      workspace.autosave_flow_in_flight.clear();
    }
    cx.notify();
  });
}

#[hotpath::measure]
fn pixels_to_pt(value: Pixels) -> f64 {
  value.as_f64() * 72.0 / 96.0
}

#[hotpath::measure]
fn pt_to_pixels(value: f64) -> Pixels {
  px((value as f32) * 96.0 / 72.0)
}
