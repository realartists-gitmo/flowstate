#[hotpath::measure]
fn modern_group(
  has_divider: bool,
  group: &RibbonCommandGroup,
  editor: Entity<RichTextEditor>,
  document_theme: &DocumentTheme,
  options: ModernRibbonOptions,
  metrics: RibbonLayoutMetrics,
  wrap_width: Option<gpui::Pixels>,
  workspace: Option<WeakEntity<Workspace>>,
  panel_id: Option<Uuid>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  div()
    .flex()
    .flex_row()
    .flex_none()
    .when_some(wrap_width, |this, wrap_width| this.w(group_outer_width(wrap_width, has_divider, metrics)))
    .gap_2()
    .when(has_divider, |this| {
      this
        .pl(metrics.group_divider_padding_left)
        .border_l_1()
        .border_color(cx.theme().border.opacity(0.72))
    })
    .child(
      div()
        .w_full()
        .flex()
        .flex_col()
        .min_w_0()
        .gap_0p5()
        .child(
          div()
            .text_size(px(10.0))
            .font_medium()
            .text_color(cx.theme().foreground)
            .child(group.label),
        )
        .child(
          div()
            .id(group.id)
            .flex()
            .flex_row()
            .when(metrics.max_chip_rows == 1, |this| this.flex_nowrap())
            .when(metrics.max_chip_rows > 1, |this| this.flex_wrap())
            .items_center()
            .content_start()
            .gap(metrics.chip_gap)
            .when_some(wrap_width, |this, wrap_width| this.w(wrap_width))
            .children(group.commands.iter().map(|command| {
              if matches!(command.id, RibbonCommandId::ToggleHighlightMode(_)) {
                modern_highlight_menu(command, editor.clone(), document_theme, metrics, cx)
              } else if matches!(command.id, RibbonCommandId::CondenseMenu) {
                modern_condense_menu(command, editor.clone(), metrics, cx)
              } else if matches!(command.id, RibbonCommandId::CondensedMenu) {
                modern_condensed_menu(command, editor.clone(), metrics, cx)
              } else if matches!(command.id, RibbonCommandId::Undo) {
                modern_undo_button(command, editor.clone(), metrics, cx)
              } else if matches!(command.id, RibbonCommandId::Redo) {
                modern_redo_button(command, editor.clone(), metrics, cx)
              } else if matches!(command.id, RibbonCommandId::ExportFormat) {
                modern_export_format(command, editor.clone(), metrics, cx)
              } else if matches!(command.id, RibbonCommandId::ExportSend) {
                modern_export_send(command, editor.clone(), metrics, cx)
              } else if matches!(command.id, RibbonCommandId::ToggleInvisibility) {
                modern_invisibility_toggle(command, editor.clone(), metrics, cx)
              } else {
                modern_command_chip(command, editor.clone(), options, metrics, workspace.clone(), panel_id, cx)
              }
            })),
        ),
    )
    .into_any_element()
}

#[hotpath::measure]
fn chip_rows_for_height(
  height: gpui::Pixels,
  chip_height: gpui::Pixels,
  chip_gap: gpui::Pixels,
  group_padding_top: gpui::Pixels,
  group_label_height: gpui::Pixels,
  group_body_gap: gpui::Pixels,
  group_bottom_guard: gpui::Pixels,
) -> usize {
  // Match the actual modern group stack: top padding + label + label/body gap
  // + N chip rows + row gaps + bottom guard. If this ever overestimates rows,
  // bottom-row buttons clip during ribbon resizing.
  let fixed_height = group_padding_top.as_f32() + group_label_height.as_f32() + group_body_gap.as_f32() + group_bottom_guard.as_f32();
  let available_for_chips = (height.as_f32() - fixed_height).max(0.0);
  for rows in (1_usize..=3).rev() {
    let needed = chip_height.as_f32() * rows as f32 + chip_gap.as_f32() * rows.saturating_sub(1) as f32;
    if available_for_chips >= needed {
      return rows;
    }
  }
  1
}

