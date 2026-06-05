#[hotpath::measure]
fn modern_command_groups(
  state: &RichTextEditorStyleState,
  armed_tool: Option<ArmedInlineTool>,
  document_theme: &DocumentTheme,
  current_highlight: Option<HighlightStyle>,
  highlight_mode_active: bool,
) -> Vec<RibbonCommandGroup> {
  let mut keyed = Vec::new();
  let mut unkeyed = Vec::new();

  keyed.extend(
    paragraph_commands(state)
      .into_iter()
      .filter(|command| command.command_id.is_some()),
  );
  keyed.extend(keyed_inline_commands(state, armed_tool));
  keyed.extend(highlight_commands(document_theme, current_highlight, highlight_mode_active));
  keyed.push(clear_formatting_command("keyed"));
  keyed.sort_by_key(|command| command_sort_key(command.command_id));

  unkeyed.extend(unkeyed_inline_commands(state, armed_tool));

  vec![
    RibbonCommandGroup {
      id: "keyed",
      label: "Keybinds",
      commands: keyed,
    },
    RibbonCommandGroup {
      id: "unkeyed",
      label: "No Keybind",
      commands: unkeyed,
    },
  ]
}

#[hotpath::measure]
fn paragraph_commands(state: &RichTextEditorStyleState) -> Vec<RibbonCommand> {
  PARAGRAPH_STYLE_SPECS
    .iter()
    .filter(|spec| spec.style != ParagraphStyle::Normal)
    .map(|spec| {
      let command_id = paragraph_command_id(spec.style);
      RibbonCommand {
        id: RibbonCommandId::Paragraph(spec.style),
        label: spec.label,
        group_id: "keyed",
        shortcut: command_id.and_then(shortcut_for),
        command_id,
        priority: paragraph_priority(spec.style),
        accent: None,
        selected: EditorRibbon::paragraph_selected(state, spec.style),
        disabled: false,
        overflow_behavior: paragraph_overflow_behavior(spec.style),
        checked_highlight: None,
      }
    })
    .collect()
}

#[hotpath::measure]
fn keyed_inline_commands(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> Vec<RibbonCommand> {
  let mut commands = SEMANTIC_STYLE_SPECS
    .iter()
    .filter(|spec| matches!(spec.style, flowstate_document::SEMANTIC_CITE | flowstate_document::SEMANTIC_EMPHASIS))
    .map(|spec| semantic_command(spec.style, spec.label, "keyed", state, armed_tool))
    .collect::<Vec<_>>();

  commands.push(RibbonCommand {
    id: RibbonCommandId::Underline,
    label: "Underline",
    group_id: "keyed",
    shortcut: shortcut_for(CommandId::ToggleUnderline),
    command_id: Some(CommandId::ToggleUnderline),
    priority: 82,
    accent: None,
    selected: EditorRibbon::underline_selected(state, armed_tool),
    disabled: false,
    overflow_behavior: OverflowBehavior::KeepVisible,
    checked_highlight: None,
  });
  commands
}

#[hotpath::measure]
fn unkeyed_inline_commands(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> Vec<RibbonCommand> {
  vec![
    RibbonCommand {
      id: RibbonCommandId::CondensedMenu,
      label: "Shrink",
      group_id: "unkeyed",
      shortcut: None,
      command_id: None,
      priority: 76,
      accent: None,
      selected: matches!(
        armed_tool,
        Some(ArmedInlineTool::Semantic(flowstate_document::SEMANTIC_CONDENSED | flowstate_document::SEMANTIC_ULTRACONDENSED))
      ) || matches!(
        state.semantic,
        SelectionState::Uniform(flowstate_document::SEMANTIC_CONDENSED | flowstate_document::SEMANTIC_ULTRACONDENSED)
      ),
      disabled: false,
      overflow_behavior: OverflowBehavior::KeepVisible,
      checked_highlight: None,
    },
    RibbonCommand {
      id: RibbonCommandId::Strikethrough,
      label: "Strikethrough",
      group_id: "unkeyed",
      shortcut: None,
      command_id: None,
      priority: 81,
      accent: None,
      selected: EditorRibbon::strikethrough_selected(state, armed_tool),
      disabled: false,
      overflow_behavior: OverflowBehavior::KeepVisible,
      checked_highlight: None,
    },
  ]
}

#[hotpath::measure]
fn semantic_command(
  style: RunSemanticStyle,
  label: &'static str,
  group_id: &'static str,
  state: &RichTextEditorStyleState,
  armed_tool: Option<ArmedInlineTool>,
) -> RibbonCommand {
  let command_id = semantic_command_id(style);
  RibbonCommand {
    id: RibbonCommandId::Semantic(style),
    label,
    group_id,
    shortcut: command_id.and_then(shortcut_for),
    command_id,
    priority: semantic_priority(style),
    accent: None,
    selected: EditorRibbon::semantic_selected(state, armed_tool, style),
    disabled: false,
    overflow_behavior: semantic_overflow_behavior(style),
    checked_highlight: None,
  }
}

#[hotpath::measure]
fn clear_formatting_command(group_id: &'static str) -> RibbonCommand {
  RibbonCommand {
    id: RibbonCommandId::ClearFormatting,
    label: "Clear",
    group_id,
    shortcut: shortcut_for(CommandId::ClearFormatting),
    command_id: Some(CommandId::ClearFormatting),
    priority: 90,
    accent: None,
    selected: false,
    disabled: false,
    overflow_behavior: OverflowBehavior::KeepVisible,
    checked_highlight: None,
  }
}

#[hotpath::measure]
fn highlight_commands(
  document_theme: &DocumentTheme,
  current_highlight: Option<HighlightStyle>,
  highlight_mode_active: bool,
) -> Vec<RibbonCommand> {
  vec![RibbonCommand {
    id: RibbonCommandId::ToggleHighlightMode(current_highlight),
    label: "Highlight",
    group_id: "highlight",
    shortcut: shortcut_for(CommandId::ApplyHighlightToSelection),
    command_id: Some(CommandId::ApplyHighlightToSelection),
    priority: 74,
    accent: Some(match current_highlight {
      Some(highlight) => RibbonAccent::Color(highlight_color(highlight, document_theme)),
      None => RibbonAccent::Transparent,
    }),
    selected: highlight_mode_active,
    disabled: false,
    overflow_behavior: OverflowBehavior::KeepVisible,
    checked_highlight: current_highlight,
  }]
}

