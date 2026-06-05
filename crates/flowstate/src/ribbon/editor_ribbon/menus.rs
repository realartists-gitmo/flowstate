#[hotpath::measure]
fn modern_highlight_menu(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  document_theme: &DocumentTheme,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let accent = command.accent.unwrap_or(RibbonAccent::Gray);
  let selected_highlight = match command.id {
    RibbonCommandId::ToggleHighlightMode(style) => style,
    _ => None,
  };
  let mode_active = command.selected;
  let document_theme = document_theme.clone();
  let command_color = ribbon_command_color(command, cx);

  let chip_height = metrics.chip_height;

  div()
    .flex()
    .flex_row()
    .items_center()
    .gap_0()
    .child(
      div().relative().h(chip_height).child(
        DropdownButton::new("modern-ribbon-highlight-dropdown")
          .with_size(Size::Size(chip_height))
          .compact()
          .outline()
          .button(
            Button::new(("modern-ribbon-highlight-toggle", 0_u64))
              .compact()
              .ghost()
              .h(chip_height)
              .px(metrics.chip_padding_x)
              .when(mode_active, |this| {
                this
                  .bg(command_color.opacity(0.18))
                  .border_color(command_color)
                  .text_color(command_color)
              })
              .tooltip_with_action("Highlight mode", &ApplyHighlightToSelection, Some("RichTextEditor"))
              .child(match accent {
                RibbonAccent::Transparent => transparent_accent_bar(cx),
                _ => accent_bar(accent_color(accent, cx), cx),
              })
              .child(
                Icon::default()
                  .path("icons/highlighter.svg")
                  .xsmall()
                  .text_color(command_color),
              )
              .when_some(shortcut_for(CommandId::ApplyHighlightToSelection), |this, shortcut| {
                this.child(keycap(shortcut, cx))
              })
              .on_click({
                let editor = editor.clone();
                move |_, _, cx| {
                  editor.update(cx, |editor, cx| {
                    editor.toggle_highlight_mode(cx);
                  });
                }
              }),
          )
          .dropdown_menu(move |menu, _, _| {
            let menu = menu.min_w(px(180.0)).max_w(px(220.0));

            let menu = HIGHLIGHT_STYLE_SPECS.iter().fold(menu, |menu, spec| {
              let style = spec.style;
              let label = spec.label;
              let editor = editor.clone();
              let color = highlight_color(style, &document_theme);

              menu.item(
                PopupMenuItem::element(move |_, _| {
                  div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(highlight_menu_swatch(color))
                    .child(label)
                })
                .checked(selected_highlight == Some(style))
                .on_click(move |_, _, cx| {
                  editor.update(cx, |editor, cx| {
                    editor.select_highlight_style(Some(style), cx);
                  });
                }),
              )
            });

            let editor = editor.clone();

            menu.item(
              PopupMenuItem::new("Clear highlight")
                .checked(selected_highlight.is_none())
                .on_click(move |_, _, cx| {
                  editor.update(cx, |editor, cx| {
                    editor.select_highlight_style(None, cx);
                  });
                }),
            )
          }),
      ),
    )
    .into_any_element()
}

#[hotpath::measure]
fn modern_condensed_menu(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let mode_active = command.selected;
  let checked = command.selected;
  let chip_height = metrics.chip_height;
  let label = RibbonLabel::for_command(command);
  let command_color = ribbon_command_color(command, cx);

  DropdownButton::new("modern-ribbon-condensed-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .button(
      Button::new("modern-ribbon-condensed-toggle")
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        .when(mode_active, |this| {
          this
            .bg(command_color.opacity(0.18))
            .border_color(command_color)
            .text_color(command_color)
        })
        .tooltip("Shrink")
        .when_some(label.icon_path, |this, path| {
          this.child(
            Icon::default()
              .path(path)
              .xsmall()
              .text_color(command_color),
          )
        })
        .when(!label.prefers_icon(), |this| {
          this.child(
            div()
              .flex_none()
              .text_size(metrics.chip_text_size)
              .line_height(relative(1.0))
              .whitespace_nowrap()
              .text_ellipsis()
              .text_color(command_color)
              .child(label.text),
          )
        })
        .on_click({
          let editor = editor.clone();
          move |_, _, cx| {
            editor.update(cx, |editor, cx| {
              editor.toggle_inline_tool(ArmedInlineTool::Semantic(flowstate_document::SEMANTIC_CONDENSED), cx);
            });
          }
        }),
    )
    .dropdown_menu(move |menu, _, _| {
      let editor_for_condensed = editor.clone();
      let editor_for_ultra = editor.clone();
      menu
        .min_w(px(190.0))
        .item(
          PopupMenuItem::new("Shrink")
            .checked(checked)
            .on_click(move |_, _, cx| {
              editor_for_condensed.update(cx, |editor, cx| {
                editor.toggle_inline_tool(ArmedInlineTool::Semantic(flowstate_document::SEMANTIC_CONDENSED), cx);
              });
            }),
        )
        .item(PopupMenuItem::new("Ultra shrink").on_click(move |_, _, cx| {
          editor_for_ultra.update(cx, |editor, cx| {
            editor.toggle_inline_tool(ArmedInlineTool::Semantic(flowstate_document::SEMANTIC_ULTRACONDENSED), cx);
          });
        }))
    })
    .into_any_element()
}

