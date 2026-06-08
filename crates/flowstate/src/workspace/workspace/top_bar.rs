#[hotpath::measure]
fn flowstate_top_bar_button(cx: &mut Context<Workspace>) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  let current_theme = Theme::global(cx).theme_name().to_string();
  let theme_names = ThemeRegistry::global(cx)
    .sorted_themes()
    .into_iter()
    .map(|theme| theme.name.to_string())
    .collect::<Vec<_>>();

  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-flowstate")
        .label("Flowstate")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, window, cx| {
          let workspace = workspace.clone();
          menu
            .submenu("Change Theme", window, cx, {
              let theme_names = theme_names.clone();
              let current_theme = current_theme.clone();
              move |menu, _, _| {
                let menu = menu
                  .scrollable(true)
                  .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always);
                theme_names.iter().fold(menu, |menu, theme_name| {
                  let selected = theme_name == &current_theme;
                  let label = theme_name.clone();
                  let preview_theme = theme_name.clone();
                  let restore_theme = current_theme.clone();
                  let apply_theme = theme_name.clone();
                  let committed = Rc::new(Cell::new(false));
                  menu.item(
                    PopupMenuItem::new(label)
                      .checked(selected)
                      .on_hover({
                        let committed = committed.clone();
                        move |hovered, window, cx| {
                          if *hovered {
                            preview_app_theme(&preview_theme, Some(window), cx);
                          } else if !committed.get() {
                            preview_app_theme(&restore_theme, Some(window), cx);
                          }
                        }
                      })
                      .on_click({
                        let committed = committed.clone();
                        move |_, window, cx| {
                          committed.set(true);
                          apply_app_theme(&apply_theme, Some(window), cx);
                        }
                      }),
                  )
                })
              }
            })
            .separator()
            .item(PopupMenuItem::new("Quit Flowstate").on_click(move |_, window, cx| {
              let _ = workspace.update(cx, |workspace, cx| workspace.request_close_window(window, cx));
            }))
        }),
    )
}

fn document_top_bar_button(cx: &mut Context<Workspace>) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-document")
        .label("Document")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, _, _| {
          [
            DocumentStyleSection::Text,
            DocumentStyleSection::Style,
            DocumentStyleSection::Colors,
            DocumentStyleSection::Size,
            DocumentStyleSection::Background,
          ]
          .into_iter()
          .fold(menu, |menu, section| {
            let workspace = workspace.clone();
            menu.item(PopupMenuItem::new(section.title()).on_click(move |_, _, cx| {
              let _ = workspace.update(cx, |workspace, cx| {
                workspace.document_style_section = section;
                workspace.settings_overlay = Some(WorkspaceSettingsOverlay::Styles);
                cx.notify();
              });
            }))
          })
        }),
    )
}

#[hotpath::measure]
fn file_top_bar_button(has_document: bool, cx: &mut Context<Workspace>) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-file")
        .label("File")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, _, _| {
          menu
            .item(file_menu_item(workspace.clone(), "New Doc", false, |workspace, window, cx| {
              workspace.new_document(window, cx);
            }))
            .item(file_menu_item(workspace.clone(), "New Flow", false, |workspace, window, cx| {
              workspace.new_flow(window, cx);
            }))
            .item(PopupMenuItem::new("New Window").on_click(|_, _, cx| {
              open_workspace_window(None, cx);
            }))
            .separator()
            .item(file_menu_item(workspace.clone(), "Open File", false, |workspace, window, cx| {
              workspace.prompt_open_document(window, cx);
            }))
            .item(file_menu_item(workspace.clone(), "Save", !has_document, |workspace, window, cx| {
              workspace.save_active(window, cx);
            }))
            .item(file_menu_item(workspace.clone(), "Save As", !has_document, |workspace, window, cx| {
              workspace.save_active_as(window, cx);
            }))
            .separator()
            .item(file_menu_item(workspace.clone(), "Close File", !has_document, |workspace, window, cx| {
              workspace.close_active_document(window, cx);
            }))
            .item(file_menu_item(workspace.clone(), "Close Window", false, |workspace, window, cx| {
              workspace.request_close_window(window, cx);
            }))
        }),
    )
}

