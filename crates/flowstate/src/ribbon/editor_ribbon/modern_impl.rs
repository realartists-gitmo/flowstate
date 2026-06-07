#[hotpath::measure_all]
impl ModernStylesRibbon {
  fn render(
    editor: Entity<RichTextEditor>,
    style_state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    document_theme: &DocumentTheme,
    current_highlight: Option<HighlightStyle>,
    highlight_mode_active: bool,
    invisibility_mode: bool,
    options: ModernRibbonOptions,
    height: gpui::Pixels,
    workspace: Option<WeakEntity<Workspace>>,
    panel_id: Option<Uuid>,
    speech_available: bool,
    speech_active: bool,
    speech_send_enabled: bool,
    _available_width: gpui::Pixels,
    window: &mut Window,
    cx: &mut Context<EditorRibbon>,
  ) -> AnyElement {
    let mut groups = modern_command_groups(style_state, armed_tool, document_theme, current_highlight, highlight_mode_active);
    if let Some(group) = groups.iter_mut().find(|group| group.id == "speech") {
      group.commands.insert(
        0,
        RibbonCommand {
          id: RibbonCommandId::ToggleSpeechDocument,
          label: "Speech",
          group_id: "speech",
          shortcut: None,
          command_id: None,
          priority: 73,
          accent: None,
          selected: speech_active,
          disabled: !speech_available,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
      );
      group.commands.insert(
        1,
        RibbonCommand {
          id: RibbonCommandId::SendToSpeechDocument,
          label: "Send",
          group_id: "speech",
          shortcut: shortcut_for(CommandId::SendToSpeechDocument),
          command_id: Some(CommandId::SendToSpeechDocument),
          priority: 74,
          accent: None,
          selected: false,
          disabled: !speech_send_enabled,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
      );
    }

    groups.insert(0, RibbonCommandGroup {
      id: "history",
      label: "History",
      commands: vec![
        RibbonCommand {
          id: RibbonCommandId::Undo,
          label: "Undo",
          group_id: "history",
          shortcut: None,
          command_id: None,
          priority: 0,
          accent: None,
          selected: false,
          disabled: false,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
        RibbonCommand {
          id: RibbonCommandId::Redo,
          label: "Redo",
          group_id: "history",
          shortcut: None,
          command_id: None,
          priority: 1,
          accent: None,
          selected: false,
          disabled: false,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
      ],
    });

    groups.push(RibbonCommandGroup {
      id: "views",
      label: "Views",
      commands: vec![
        RibbonCommand {
          id: RibbonCommandId::ToggleInvisibility,
          label: "",
          group_id: "views",
          shortcut: None,
          command_id: None,
          priority: 0,
          accent: None,
          selected: invisibility_mode,
          disabled: false,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
      ],
    });

    groups.push(RibbonCommandGroup {
      id: "export",
      label: "Export",
      commands: vec![
        RibbonCommand {
          id: RibbonCommandId::ExportFormat,
          label: "Format",
          group_id: "export",
          shortcut: None,
          command_id: None,
          priority: 0,
          accent: None,
          selected: false,
          disabled: false,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
        RibbonCommand {
          id: RibbonCommandId::ExportSend,
          label: "Send",
          group_id: "export",
          shortcut: None,
          command_id: None,
          priority: 1,
          accent: None,
          selected: false,
          disabled: false,
          overflow_behavior: OverflowBehavior::KeepVisible,
          checked_highlight: None,
        },
      ],
    });

    let metrics = RibbonLayoutMetrics::from_height(height);
    // Use vertical ribbon room proactively. Width pressure can force wrapping,
    // but when there is spare height we still prefer balanced columns over one
    // long horizontal strip.
    let wrap_widths = groups
      .iter()
      .map(|group| {
        (metrics.max_chip_rows > 1).then(|| {
          if group.id == "style" {
            group_row_width(group, metrics, 2, window, cx)
          } else if group.id == "history" {
            group_row_width(group, metrics, 1, window, cx)
          } else if group.id == "speech" {
            balanced_group_width(group, metrics, 2, window, cx)
          } else if group.id == "views" {
            let label_width = measure_ribbon_text("Views", metrics.chip_text_size, window, cx).as_f32();
            let cmd_width = group_row_width(group, metrics, metrics.max_chip_rows, window, cx).as_f32();
            px(label_width.max(cmd_width))
          } else {
            group_row_width(group, metrics, metrics.max_chip_rows, window, cx)
          }
        })
      })
      .collect::<Vec<_>>();

    div()
      .w_full()
      .h(metrics.height)
      .min_h(min_ribbon_height())
      .px(metrics.outer_padding_x)
      .pt_0()
      .pb_0()
      .child(
        div()
          .w_full()
          .min_w_0()
          .flex()
          .flex_row()
          .flex_nowrap()
          .items_start()
          .gap_2()
          .bg(cx.theme().background)
          .px(metrics.inner_padding_x)
          .pt(metrics.group_padding_top)
          .pb(px(1.0))
          .child(
            div()
              .flex()
              .flex_none()
              .flex_row()
              .flex_nowrap()
          .gap(metrics.group_gap)
          .min_w_0()
              .children(groups.iter().enumerate().map(|(index, group)| {
                modern_group(
                  index > 0,
                  group,
                  editor.clone(),
                  document_theme,
                  options,
                  metrics,
                  wrap_widths[index],
                  workspace.clone(),
                  panel_id,
                  cx,
                )
              })),
          ),
      )
      .into_any_element()
  }
}
