#[hotpath::measure]
fn command_sort_key(command_id: Option<CommandId>) -> u16 {
  match command_id {
    Some(CommandId::SetParagraphPocket) => 400,
    Some(CommandId::SetParagraphHat) => 500,
    Some(CommandId::SetParagraphBlock) => 600,
    Some(CommandId::SetParagraphTag) => 700,
    Some(CommandId::SetParagraphAnalytic) => 701,
    Some(CommandId::ToggleCite) => 800,
    Some(CommandId::SetParagraphUndertag) => 801,
    Some(CommandId::ToggleUnderline) => 900,
    Some(CommandId::ToggleEmphasis) => 1000,
    Some(CommandId::ApplyHighlightToSelection) => 1100,
    Some(CommandId::ClearFormatting) => 1200,
    Some(_) => 9000,
    None => 9500,
  }
}

#[hotpath::measure]
fn perform_ribbon_command(editor: &mut RichTextEditor, command_id: RibbonCommandId, cx: &mut Context<RichTextEditor>) {
  match command_id {
    RibbonCommandId::Paragraph(style) => {
      editor.set_paragraph_style_for_selection(style, cx);
    },
    RibbonCommandId::Semantic(style) => {
      editor.toggle_inline_tool(ArmedInlineTool::Semantic(style), cx);
    },
    RibbonCommandId::CondensedMenu => {
      editor.toggle_inline_tool(ArmedInlineTool::Semantic(RunSemanticStyle::Condensed), cx);
    },
    RibbonCommandId::Underline => {
      editor.toggle_inline_tool(ArmedInlineTool::Underline, cx);
    },
    RibbonCommandId::Strikethrough => {
      editor.toggle_inline_tool(ArmedInlineTool::Strikethrough, cx);
    },
    RibbonCommandId::Highlight(style) => {
      editor.toggle_inline_tool(ArmedInlineTool::Highlight(style), cx);
    },
    RibbonCommandId::ToggleHighlightMode(_) => {
      editor.toggle_highlight_mode(cx);
    },
    RibbonCommandId::ClearHighlight => {
      editor.clear_armed_inline_tool(cx);
      editor.set_highlight_for_selection(None, cx);
    },
    RibbonCommandId::HighlightMenu => {},
    RibbonCommandId::ClearFormatting => {
      editor.clear_formatting(cx);
    },
  }
}

#[hotpath::measure]
fn paragraph_command_id(style: ParagraphStyle) -> Option<CommandId> {
  match style {
    ParagraphStyle::Normal => None,
    ParagraphStyle::Pocket => Some(CommandId::SetParagraphPocket),
    ParagraphStyle::Hat => Some(CommandId::SetParagraphHat),
    ParagraphStyle::Block => Some(CommandId::SetParagraphBlock),
    ParagraphStyle::Tag => Some(CommandId::SetParagraphTag),
    ParagraphStyle::Analytic => Some(CommandId::SetParagraphAnalytic),
    ParagraphStyle::Undertag => Some(CommandId::SetParagraphUndertag),
  }
}

#[hotpath::measure]
fn semantic_command_id(style: RunSemanticStyle) -> Option<CommandId> {
  match style {
    RunSemanticStyle::Cite => Some(CommandId::ToggleCite),
    RunSemanticStyle::Emphasis => Some(CommandId::ToggleEmphasis),
    RunSemanticStyle::Condensed => None,
    RunSemanticStyle::Ultracondensed => None,
    RunSemanticStyle::Underline => Some(CommandId::ToggleUnderline),
    RunSemanticStyle::Plain => Some(CommandId::ClearFormatting),
  }
}

#[hotpath::measure]
fn paragraph_priority(style: ParagraphStyle) -> u8 {
  match style {
    ParagraphStyle::Normal => 100,
    ParagraphStyle::Pocket => 96,
    ParagraphStyle::Hat => 94,
    ParagraphStyle::Block => 92,
    ParagraphStyle::Tag => 78,
    ParagraphStyle::Analytic => 76,
    ParagraphStyle::Undertag => 72,
  }
}

#[hotpath::measure]
fn semantic_priority(style: RunSemanticStyle) -> u8 {
  match style {
    RunSemanticStyle::Cite => 92,
    RunSemanticStyle::Emphasis => 90,
    RunSemanticStyle::Condensed => 76,
    RunSemanticStyle::Ultracondensed => 70,
    RunSemanticStyle::Underline => 82,
    RunSemanticStyle::Plain => 0,
  }
}

#[hotpath::measure]
fn paragraph_overflow_behavior(style: ParagraphStyle) -> OverflowBehavior {
  match style {
    ParagraphStyle::Normal | ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block => OverflowBehavior::KeepVisible,
    ParagraphStyle::Tag | ParagraphStyle::Analytic | ParagraphStyle::Undertag => OverflowBehavior::MoveToOverflow,
  }
}

#[hotpath::measure]
fn semantic_overflow_behavior(style: RunSemanticStyle) -> OverflowBehavior {
  match style {
    RunSemanticStyle::Cite | RunSemanticStyle::Emphasis | RunSemanticStyle::Underline => OverflowBehavior::KeepVisible,
    RunSemanticStyle::Condensed | RunSemanticStyle::Ultracondensed => OverflowBehavior::MoveToOverflow,
    RunSemanticStyle::Plain => OverflowBehavior::HideInCompact,
  }
}

