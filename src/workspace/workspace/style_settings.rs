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
  let read_workspace = workspace.clone();
  let write_workspace = workspace;
  SettingItem::new(
    title,
    SettingField::number_input(
      NumberFieldOptions { min, max, step },
      move |cx| active_theme_value(cx, &read_workspace, get).unwrap_or_default(),
      move |value, cx| update_active_document_theme(cx, &write_workspace, move |theme| set(theme, value)),
    ),
  )
  .layout(Axis::Horizontal)
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
    .child(div().text_sm().child("Font family"))
    .child(
      Select::new(&select)
        .placeholder("Font family")
        .search_placeholder("Search fonts")
        .menu_width(px(360.0))
        .w_96(),
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
  style_compact_item(workspace, label, |_| 0.0, |_, _| {}, None, get, set)
}

#[hotpath::measure]
fn style_compact_item(
  workspace: WeakEntity<Workspace>,
  label: &'static str,
  size_get: fn(&DocumentTheme) -> f64,
  size_set: fn(&mut DocumentTheme, f64),
  color_access: Option<(fn(&DocumentTheme) -> Hsla, fn(&mut DocumentTheme, Hsla))>,
  get: fn(&DocumentTheme) -> (bool, bool, ThemeUnderline),
  set: fn(&mut DocumentTheme, bool, bool, ThemeUnderline),
) -> SettingItem {
  SettingItem::render(move |_, window, cx| {
    render_style_compact_row(workspace.clone(), label, size_get, size_set, color_access, get, set, window, cx)
  })
}

