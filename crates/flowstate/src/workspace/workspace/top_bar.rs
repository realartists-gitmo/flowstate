#[hotpath::measure]
fn flowstate_top_bar_button(cx: &mut Context<Workspace>) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-flowstate")
        .icon(
          Icon::default()
            .path("logo/flowstate-mark.svg")
            .with_size(px(13.0)),
        )
        .label("Flowstate")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, _window, _cx| {
          let appearance_workspace = workspace.clone();
          menu
            // P5-S2: theme moved to Settings ▸ Appearance (live-applied there).
            .item(PopupMenuItem::new("Appearance Settings…").on_click(move |_, _, cx| {
              let _ = appearance_workspace.update(cx, |workspace, cx| {
                workspace.settings_section = WorkspaceSettingsSection::Appearance;
                workspace.settings_overlay = Some(WorkspaceSettingsOverlay::Settings);
                cx.notify();
              });
            }))
            .separator()
            // W-S1: quit means quit — every window gets its close prompts,
            // not just the focused one.
            .item(PopupMenuItem::new("Quit Flowstate").on_click(move |_, _, cx| {
              crate::workspace::request_quit_all_windows(cx);
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
          // Patient 5/7: five section items all opened the same overlay —
          // one honest door until the P5-S4 style editor replaces it.
          let workspace = workspace.clone();
          menu.item(PopupMenuItem::new("Document Styles...").on_click(move |_, _, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.document_style_section = DocumentStyleSection::Text;
              workspace.settings_overlay = Some(WorkspaceSettingsOverlay::Styles);
              cx.notify();
            });
          }))
        }),
    )
}

