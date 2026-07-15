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
    available_width: gpui::Pixels,
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
          shortcut: shortcut_for(CommandId::ToggleSpeechDocument),
          command_id: Some(CommandId::ToggleSpeechDocument),
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

    groups.insert(
      0,
      RibbonCommandGroup {
        id: "history",
        label: "History",
        commands: vec![
          RibbonCommand {
            id: RibbonCommandId::Undo,
            label: "Undo",
            group_id: "history",
            shortcut: shortcut_for(CommandId::Undo),
            command_id: Some(CommandId::Undo),
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
            shortcut: shortcut_for(CommandId::Redo),
            command_id: Some(CommandId::Redo),
            priority: 1,
            accent: None,
            selected: false,
            disabled: false,
            overflow_behavior: OverflowBehavior::KeepVisible,
            checked_highlight: None,
          },
          RibbonCommand {
            id: RibbonCommandId::Revisions,
            label: "Revisions",
            group_id: "history",
            shortcut: None,
            command_id: None,
            priority: 2,
            accent: None,
            selected: false,
            disabled: panel_id.is_none(),
            overflow_behavior: OverflowBehavior::KeepVisible,
            checked_highlight: None,
          },
        ],
      },
    );

    groups.push(RibbonCommandGroup {
      id: "views",
      label: "Views",
      commands: vec![RibbonCommand {
        id: RibbonCommandId::ToggleInvisibility,
        label: "",
        group_id: "views",
        shortcut: shortcut_for(CommandId::ToggleInvisibility),
        command_id: Some(CommandId::ToggleInvisibility),
        priority: 0,
        accent: None,
        selected: invisibility_mode,
        disabled: false,
        overflow_behavior: OverflowBehavior::KeepVisible,
        checked_highlight: None,
      }],
    });

    groups.push(RibbonCommandGroup {
      id: "export",
      label: "Export",
      commands: vec![
        RibbonCommand {
          id: RibbonCommandId::ExportFormat,
          label: "Format",
          group_id: "export",
          shortcut: shortcut_for(CommandId::ExportFormat),
          command_id: Some(CommandId::ExportFormat),
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
          shortcut: shortcut_for(CommandId::ExportSend),
          command_id: Some(CommandId::ExportSend),
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

    // P4-S3: the OverflowBehavior enum is finally consumed. Under width
    // pressure, the lowest-priority MoveToOverflow commands fold into a
    // trailing chevron menu (HideInCompact ones hide outright); KeepVisible
    // never folds. The estimate reuses the same row-width math the wrap
    // layout trusts.
    let mut overflowed: Vec<RibbonCommand> = Vec::new();
    if available_width > px(0.0) {
      let chrome = metrics.outer_padding_x * 2.0 + metrics.inner_padding_x * 2.0 + px(64.0);
      loop {
        let strip: f32 = groups
          .iter()
          .filter(|group| !group.commands.is_empty())
          .map(|group| group_row_width(group, metrics, metrics.max_chip_rows, window, cx).as_f32() + metrics.group_gap.as_f32())
          .sum();
        let more_chip = if overflowed.is_empty() { 0.0 } else { 40.0 };
        if strip + chrome.as_f32() + more_chip <= available_width.as_f32() {
          break;
        }
        // Fold the lowest-priority foldable command anywhere in the strip.
        let candidate = groups
          .iter()
          .enumerate()
          .flat_map(|(group_ix, group)| {
            group
              .commands
              .iter()
              .enumerate()
              .filter(|(_, command)| !matches!(command.overflow_behavior, OverflowBehavior::KeepVisible))
              .map(move |(command_ix, command)| (command.priority, group_ix, command_ix))
          })
          .min_by_key(|(priority, ..)| *priority);
        let Some((_, group_ix, command_ix)) = candidate else {
          break;
        };
        let folded = groups[group_ix].commands.remove(command_ix);
        if matches!(folded.overflow_behavior, OverflowBehavior::MoveToOverflow) {
          overflowed.push(folded);
        }
      }
      groups.retain(|group| !group.commands.is_empty());
    }

    // Use vertical ribbon room proactively. Width pressure can force wrapping,
    // but when there is spare height we still prefer balanced columns over one
    // long horizontal strip.
    let wrap_widths = groups
      .iter()
      .map(|group| {
        let has_wrap = group.id == "speech" || metrics.max_chip_rows > 1;
        has_wrap.then(|| {
          if group.id == "style" {
            group_row_width(group, metrics, 2, window, cx)
          } else if group.id == "history" || group.id == "speech" {
            group_row_width(group, metrics, 1, window, cx)
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
              }))
              .when(!overflowed.is_empty(), |this| {
                this.child(modern_overflow_menu(overflowed, editor.clone(), metrics, workspace.clone(), panel_id, cx))
              }),
          ),
      )
      .into_any_element()
  }
}

/// P4-S3: the trailing chevron holding folded commands; every entry executes
/// through the unified dispatch, so folded and visible chips behave
/// identically.
#[hotpath::measure]
fn modern_overflow_menu(
  overflowed: Vec<RibbonCommand>,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  workspace: Option<WeakEntity<Workspace>>,
  panel_id: Option<Uuid>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let chip_height = metrics.chip_height;
  Button::new("modern-ribbon-overflow")
    .compact()
    .outline()
    .h(chip_height)
    .px(metrics.chip_padding_x)
    .tooltip("More commands")
    .child(Icon::new(IconName::Ellipsis).xsmall().text_color(cx.theme().muted_foreground))
    .dropdown_menu(move |menu, _, _| {
      overflowed.iter().fold(menu, |menu, command| {
        let label = command.label;
        let command_id = command.id;
        let editor = editor.clone();
        let workspace = workspace.clone();
        menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
          perform_ribbon_command_in_workspace(&editor, workspace.as_ref(), panel_id, command_id, window, cx);
        }))
      })
    })
    .into_any_element()
}