#[hotpath::measure]
fn timer_section(metrics: RibbonLayoutMetrics, cx: &mut Context<EditorRibbon>) -> AnyElement {
  let remaining = cx.entity().read(cx).timer_remaining_secs();
  let running = cx.entity().read(cx).timer_running();
  let minutes = remaining / 60;
  let seconds = remaining % 60;
  div()
    .flex()
    .flex_col()
    .flex_none()
    .gap_0p5()
    .pl(metrics.group_divider_padding_left)
    .border_l_1()
    .border_color(cx.theme().border.opacity(0.72))
    .child(
      div()
        .text_size(px(10.0))
        .font_medium()
        .text_color(cx.theme().foreground)
        .child("Timer"),
    )
    .child(
      h_flex()
        .gap_0p5()
        .child(
          Button::new("ribbon-timer-minus")
            .compact()
            .ghost()
            .h(metrics.chip_height)
            .label("−")
            .tooltip("Subtract 30 seconds")
            .on_click(cx.listener(|ribbon, _, _, cx| ribbon.adjust_timer_duration(-30, cx))),
        )
        .child(
          Button::new("ribbon-timer-toggle")
            .compact()
            .outline()
            .h(metrics.chip_height)
            .px(metrics.chip_padding_x)
            .label(format!("{minutes:02}:{seconds:02}"))
            .tooltip(if running { "Pause timer" } else { "Start timer" })
            .on_click(cx.listener(|ribbon, _, _, cx| ribbon.toggle_timer(cx))),
        )
        .child(
          Button::new("ribbon-timer-plus")
            .compact()
            .ghost()
            .h(metrics.chip_height)
            .label("+")
            .tooltip("Add 30 seconds")
            .on_click(cx.listener(|ribbon, _, _, cx| ribbon.adjust_timer_duration(30, cx))),
        )
        .child(
          Button::new("ribbon-timer-reset")
            .compact()
            .ghost()
            .h(metrics.chip_height)
            .label("↺")
            .tooltip("Reset timer")
            .on_click(cx.listener(|ribbon, _, _, cx| ribbon.reset_timer(cx))),
        ),
    )
    .into_any_element()
}

#[hotpath::measure]
fn export_section(editor: Entity<RichTextEditor>, metrics: RibbonLayoutMetrics, cx: &mut Context<EditorRibbon>) -> AnyElement {
  let chip_height = metrics.chip_height;
  let send_created = editor
    .read(cx)
    .send_document_created_since_last_saved_edit();
  let format_created = editor
    .read(cx)
    .format_export_created_since_last_saved_edit();
  div()
    .flex()
    .flex_col()
    .flex_none()
    .gap_0p5()
    .pl(metrics.group_divider_padding_left)
    .border_l_1()
    .border_color(cx.theme().border.opacity(0.72))
    .child(
      div()
        .text_size(px(10.0))
        .font_medium()
        .text_color(cx.theme().foreground)
        .child("Export"),
    )
    .child(
      div()
        .flex()
        .gap_0p5()
        .child(format_dropdown(editor.clone(), chip_height, metrics, format_created, cx))
        .child(send_dropdown(editor, chip_height, metrics, send_created, cx)),
    )
    .into_any_element()
}