fn collaboration_top_bar_button(cx: &mut Context<Workspace>, has_document: bool, active_collaborating: bool) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(
      Button::new("top-collaborate")
        .label("Collaborate")
        .xsmall()
        .ghost()
        .dropdown_menu(move |menu, _, _| {
          menu
            .item(file_menu_item(workspace.clone(), "Share...", !has_document, |workspace, window, cx| {
              workspace.open_collaboration_dialog(window, cx);
            }))
            .item(file_menu_item(
              workspace.clone(),
              "Copy Invite Ticket",
              !has_document,
              |workspace, window, cx| {
                workspace.copy_active_collaboration_ticket(window, cx);
              },
            ))
            .item(command_menu_item(
              workspace.clone(),
              "Comments...",
              Some(crate::commands::CommandId::OpenComments),
              !has_document,
              |workspace, window, cx| {
                workspace.open_comments_panel(window, cx);
              },
            ))
            .separator()
            .item(file_menu_item(workspace.clone(), "Join Session...", false, |workspace, window, cx| {
              workspace.open_join_collaboration_dialog(window, cx);
            }))
            .item(file_menu_item(
              workspace.clone(),
              "Join from Clipboard",
              false,
              |workspace, window, cx| {
                workspace.join_collaboration_from_clipboard(window, cx);
              },
            ))
            .item(file_menu_item(
              workspace.clone(),
              "Leave Shared Session",
              !active_collaborating,
              |workspace, window, cx| {
                workspace.confirm_leave_collaboration_on_active_document(window, cx);
              },
            ))
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
            .item(command_menu_item(
              workspace.clone(),
              "Save",
              Some(crate::commands::CommandId::Save),
              !has_document,
              |workspace, window, cx| {
                workspace.save_active(window, cx);
              },
            ))
            .item(file_menu_item(workspace.clone(), "Save As", !has_document, |workspace, window, cx| {
              workspace.save_active_as(window, cx);
            }))
            .separator()
            .item(file_menu_item(workspace.clone(), "Share...", !has_document, |workspace, window, cx| {
              workspace.open_collaboration_dialog(window, cx);
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
  command_menu_item(workspace, label, None, disabled, action)
}

/// Menu item that displays its command's keybinding (Law 9: menus never hide
/// the keyboard path). `command` is optional only because some entries have
/// no `CommandId` yet — the omni-palette build registers the rest.
fn command_menu_item(
  workspace: WeakEntity<Workspace>,
  label: &'static str,
  command: Option<crate::commands::CommandId>,
  disabled: bool,
  action: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> PopupMenuItem {
  let mut item = PopupMenuItem::new(label);
  if let Some(bound) = command.and_then(crate::commands::action_for_command) {
    item = item.action(bound);
  }
  item.disabled(disabled).on_click(move |_, window, cx| {
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
        .dropdown_menu(move |menu, window, cx| {
          // The whole button disables without a document; per-item disable
          // duplication removed (the audit's redundant-disable finding).
          let image_workspace = workspace.clone();
          let table_workspace = workspace.clone();
          let equation_workspace = workspace.clone();
          menu
            .item(PopupMenuItem::new("Image...").on_click(move |_, _, cx| {
              insert_image_from_top_bar(&image_workspace, cx);
            }))
            .submenu("Table", window, cx, move |menu, _, _| {
              [(2usize, 2usize), (2, 3), (3, 3), (4, 4), (5, 5)]
                .into_iter()
                .fold(menu, |menu, (rows, columns)| {
                  let table_workspace = table_workspace.clone();
                  menu.item(
                    PopupMenuItem::new(format!("{rows} × {columns}")).on_click(move |_, _, cx| {
                      insert_table_from_top_bar(&table_workspace, rows, columns, cx);
                    }),
                  )
                })
            })
            .item(PopupMenuItem::new("Equation").on_click(move |_, window, cx| {
              insert_equation_composer_from_top_bar(&equation_workspace, window, cx);
            }))
        }),
    )
}

#[hotpath::measure]
fn share_top_bar_button(cx: &mut Context<Workspace>, has_document: bool, collaborating: bool) -> impl IntoElement {
  let workspace = cx.entity().downgrade();
  let button = Button::new("top-share-document")
    .label("Share")
    .xsmall()
    .ghost()
    .disabled(!has_document)
    .tooltip("Share / Collaborate")
    .on_click(move |_, window, cx| {
      let _ = workspace.update(cx, |workspace, cx| workspace.open_collaboration_dialog(window, cx));
    });

  div()
    .h_full()
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .child(if collaborating {
      button.custom(
        ButtonCustomVariant::new(cx)
          .foreground(cx.theme().success)
          .hover(cx.theme().success.opacity(0.12))
          .active(cx.theme().success.opacity(0.18)),
      )
    } else {
      button
    })
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
fn insert_table_from_top_bar(workspace: &WeakEntity<Workspace>, rows: usize, columns: usize, cx: &mut App) {
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.insert_default_table(rows, columns, cx));
    }
  });
}

#[hotpath::measure]
fn insert_equation_composer_from_top_bar(workspace: &WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
  // B-S8: insert opens the composer — the hardcoded placeholder died.
  let _ = workspace.update(cx, |workspace, cx| {
    if let Some(editor) = workspace.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.request_equation_composer(window, cx));
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
          [
            WorkspaceSettingsSection::General,
            WorkspaceSettingsSection::Collaboration,
            WorkspaceSettingsSection::Keymap,
          ]
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
fn view_top_bar_button(cx: &mut Context<Workspace>, outline_open: bool, ribbon_open: bool) -> impl IntoElement {
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
          menu
            .item(
              PopupMenuItem::new("Outline")
                .checked(outline_open)
                .keep_open(true)
                .on_click(move |_, _, cx| {
                  let _ = outline_workspace.update(cx, |workspace, cx| workspace.toggle_outline(cx));
                }),
            )
            .item(
              PopupMenuItem::new("Ribbon")
                .checked(ribbon_open)
                .keep_open(true)
                .on_click(move |_, _, cx| {
                  let _ = ribbon_workspace.update(cx, |workspace, cx| workspace.toggle_ribbon(cx));
                }),
            )
        }),
    )
}