#[hotpath::measure]
fn render_style_compact_row(
  workspace: WeakEntity<Workspace>,
  label: &'static str,
  size_get: fn(&DocumentTheme) -> f64,
  size_set: fn(&mut DocumentTheme, f64),
  color_access: Option<(fn(&DocumentTheme) -> Hsla, fn(&mut DocumentTheme, Hsla))>,
  get: fn(&DocumentTheme) -> (bool, bool, ThemeUnderline),
  set: fn(&mut DocumentTheme, bool, bool, ThemeUnderline),
  window: &mut Window,
  cx: &mut App,
) -> AnyElement {
  let key = label.to_ascii_lowercase().replace(' ', "-");
  let size_state = window.use_keyed_state(SharedString::from(format!("style-size-{key}")), cx, |window, cx| {
    let value = active_theme_value(cx, &workspace, size_get).unwrap_or_default();
    cx.new(|cx| InputState::new(window, cx).default_value(format!("{value:.2}")))
  });
  let color_picker_state = window.use_keyed_state(SharedString::from(format!("style-picker-{key}")), cx, |window, cx| {
    let value = color_access
      .and_then(|(get, _)| active_theme_value(cx, &workspace, get))
      .unwrap_or_else(black);
    ColorPickerState::new(window, cx).default_value(value)
  });
  let size_state = size_state.read(cx).clone();
  let color_picker_state = color_picker_state.clone();
  let (bold, italic, underline) = active_theme_value(cx, &workspace, get).unwrap_or_default();

  h_flex()
    .w_full()
    .items_center()
    .gap_2()
    .child(div().w_32().text_sm().child(label))
    .child(NumberInput::new(&size_state).w_24())
    .when_some(color_access, |this, (_, color_set)| {
      this
        .child(
          ColorPicker::new(&color_picker_state)
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
                if let Some(color) = color_picker_state.read(cx).value() {
                  update_active_document_theme(cx, &workspace, move |theme| color_set(theme, color));
                }
              }
            }),
        )
    })
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
    .child(
      Button::new(SharedString::from(format!("style-apply-size-{key}")))
        .icon(IconName::Check)
        .small()
        .ghost()
        .tooltip("Apply size")
        .on_click(move |_, _, cx| {
          if let Ok(value) = size_state.read(cx).value().parse::<f64>() {
            update_active_document_theme(cx, &workspace, move |theme| size_set(theme, value));
          }
        }),
    )
    .into_any_element()
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
          .child(div().text_sm().child("Smart word selection"))
          .child(
            div()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child("Snap mouse drag selections to whole words after crossing a word boundary."),
          ),
      )
      .child(
        Toggle::new("document-style-smart-word-selection")
          .small()
          .outline()
          .checked(enabled)
          .on_click({
            let workspace = workspace.clone();
            move |_, _, cx| {
              update_smart_word_selection(cx, &workspace, !enabled);
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
fn render_apply_all_styles(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) -> AnyElement {
  let font_size = window.use_keyed_state("style-apply-all-font-size", cx, |window, cx| {
    cx.new(|cx| {
      InputState::new(window, cx)
        .placeholder("Font size pt")
        .default_value("")
    })
  });
  let before = window.use_keyed_state("style-apply-all-before", cx, |window, cx| {
    cx.new(|cx| {
      InputState::new(window, cx)
        .placeholder("Before spacing pt")
        .default_value("")
    })
  });
  let text_color = window.use_keyed_state("style-apply-all-text-color", cx, |window, cx| {
    cx.new(|cx| {
      InputState::new(window, cx)
        .placeholder("Text color")
        .default_value("")
    })
  });
  let font_size_state = font_size.read(cx).clone();
  let before_state = before.read(cx).clone();
  let text_color_state = text_color.read(cx).clone();

  h_flex()
    .w_full()
    .gap_2()
    .items_center()
    .child(Input::new(&font_size_state).w_32())
    .child(Input::new(&before_state).w_32())
    .child(Input::new(&text_color_state).w_32())
    .child(
      Button::new("apply-all-document-styles")
        .label("Apply")
        .primary()
        .small()
        .on_click(move |_, _, cx| {
          let font_size = optional_f64(&font_size_state.read(cx).value());
          let before = optional_f64(&before_state.read(cx).value());
          let text_color = optional_hex_color(&text_color_state.read(cx).value());

          update_active_document_theme(cx, &workspace, move |theme| {
            if let Some(font_size) = font_size {
              let size = pt_to_pixels(font_size);
              theme.body_font_size = size;
              theme.cite_font_size = size;
              theme.condensed_font_size = size;
              theme.ultracondensed_font_size = size;
              theme.pocket_font_size = size;
              theme.hat_font_size = size;
              theme.block_font_size = size;
              theme.tag_font_size = size;
              theme.undertag_font_size = size;
            }
            if let Some(before) = before {
              let spacing = pt_to_pixels(before);
              theme.pocket_before = spacing;
              theme.hat_before = spacing;
              theme.block_before = spacing;
              theme.tag_before = spacing;
            }
            if let Some(color) = text_color {
              theme.default_text_color = color;
              theme.analytic_color = color;
              theme.undertag_color = color;
            }
          });
        }),
    )
    .into_any_element()
}

#[hotpath::measure]
fn pixels_to_pt(value: Pixels) -> f64 {
  value.as_f64() * 72.0 / 96.0
}

#[hotpath::measure]
fn pt_to_pixels(value: f64) -> Pixels {
  px((value as f32) * 96.0 / 72.0)
}

#[hotpath::measure]
fn parse_hex_color(value: &str) -> Option<Hsla> {
  let value = value.trim().trim_start_matches('#');
  if value.len() != 6 {
    return None;
  }
  u32::from_str_radix(value, 16)
    .ok()
    .map(|hex| rgb(hex).into())
}

#[hotpath::measure]
fn optional_f64(value: &str) -> Option<f64> {
  let value = value.trim();
  if value.is_empty() { None } else { value.parse::<f64>().ok() }
}

#[hotpath::measure]
fn optional_hex_color(value: &str) -> Option<Hsla> {
  let value = value.trim();
  if value.is_empty() { None } else { parse_hex_color(value) }
}