#[hotpath::measure]
fn file_menu_item(
  workspace: WeakEntity<Workspace>,
  label: &'static str,
  disabled: bool,
  action: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> PopupMenuItem {
  PopupMenuItem::new(label)
    .disabled(disabled)
    .on_click(move |_, window, cx| {
      let _ = workspace.update(cx, |workspace, cx| action(workspace, window, cx));
    })
}

#[hotpath::measure]
fn insert_top_bar_button(cx: &mut Context<Workspace>, has_document: bool) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-insert")
        .label("Insert")
        .xsmall()
        .ghost()
        .disabled(!has_document)
        .dropdown_menu(move |menu, _, _| {
          let image_workspace = workspace.clone();
          let table_workspace = workspace.clone();
          let equation_workspace = workspace.clone();
          menu
            .item(
              PopupMenuItem::new("Image...")
                .disabled(!has_document)
                .on_click(move |_, _, cx| {
                  insert_image_from_top_bar(&image_workspace, cx);
                }),
            )
            .item(
              PopupMenuItem::new("Table")
                .disabled(!has_document)
                .on_click(move |_, _, cx| {
                  insert_default_table_from_top_bar(&table_workspace, cx);
                }),
            )
            .item(
              PopupMenuItem::new("Equation")
                .disabled(!has_document)
                .on_click(move |_, _, cx| {
                  insert_default_equation_from_top_bar(&equation_workspace, cx);
                }),
            )
        }),
    )
}

#[hotpath::measure]
fn insert_image_from_top_bar(workspace: &WeakEntity<Workspace>, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.prompt_insert_image(cx));
    }
  });
}

#[hotpath::measure]
fn insert_default_table_from_top_bar(workspace: &WeakEntity<Workspace>, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.insert_default_table(2, 2, cx));
    }
  });
}

#[hotpath::measure]
fn insert_default_equation_from_top_bar(workspace: &WeakEntity<Workspace>, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.insert_equation("x^2 + y^2 = z^2", cx));
    }
  });
}

#[hotpath::measure]
fn settings_top_bar_button(cx: &mut Context<Workspace>) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-settings")
        .label("Settings")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, _, _| {
          [WorkspaceSettingsSection::General, WorkspaceSettingsSection::Keymap]
            .into_iter()
            .fold(menu, |menu, section| {
              let workspace = workspace.clone();
              menu.item(PopupMenuItem::new(section.title()).on_click(move |_, _, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                  workspace.settings_section = section;
                  workspace.settings_overlay = Some(WorkspaceSettingsOverlay::Settings);
                  cx.notify();
                });
              }))
            })
        }),
    )
}

#[hotpath::measure]
fn view_top_bar_button(cx: &mut Context<Workspace>, outline_open: bool, ribbon_open: bool, toolkit_open: bool) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-view")
        .label("View")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, _, _| {
          let outline_workspace = workspace.clone();
          let ribbon_workspace = workspace.clone();
          let toolkit_workspace = workspace.clone();
          menu
            .item(
              PopupMenuItem::new("Outline")
                .checked(outline_open)
                .on_click(move |_, _, cx| {
                  let _ = outline_workspace.update(cx, |workspace, cx| workspace.toggle_outline(cx));
                }),
            )
            .item(
              PopupMenuItem::new("Ribbon")
                .checked(ribbon_open)
                .on_click(move |_, _, cx| {
                  let _ = ribbon_workspace.update(cx, |workspace, cx| workspace.toggle_ribbon(cx));
                }),
            )
            .item(
          PopupMenuItem::new("Toolkit")
                .checked(toolkit_open)
                .on_click(move |_, _, cx| {
                  let _ = toolkit_workspace.update(cx, |workspace, cx| workspace.toggle_toolkit(cx));
                }),
            )
        }),
    )
}