#[hotpath::measure]
fn command_columns(commands: &[RibbonCommand], max_rows: usize) -> Vec<Vec<&RibbonCommand>> {
  let rows = max_rows.max(1);
  commands
    .chunks(rows)
    .map(|chunk| chunk.iter().collect())
    .collect()
}

#[hotpath::measure]
fn group_row_width(
  group: &RibbonCommandGroup,
  metrics: RibbonLayoutMetrics,
  rows: usize,
  window: &mut Window,
  cx: &mut Context<EditorRibbon>,
) -> gpui::Pixels {
  let columns = command_columns(&group.commands, rows);
  let commands_width = columns
    .iter()
    .map(|column| {
      column
        .iter()
        .map(|command| command_chip_width(command, metrics, window, cx).as_f32())
        .fold(0.0, f32::max)
    })
    .sum::<f32>();
  let gap_width = metrics.chip_gap.as_f32() * columns.len().saturating_sub(1) as f32;

  px(commands_width + gap_width)
}

#[hotpath::measure]
fn balanced_group_width(
  group: &RibbonCommandGroup,
  metrics: RibbonLayoutMetrics,
  max_rows: usize,
  window: &mut Window,
  cx: &mut Context<EditorRibbon>,
) -> gpui::Pixels {
  let rows = max_rows.clamp(1, 3).min(group.commands.len().max(1));
  if rows <= 1 {
    return group_row_width(group, metrics, 1, window, cx);
  }

  // Use the same chunking model as the rendered command order. This width is
  // the sum of each column's widest button, so GPUI's flex-wrap has enough
  // horizontal room to keep the visual row count at or below `rows`.
  group_row_width(group, metrics, rows, window, cx)
}

#[hotpath::measure]
fn command_chip_width(
  command: &RibbonCommand,
  metrics: RibbonLayoutMetrics,
  window: &mut Window,
  cx: &mut Context<EditorRibbon>,
) -> gpui::Pixels {
  match command.id {
    RibbonCommandId::Undo | RibbonCommandId::Redo | RibbonCommandId::ToggleInvisibility => {
      px(metrics.chip_height.as_f32())
    },
    RibbonCommandId::ExportFormat => {
      let text_width = measure_ribbon_text("Format", metrics.chip_text_size, window, cx).as_f32();
      px(text_width + 10.0 + metrics.chip_padding_x.as_f32() * 2.0 + 10.0)
    },
    RibbonCommandId::ExportSend => {
      let text_width = measure_ribbon_text("Send", metrics.chip_text_size, window, cx).as_f32();
      px(text_width + 10.0 + metrics.chip_padding_x.as_f32() * 2.0 + 10.0)
    },
    _ => {
      let label = RibbonLabel::for_command(command);
      let label_width = if label.prefers_icon() {
        0.0
      } else {
        measure_ribbon_text(label.text, metrics.chip_text_size, window, cx).as_f32()
      };
      let icon_width = label
        .icon_path
        .map(|_| metrics.chip_height.as_f32())
        .unwrap_or(0.0);
      let shortcut_width = command
        .shortcut
        .as_ref()
        .map(|shortcut| measure_ribbon_text(shortcut, px(10.0), window, cx).as_f32() + 16.0)
        .unwrap_or(0.0);
      let accent_width = if command.accent.is_some() { 14.0 } else { 0.0 };
      let component_padding_x = px(4.0);
      let caret_width = if matches!(command.id, RibbonCommandId::HighlightMenu | RibbonCommandId::CondensedMenu) {
        10.0
      } else {
        0.0
      };
      let chrome_width = metrics.chip_padding_x.as_f32() * 2.0 + component_padding_x.as_f32() * 2.0 + 10.0 + caret_width;

      px(label_width.max(icon_width) + shortcut_width + accent_width + chrome_width)
    },
  }
}

