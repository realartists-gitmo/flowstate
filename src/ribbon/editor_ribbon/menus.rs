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
              .child(Icon::default().path("icons/highlighter.svg").xsmall().text_color(command_color))
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
        .tooltip("Condensed")
        .when_some(label.icon_path, |this, path| {
          this.child(Icon::default().path(path).xsmall().text_color(command_color))
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
              editor.toggle_inline_tool(ArmedInlineTool::Semantic(RunSemanticStyle::Condensed), cx);
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
          PopupMenuItem::new("Condensed")
            .checked(checked)
            .on_click(move |_, _, cx| {
              editor_for_condensed.update(cx, |editor, cx| {
                editor.toggle_inline_tool(ArmedInlineTool::Semantic(RunSemanticStyle::Condensed), cx);
              });
            }),
        )
        .item(PopupMenuItem::new("Ultracondensed").on_click(move |_, _, cx| {
          editor_for_ultra.update(cx, |editor, cx| {
            editor.toggle_inline_tool(ArmedInlineTool::Semantic(RunSemanticStyle::Ultracondensed), cx);
          });
        }))
    })
    .into_any_element()
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
