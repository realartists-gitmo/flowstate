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
    RibbonCommandId::ToggleSpeechDocument | RibbonCommandId::SendToSpeechDocument => {},
    RibbonCommandId::CondenseMenu | RibbonCommandId::CondensedMenu => {
      editor.toggle_inline_tool(ArmedInlineTool::Semantic(flowstate_document::SEMANTIC_CONDENSED), cx);
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
    RibbonCommandId::MarkCard => {
      editor.set_highlight_from_caret_to_enclosing_section_end(flowstate_document::HIGHLIGHT_MARKED, &[2, 3, 4], cx);
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
    flowstate_document::PARAGRAPH_POCKET => Some(CommandId::SetParagraphPocket),
    flowstate_document::PARAGRAPH_HAT => Some(CommandId::SetParagraphHat),
    flowstate_document::PARAGRAPH_BLOCK => Some(CommandId::SetParagraphBlock),
    flowstate_document::PARAGRAPH_TAG => Some(CommandId::SetParagraphTag),
    flowstate_document::PARAGRAPH_ANALYTIC => Some(CommandId::SetParagraphAnalytic),
    flowstate_document::PARAGRAPH_UNDERTAG => Some(CommandId::SetParagraphUndertag),
    ParagraphStyle::Custom(_) => None,
  }
}

#[hotpath::measure]
fn semantic_command_id(style: RunSemanticStyle) -> Option<CommandId> {
  match style {
    flowstate_document::SEMANTIC_CITE => Some(CommandId::ToggleCite),
    flowstate_document::SEMANTIC_EMPHASIS => Some(CommandId::ToggleEmphasis),
    flowstate_document::SEMANTIC_CONDENSED => None,
    flowstate_document::SEMANTIC_ULTRACONDENSED => None,
    flowstate_document::SEMANTIC_UNDERLINE => Some(CommandId::ToggleUnderline),
    RunSemanticStyle::Plain => Some(CommandId::ClearFormatting),
    RunSemanticStyle::Custom(_) => None,
  }
}

#[hotpath::measure]
fn paragraph_priority(style: ParagraphStyle) -> u8 {
  match style {
    ParagraphStyle::Normal => 100,
    flowstate_document::PARAGRAPH_POCKET => 96,
    flowstate_document::PARAGRAPH_HAT => 94,
    flowstate_document::PARAGRAPH_BLOCK => 92,
    flowstate_document::PARAGRAPH_TAG => 78,
    flowstate_document::PARAGRAPH_ANALYTIC => 76,
    flowstate_document::PARAGRAPH_UNDERTAG => 72,
    ParagraphStyle::Custom(_) => 60,
  }
}

#[hotpath::measure]
fn semantic_priority(style: RunSemanticStyle) -> u8 {
  match style {
    flowstate_document::SEMANTIC_CITE => 92,
    flowstate_document::SEMANTIC_EMPHASIS => 90,
    flowstate_document::SEMANTIC_CONDENSED => 76,
    flowstate_document::SEMANTIC_ULTRACONDENSED => 70,
    flowstate_document::SEMANTIC_UNDERLINE => 82,
    RunSemanticStyle::Plain => 0,
    RunSemanticStyle::Custom(_) => 60,
  }
}

#[hotpath::measure]
fn paragraph_overflow_behavior(style: ParagraphStyle) -> OverflowBehavior {
  match style {
    ParagraphStyle::Normal | flowstate_document::PARAGRAPH_POCKET | flowstate_document::PARAGRAPH_HAT | flowstate_document::PARAGRAPH_BLOCK => {
      OverflowBehavior::KeepVisible
    },
    flowstate_document::PARAGRAPH_TAG | flowstate_document::PARAGRAPH_ANALYTIC | flowstate_document::PARAGRAPH_UNDERTAG => {
      OverflowBehavior::MoveToOverflow
    },
    ParagraphStyle::Custom(_) => OverflowBehavior::MoveToOverflow,
  }
}

#[hotpath::measure]
fn semantic_overflow_behavior(style: RunSemanticStyle) -> OverflowBehavior {
  match style {
    flowstate_document::SEMANTIC_CITE | flowstate_document::SEMANTIC_EMPHASIS | flowstate_document::SEMANTIC_UNDERLINE => {
      OverflowBehavior::KeepVisible
    },
    flowstate_document::SEMANTIC_CONDENSED | flowstate_document::SEMANTIC_ULTRACONDENSED => OverflowBehavior::MoveToOverflow,
    RunSemanticStyle::Plain => OverflowBehavior::HideInCompact,
    RunSemanticStyle::Custom(_) => OverflowBehavior::MoveToOverflow,
  }
}
