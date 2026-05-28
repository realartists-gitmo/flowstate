fn styles_top_bar_button(cx: &mut Context<Workspace>) -> impl IntoElement {
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-styles")
        .label("Styles")
        .xsmall()
        .ghost()
        .on_click(cx.listener(|workspace, _, _, cx| {
          workspace.styles_settings_open = !workspace.styles_settings_open;
          cx.stop_propagation();
          cx.notify();
        })),
    )
}

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
            .item(file_menu_item(workspace.clone(), "New File", false, |workspace, window, cx| {
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

fn insert_image_from_top_bar(workspace: &WeakEntity<Workspace>, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.prompt_insert_image(cx));
    }
  });
}

fn insert_default_table_from_top_bar(workspace: &WeakEntity<Workspace>, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.insert_default_table(2, 2, cx));
    }
  });
}

fn insert_default_equation_from_top_bar(workspace: &WeakEntity<Workspace>, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.insert_equation("x^2 + y^2 = z^2", cx));
    }
  });
}

fn top_bar_button(id: &'static str, label: &'static str) -> impl IntoElement {
  // The top bar itself starts native window dragging on mouse down. Each
  // button owns its mouse-down event so it behaves like a control instead of
  // dragging the window.
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new(id)
        .label(label)
        .xsmall()
        .ghost()
        .on_click(|_, _, cx| cx.stop_propagation()),
    )
}

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
        })
        .anchor(Corner::BottomLeft),
    )
}