#[hotpath::measure]
fn format_dropdown(
  editor: Entity<RichTextEditor>,
  chip_height: gpui::Pixels,
  metrics: RibbonLayoutMetrics,
  format_created: bool,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  DropdownButton::new("modern-ribbon-format-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .when(format_created, |this| this.dropdown_icon(IconName::Check, Some(cx.theme().success)))
    .button(
      export_chip_button("modern-ribbon-format", "Export as DOCX", "Format", chip_height, metrics).on_click({
        let editor = editor.clone();
        move |_, _, cx| {
          export_format_from_ribbon(editor.clone(), DocumentExportFormat::Docx, cx);
        }
      }),
    )
    .dropdown_menu(move |menu, _, _| {
      let docx_editor = editor.clone();
      let pdf_editor = editor.clone();
      menu
        .min_w(px(120.0))
        .item(PopupMenuItem::new(".docx").on_click(move |_, _, cx| {
          export_format_from_ribbon(docx_editor.clone(), DocumentExportFormat::Docx, cx);
        }))
        .item(PopupMenuItem::new(".pdf").on_click(move |_, _, cx| {
          export_format_from_ribbon(pdf_editor.clone(), DocumentExportFormat::Pdf, cx);
        }))
    })
    .into_any_element()
}

#[hotpath::measure]
fn send_dropdown(
  editor: Entity<RichTextEditor>,
  chip_height: gpui::Pixels,
  metrics: RibbonLayoutMetrics,
  send_created: bool,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  DropdownButton::new("modern-ribbon-send-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .when(send_created, |this| this.dropdown_icon(IconName::Check, Some(cx.theme().success)))
    .button(
      export_chip_button("modern-ribbon-send", "Send as DB8", "Send", chip_height, metrics).on_click({
        let editor = editor.clone();
        move |_, _, cx| {
          send_format_from_ribbon(
            editor.clone(),
            DocumentExportFormat::NativeWithExtension(flowstate_document::FLOWSTATE_EXTENSION),
            cx,
          );
        }
      }),
    )
    .dropdown_menu(move |menu, _, _| {
      let db8_editor = editor.clone();
      let docx_editor = editor.clone();
      let pdf_editor = editor.clone();
      menu
        .min_w(px(120.0))
        .item(PopupMenuItem::new(".db8").on_click(move |_, _, cx| {
          send_format_from_ribbon(
            db8_editor.clone(),
            DocumentExportFormat::NativeWithExtension(flowstate_document::FLOWSTATE_EXTENSION),
            cx,
          );
        }))
        .item(PopupMenuItem::new(".docx").on_click(move |_, _, cx| {
          send_format_from_ribbon(docx_editor.clone(), DocumentExportFormat::Docx, cx);
        }))
        .item(PopupMenuItem::new(".pdf").on_click(move |_, _, cx| {
          send_format_from_ribbon(pdf_editor.clone(), DocumentExportFormat::Pdf, cx);
        }))
    })
    .into_any_element()
}

#[hotpath::measure]
fn export_chip_button(
  id: &'static str,
  tooltip: &'static str,
  label: &'static str,
  chip_height: gpui::Pixels,
  metrics: RibbonLayoutMetrics,
) -> Button {
  Button::new(id)
    .compact()
    .ghost()
    .h(chip_height)
    .px(metrics.chip_padding_x)
    .tooltip(tooltip)
    .child(
      div()
        .flex_none()
        .text_size(metrics.chip_text_size)
        .line_height(relative(1.0))
        .whitespace_nowrap()
        .text_ellipsis()
        .child(label),
    )
}

#[hotpath::measure]
fn send_format_from_ribbon(editor: Entity<RichTextEditor>, format: DocumentExportFormat, cx: &mut App) {
  let task = editor.update(cx, |editor, cx| editor.send_document(format, cx));
  cx.spawn(async move |_| {
    if let Err(error) = task.await {
      eprintln!("send export failed: {error}");
    }
  })
  .detach();
}

#[hotpath::measure]
fn export_format_from_ribbon(editor: Entity<RichTextEditor>, format: DocumentExportFormat, cx: &mut App) {
  let task = editor.update(cx, |editor, cx| editor.export_document_format(format, cx));
  cx.spawn(async move |_| {
    if let Err(error) = task.await {
      eprintln!("format export failed: {error}");
    }
  })
  .detach();
}

#[hotpath::measure]
fn invisibility_mode_button(
  editor: Entity<RichTextEditor>,
  invisibility_mode: bool,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  div()
    .flex()
    .flex_col()
    .flex_none()
    .gap_0p5()
    .pl(metrics.group_divider_padding_left)
    .border_l_1()
    .border_color(cx.theme().border.opacity(0.72))
    .child(
      div()
        .text_size(px(10.0))
        .font_medium()
        .text_color(cx.theme().foreground)
        .child("Views"),
    )
    .child(
      Button::new("invisibility-mode-toggle")
        .xsmall()
        .compact()
        .outline()
        .h(metrics.chip_height)
        .w(metrics.chip_height)
        .icon(Icon::new(if invisibility_mode { IconName::EyeOff } else { IconName::Eye }).text_color(cx.theme().info))
        .selected(invisibility_mode)
        .tooltip("Invisibility mode")
        .on_click(move |_, _, cx| {
          editor.update(cx, |editor, cx| {
            editor.toggle_invisibility_mode(cx);
          });
        }),
    )
    .into_any_element()
}