#[hotpath::measure]
fn measure_ribbon_text(text: &str, font_size: gpui::Pixels, window: &mut Window, _cx: &mut App) -> gpui::Pixels {
  if text.is_empty() {
    return px(0.0);
  }
  let text_style = window.text_style();
  let runs = vec![text_style.to_run(text.len())];
  window
    .text_system()
    .layout_line(text, font_size, &runs, None)
    .width
}

#[hotpath::measure]
fn modern_command_chip(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  options: ModernRibbonOptions,
  metrics: RibbonLayoutMetrics,
  workspace: Option<WeakEntity<Workspace>>,
  panel_id: Option<Uuid>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let command_id = command.id;
  let tooltip = command_tooltip(command);
  let shortcut = command.shortcut.clone();
  let label = RibbonLabel::for_command(command);
  let icon_path = label.prefers_icon().then_some(label.icon_path).flatten();
  let command_color = ribbon_command_color(command, cx);

  Button::new(("modern-ribbon-command", ribbon_command_key(command_id)))
    .xsmall()
    .compact()
    .outline()
    .h(metrics.chip_height)
    .max_w(metrics.chip_max_width)
    .px(metrics.chip_padding_x)
    .rounded(cx.theme().radius)
    .selected(command.selected)
    .disabled(command.disabled)
    .text_color(command_color)
    .tooltip(tooltip)
    .when(command.selected, |this| {
      this
        .border_color(command_color)
        .bg(command_color.opacity(0.18))
        .text_color(command_color)
    })
    .when_some(command.accent, |this, accent| this.child(accent_dot(accent_color(accent, cx))))
    .when_some(icon_path, |this, path| {
      this.child(
        Icon::default()
          .path(path)
          .xsmall()
          .text_color(command_color),
      )
    })
    .when(icon_path.is_none(), |this| {
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
    .when(show_shortcut(options), |this| {
      this.when_some(shortcut, |this, shortcut| this.child(keycap(shortcut, cx)))
    })
    .on_click(move |_, window, cx| match command_id {
      RibbonCommandId::ToggleSpeechDocument => {
        if let (Some(workspace), Some(panel_id)) = (workspace.clone(), panel_id) {
          let _ = workspace.update(cx, |workspace, cx| workspace.toggle_speech_document(panel_id, cx));
        }
      },
      RibbonCommandId::SendToSpeechDocument => {
        if let Some(workspace) = workspace.clone() {
          let _ = workspace.update(cx, |workspace, cx| workspace.send_selection_to_speech_document(window, cx));
        }
      },
      _ => {
        editor.update(cx, |editor, cx| {
          perform_ribbon_command(editor, command_id, cx);
        });
      },
    })
    .into_any_element()
}

fn ribbon_command_color(command: &RibbonCommand, cx: &App) -> Hsla {
  match command.id {
    RibbonCommandId::Paragraph(_) => cx.theme().primary,
    RibbonCommandId::Semantic(_) | RibbonCommandId::Underline | RibbonCommandId::Strikethrough => cx.theme().link,
    RibbonCommandId::Highlight(_) | RibbonCommandId::HighlightMenu | RibbonCommandId::ToggleHighlightMode(_) | RibbonCommandId::MarkCard => {
      cx.theme().warning
    },
    RibbonCommandId::ClearHighlight | RibbonCommandId::ClearFormatting => cx.theme().danger,
    RibbonCommandId::CondenseMenu | RibbonCommandId::CondensedMenu | RibbonCommandId::SendToSpeechDocument => cx.theme().info,
    RibbonCommandId::ToggleSpeechDocument => cx.theme().success,
    RibbonCommandId::Undo | RibbonCommandId::Redo => cx.theme().primary,
    RibbonCommandId::ExportFormat | RibbonCommandId::ExportSend => cx.theme().info,
    RibbonCommandId::ToggleInvisibility => cx.theme().primary,
  }
}
