pub struct ModernStylesRibbon;

#[derive(Clone, Copy, Debug)]
struct RibbonLayoutMetrics {
  height: gpui::Pixels,
  chip_height: gpui::Pixels,
  chip_max_width: gpui::Pixels,
  chip_padding_x: gpui::Pixels,
  chip_text_size: gpui::Pixels,
  chip_gap: gpui::Pixels,
  max_chip_rows: usize,
  group_gap: gpui::Pixels,
  group_padding_top: gpui::Pixels,
  outer_padding_x: gpui::Pixels,
  inner_padding_x: gpui::Pixels,
  group_divider_padding_left: gpui::Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RibbonAccent {
  Blue,
  Purple,
  Green,
  Yellow,
  Gray,
  Transparent,
  Color(Hsla),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverflowBehavior {
  KeepVisible,
  MoveToOverflow,
  HideInCompact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RibbonCommandId {
  Paragraph(ParagraphStyle),
  Semantic(RunSemanticStyle),
  CondensedMenu,
  Underline,
  Strikethrough,
  Highlight(HighlightStyle),
  ClearHighlight,
  MarkCard,
  HighlightMenu,
  ToggleHighlightMode(Option<HighlightStyle>),
  ClearFormatting,
}

#[derive(Clone, Debug)]
pub struct RibbonCommand {
  pub id: RibbonCommandId,
  pub label: &'static str,
  pub group_id: &'static str,
  pub shortcut: Option<String>,
  pub command_id: Option<CommandId>,
  pub priority: u8,
  pub accent: Option<RibbonAccent>,
  pub selected: bool,
  pub disabled: bool,
  pub overflow_behavior: OverflowBehavior,
  pub checked_highlight: Option<HighlightStyle>,
}

#[derive(Clone, Debug)]
pub struct RibbonCommandGroup {
  pub id: &'static str,
  pub label: &'static str,
  pub commands: Vec<RibbonCommand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RibbonLabel {
  text: &'static str,
  icon_path: Option<&'static str>,
}

#[hotpath::measure_all]
impl RibbonLabel {
  fn for_command(command: &RibbonCommand) -> Self {
    let icon_path = match command.id {
      RibbonCommandId::Semantic(flowstate_document::SEMANTIC_EMPHASIS) => Some("icons/bold.svg"),
      RibbonCommandId::Underline => Some("icons/underline.svg"),
      RibbonCommandId::Strikethrough => Some("icons/strikethrough.svg"),
      RibbonCommandId::CondensedMenu => Some("icons/shrink.svg"),
      RibbonCommandId::ToggleHighlightMode(_) => Some("icons/highlighter.svg"),
      RibbonCommandId::MarkCard => Some("icons/highlighter.svg"),
      RibbonCommandId::ClearFormatting => Some("icons/eraser.svg"),
      _ => None,
    };
    Self {
      text: command.label,
      icon_path,
    }
  }

  // Future settings should choose whether this renders `icon_path` or `text`
  // so users can switch the ribbon between icon and text label modes.
  fn prefers_icon(self) -> bool {
    self.icon_path.is_some()
  }
}

