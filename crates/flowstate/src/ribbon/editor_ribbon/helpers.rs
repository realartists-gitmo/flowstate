#[hotpath::measure]
fn highlight_color(style: HighlightStyle, theme: &DocumentTheme) -> Hsla {
  match style {
    flowstate_document::HIGHLIGHT_SPOKEN => flowstate_document::custom_highlight_color(theme, 1),
    flowstate_document::HIGHLIGHT_INSERT => flowstate_document::custom_highlight_color(theme, 2),
    flowstate_document::HIGHLIGHT_ALTERNATIVE => flowstate_document::custom_highlight_color(theme, 3),
    HighlightStyle::Custom(slot) => flowstate_document::custom_highlight_color(theme, slot),
  }
}

#[hotpath::measure]
fn shortcut_for(command_id: CommandId) -> Option<String> {
  default_keys_for(command_id).first().map(|key| {
    Keystroke::parse(key)
      .map(|stroke| Kbd::format(&stroke))
      .unwrap_or_else(|_| (*key).to_string())
  })
}

#[hotpath::measure]
fn show_shortcut(options: ModernRibbonOptions) -> bool {
  match options.shortcut_visibility {
    ShortcutVisibility::Always => true,
    ShortcutVisibility::HideInCompact => options.density == RibbonDensity::Full,
    ShortcutVisibility::HoverOnly | ShortcutVisibility::Hidden => false,
  }
}

#[hotpath::measure]
fn command_tooltip(command: &RibbonCommand) -> String {
  match &command.shortcut {
    Some(shortcut) => format!("{} ({})", command.label, shortcut),
    None => command.label.to_string(),
  }
}

#[hotpath::measure]
fn keycap(shortcut: String, cx: &mut Context<EditorRibbon>) -> AnyElement {
  div()
    .flex_none()
    .whitespace_nowrap()
    .rounded(cx.theme().radius)
    .border_1()
    .border_color(cx.theme().border)
    .bg(cx.theme().muted.opacity(0.68))
    .px_1()
    .py_0p5()
    .text_size(px(10.0))
    .line_height(relative(1.0))
    .text_color(cx.theme().muted_foreground)
    .child(shortcut)
    .into_any_element()
}

#[hotpath::measure]
fn accent_dot(color: Hsla) -> AnyElement {
  div()
    .flex_none()
    .size(px(6.0))
    .rounded(px(3.0))
    .bg(color)
    .into_any_element()
}

#[hotpath::measure]
fn accent_bar(color: Hsla, cx: &mut Context<EditorRibbon>) -> AnyElement {
  div()
    .flex_none()
    .w(px(3.0))
    .h(px(12.0))
    .rounded(cx.theme().radius)
    .bg(color)
    .into_any_element()
}

#[hotpath::measure]
fn transparent_accent_bar(cx: &mut Context<EditorRibbon>) -> AnyElement {
  div()
    .flex_none()
    .w(px(3.0))
    .h(px(12.0))
    .rounded(cx.theme().radius)
    .border_1()
    .border_color(cx.theme().border.opacity(0.62))
    .bg(cx.theme().background.opacity(0.0))
    .into_any_element()
}

#[hotpath::measure]
fn highlight_menu_swatch(color: Hsla) -> AnyElement {
  div()
    .flex_none()
    .size(px(10.0))
    .rounded(px(2.0))
    .border_1()
    .border_color(color.opacity(0.8))
    .bg(color.opacity(0.72))
    .into_any_element()
}

#[hotpath::measure]
fn accent_color(accent: RibbonAccent, cx: &mut Context<EditorRibbon>) -> Hsla {
  match accent {
    RibbonAccent::Blue => cx.theme().blue,
    RibbonAccent::Purple => cx.theme().magenta,
    RibbonAccent::Green => cx.theme().green,
    RibbonAccent::Yellow => cx.theme().yellow,
    RibbonAccent::Gray => cx.theme().muted_foreground,
    RibbonAccent::Transparent => cx.theme().background.opacity(0.0),
    RibbonAccent::Color(color) => color,
  }
}

#[hotpath::measure]
fn ribbon_command_key(command_id: RibbonCommandId) -> u64 {
  match command_id {
    RibbonCommandId::Paragraph(style) => 1_000 + style.slot(),
    RibbonCommandId::Semantic(style) => 2_000 + style.slot(),
    RibbonCommandId::CondensedMenu => 2_900,
    RibbonCommandId::Underline => 3_000,
    RibbonCommandId::Strikethrough => 3_100,
    RibbonCommandId::Highlight(style) => 4_000 + style.slot(),
    RibbonCommandId::ClearHighlight => 5_000,
    RibbonCommandId::HighlightMenu => 5_002,
    RibbonCommandId::ToggleHighlightMode(style) => {
      5_100
        + match style {
          Some(style) => style.slot(),
          None => 999,
        }
    },
    RibbonCommandId::ClearFormatting => 5_001,
  }
}
