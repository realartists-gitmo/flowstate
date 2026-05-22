use gpui::{
  AnyElement, App, Context, Edges, Entity, Hsla, IntoElement, Keystroke, ParentElement as _, Render, Styled as _, Window, div, prelude::*, px,
  relative,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _, Toggle, ToggleVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::button::DropdownButton;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, PixelsExt as _, Selectable as _, Sizable as _, StyledExt as _};
use gpui_component::Size;
use serde::{Deserialize, Serialize};

use crate::commands::{CommandId, default_keys_for};
use crate::ribbon::style_catalog::{HIGHLIGHT_STYLE_SPECS, PARAGRAPH_STYLE_SPECS, SEMANTIC_STYLE_SPECS};
use crate::rich_text_element::{
  ApplyHighlightToSelection, ArmedInlineTool, DocumentTheme, HighlightStyle, ParagraphStyle, RichTextEditor, RichTextEditorStyleState,
  RunSemanticStyle, SelectionState,
};

/// User-selectable ribbon renderer.
///
/// This enum is intentionally serializable so a future settings panel can save
/// `editor.ribbon_mode` without touching the render implementations.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RibbonMode {
  Legacy,
  #[default]
  Modern,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RibbonDensity {
  Full,
  Compact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutVisibility {
  Always,
  HideInCompact,
  HoverOnly,
  Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModernRibbonOptions {
  pub density: RibbonDensity,
  pub shortcut_visibility: ShortcutVisibility,
}

impl Default for ModernRibbonOptions {
  fn default() -> Self {
    Self {
      density: RibbonDensity::Full,
      shortcut_visibility: ShortcutVisibility::Always,
    }
  }
}

/// Switching layer for the editor styles ribbon.
///
/// `LegacyStylesRibbon` and `ModernStylesRibbon` stay separate so the old
/// ribbon can be restored by changing only this mode value.
pub struct EditorRibbon {
  editor: Entity<RichTextEditor>,
  mode: RibbonMode,
  modern_options: ModernRibbonOptions,
  height: gpui::Pixels,
}

/// Compatibility name for code that wants to talk in settings terms.
pub type StylesRibbon = EditorRibbon;

impl EditorRibbon {
  pub fn new(editor: Entity<RichTextEditor>) -> Self {
    Self::new_with_mode(editor, RibbonMode::default())
  }

  pub fn new_with_mode(editor: Entity<RichTextEditor>, mode: RibbonMode) -> Self {
    Self {
      editor,
      mode,
      modern_options: ModernRibbonOptions::default(),
      height: default_ribbon_height(),
    }
  }

  pub fn mode(&self) -> RibbonMode {
    self.mode
  }

  /// Future settings panels can call this after updating
  /// `settings.editor.ribbon_mode`.
  pub fn set_mode(&mut self, mode: RibbonMode, cx: &mut Context<Self>) {
    if self.mode != mode {
      self.mode = mode;
      cx.notify();
    }
  }

  pub fn set_modern_options(&mut self, modern_options: ModernRibbonOptions, cx: &mut Context<Self>) {
    if self.modern_options != modern_options {
      self.modern_options = modern_options;
      cx.notify();
    }
  }

  pub fn set_height(&mut self, height: gpui::Pixels, cx: &mut Context<Self>) {
    if self.height != height {
      self.height = height;
      cx.notify();
    }
  }

  fn paragraph_selected(state: &RichTextEditorStyleState, style: ParagraphStyle) -> bool {
    matches!(state.paragraph_style, SelectionState::Uniform(current) if current == style)
  }

  fn semantic_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>, style: RunSemanticStyle) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Semantic(current)) if current == style)
      || matches!(state.semantic, SelectionState::Uniform(current) if current == style)
  }

  fn underline_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Underline)) || matches!(state.underline, SelectionState::Uniform(true))
  }

  fn strikethrough_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Strikethrough)) || matches!(state.strikethrough, SelectionState::Uniform(true))
  }

  fn highlight_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>, style: HighlightStyle) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Highlight(current)) if current == style)
      || matches!(state.highlight, SelectionState::Uniform(Some(current)) if current == style)
  }
}

impl Render for EditorRibbon {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let (style_state, armed_tool, document_theme, current_highlight, highlight_mode_active) = {
      let editor = self.editor.read(cx);
      (
        editor.style_state(),
        editor.armed_inline_tool(),
        editor.document_theme(),
        editor.current_highlight_style(),
        editor.highlight_mode_active(),
      )
    };

    match self.mode {
      RibbonMode::Legacy => LegacyStylesRibbon::render(self.editor.clone(), &style_state, armed_tool, cx),
      RibbonMode::Modern => ModernStylesRibbon::render(
        self.editor.clone(),
        &style_state,
        armed_tool,
        &document_theme,
        current_highlight,
        highlight_mode_active,
        self.modern_options,
        self.height,
        window.viewport_size().width,
        window,
        cx,
      ),
    }
  }
}

pub struct LegacyStylesRibbon;

impl LegacyStylesRibbon {
  fn render(
    editor: Entity<RichTextEditor>,
    style_state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    cx: &mut Context<EditorRibbon>,
  ) -> AnyElement {
    div()
      .w_full()
      .flex()
      .flex_row()
      .items_center()
      .gap_3()
      .px_3()
      .py_2()
      .border_b_1()
      .border_color(cx.theme().border)
      .bg(cx.theme().secondary)
      .child(legacy_ribbon_group(
        "Styles",
        ButtonGroup::new("paragraph-styles")
          .compact()
          .outline()
          .children(PARAGRAPH_STYLE_SPECS.iter().map(|spec| {
            let editor = editor.clone();
            let style = spec.style;
            Button::new(("paragraph-style", style as u64))
              .label(spec.label)
              .selected(EditorRibbon::paragraph_selected(style_state, style))
              .tooltip(spec.label)
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.set_paragraph_style_for_selection(style, cx);
                });
              })
          })),
        cx,
      ))
      .child(legacy_ribbon_group(
        "Inline",
        div()
          .flex()
          .flex_row()
          .items_center()
          .gap_1()
          .children(SEMANTIC_STYLE_SPECS.iter().map(|spec| {
            let editor = editor.clone();
            let style = spec.style;
            Toggle::new(("semantic-style", style as u64))
              .label(spec.label)
              .small()
              .outline()
              .checked(EditorRibbon::semantic_selected(style_state, armed_tool, style))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.toggle_inline_tool(ArmedInlineTool::Semantic(style), cx);
                });
              })
          }))
          .child({
            let editor = editor.clone();
            Toggle::new("underline-style")
              .label("Underline")
              .small()
              .outline()
              .checked(EditorRibbon::underline_selected(style_state, armed_tool))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.toggle_inline_tool(ArmedInlineTool::Underline, cx);
                });
              })
          })
          .child({
            let editor = editor.clone();
            Toggle::new("strikethrough-style")
              .label("Strike")
              .small()
              .outline()
              .checked(EditorRibbon::strikethrough_selected(style_state, armed_tool))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.toggle_inline_tool(ArmedInlineTool::Strikethrough, cx);
                });
              })
          }),
        cx,
      ))
      .child(legacy_ribbon_group(
        "Highlight",
        div()
          .flex()
          .flex_row()
          .items_center()
          .gap_1()
          .children(HIGHLIGHT_STYLE_SPECS.iter().map(|spec| {
            let editor = editor.clone();
            let highlight = spec.style;
            Toggle::new(("highlight-style", highlight as u64))
              .label(spec.label)
              .small()
              .outline()
              .checked(EditorRibbon::highlight_selected(style_state, armed_tool, highlight))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.toggle_inline_tool(ArmedInlineTool::Highlight(highlight), cx);
                });
              })
          }))
          .child({
            let editor = editor.clone();
            Button::new("clear-highlight")
              .label("Clear")
              .small()
              .ghost()
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.clear_armed_inline_tool(cx);
                  editor.set_highlight_for_selection(None, cx);
                });
              })
          }),
        cx,
      ))
      .child(legacy_ribbon_group(
        "Reset",
        {
          let editor = editor.clone();
          Button::new("clear-formatting")
            .label("Clear Formatting")
            .small()
            .ghost()
            .on_click(move |_, _, cx| {
              editor.update(cx, |editor, cx| {
                editor.clear_formatting(cx);
              });
            })
        },
        cx,
      ))
      .into_any_element()
  }
}

fn legacy_ribbon_group(label: &'static str, controls: impl IntoElement, cx: &mut Context<EditorRibbon>) -> impl IntoElement {
  div()
    .h_full()
    .flex()
    .flex_col()
    .gap_1()
    .child(
      div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label),
    )
    .child(controls)
}

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
  Underline,
  Strikethrough,
  Highlight(HighlightStyle),
  ClearHighlight,
  HighlightMenu,
  ToggleHighlightMode(HighlightStyle),
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
}

#[derive(Clone, Debug)]
pub struct RibbonCommandGroup {
  pub id: &'static str,
  pub label: &'static str,
  pub commands: Vec<RibbonCommand>,
}

impl ModernStylesRibbon {
  fn render(
    editor: Entity<RichTextEditor>,
    style_state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    document_theme: &DocumentTheme,
    current_highlight: HighlightStyle,
    highlight_mode_active: bool,
    options: ModernRibbonOptions,
    height: gpui::Pixels,
    available_width: gpui::Pixels,
    window: &mut Window,
    cx: &mut Context<EditorRibbon>,
  ) -> AnyElement {
    let groups = modern_command_groups(style_state, armed_tool, document_theme, current_highlight, highlight_mode_active);
    let mut metrics = RibbonLayoutMetrics::from_height(height);
    let rows_allowed_by_height = metrics.max_chip_rows;
    let rows_requested_by_width = rows_that_fit_width(&groups, metrics, available_width, window, cx);
    // Width can ask for more rows, but height decides whether those rows can
    // actually be displayed without clipping into the document tabs below.
    metrics.max_chip_rows = rows_requested_by_width.min(rows_allowed_by_height);
    let wrap_widths = groups
      .iter()
      .map(|group| {
        (metrics.max_chip_rows > 1).then(|| group_row_width(group, metrics, metrics.max_chip_rows, window, cx))
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
              .flex_1()
              .flex_row()
              .flex_nowrap()
              .items_start()
              .justify_between()
              .gap(metrics.group_gap)
              .min_w_0()
              .children(
                groups
                  .iter()
                  .enumerate()
                  .map(|(index, group)| modern_group(index > 0, group, editor.clone(), document_theme, options, metrics, wrap_widths[index], cx)),
              ),
          ),
      )
      .into_any_element()
  }
}

impl RibbonLayoutMetrics {
  fn from_height(height: gpui::Pixels) -> Self {
    let height = clamp_pixels(height, min_ribbon_height(), max_ribbon_height());
    let scale =
      ((height.as_f32() - min_ribbon_height().as_f32()) / (max_ribbon_height().as_f32() - min_ribbon_height().as_f32())).clamp(0.0, 1.0);
    let group_padding_top = px(3.0 + 3.0 * scale);
    let chip_gap = px(2.0 + 4.0 * scale);
    let chip_height = px(20.0 + 10.0 * scale);
    let max_chip_rows = chip_rows_for_height(height, chip_height, chip_gap, group_padding_top);
    let outer_padding_x = px(8.0);
    let inner_padding_x = px(8.0);
    let group_divider_padding_left = px(8.0);

    Self {
      height,
      chip_height,
      chip_max_width: px(112.0 + 40.0 * scale),
      chip_padding_x: px(3.0 + 7.0 * scale),
      chip_text_size: px(9.5 + 3.0 * scale),
      chip_gap,
      max_chip_rows,
      group_gap: px(4.0 + 7.0 * scale),
      group_padding_top,
      outer_padding_x,
      inner_padding_x,
      group_divider_padding_left,
    }
  }
}

fn default_ribbon_height() -> gpui::Pixels {
  px(112.0)
}

fn min_ribbon_height() -> gpui::Pixels {
  px(56.0)
}

fn max_ribbon_height() -> gpui::Pixels {
  px(158.0)
}

fn clamp_pixels(value: gpui::Pixels, min: gpui::Pixels, max: gpui::Pixels) -> gpui::Pixels {
  px(value.as_f32().clamp(min.as_f32(), max.as_f32()))
}

fn modern_group(
  has_divider: bool,
  group: &RibbonCommandGroup,
  editor: Entity<RichTextEditor>,
  document_theme: &DocumentTheme,
  options: ModernRibbonOptions,
  metrics: RibbonLayoutMetrics,
  wrap_width: Option<gpui::Pixels>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  div()
    .flex()
    .flex_row()
    .flex_none()
    .flex_grow()
    .min_w_0()
    .when_some(wrap_width, |this, wrap_width| this.max_w(wrap_width))
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
            .text_color(cx.theme().muted_foreground)
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
            .min_w_0()
            .children(
              group
                .commands
                .iter()
                .map(|command| {
                  if matches!(command.id, RibbonCommandId::ToggleHighlightMode(_)) {
                    modern_highlight_menu(command, editor.clone(), document_theme, metrics, cx)
                  } else {
                    modern_command_chip(command, editor.clone(), options, metrics, cx)
                  }
                }),
            ),
        ),
    )
    .into_any_element()
}

fn chip_rows_for_height(height: gpui::Pixels, chip_height: gpui::Pixels, chip_gap: gpui::Pixels, group_padding_top: gpui::Pixels) -> usize {
  // The label, top padding, and bottom breathing room are fixed vertical costs
  // before chips can stack. Calculate this instead of using a magic threshold
  // so the default 112px ribbon can wrap to two rows when the window is narrow.
  let fixed_height = group_padding_top.as_f32() + 10.0 + 3.0 + 2.0;
  let available_for_chips = (height.as_f32() - fixed_height).max(0.0);
  let one_row = chip_height.as_f32();
  let two_rows = chip_height.as_f32() * 2.0 + chip_gap.as_f32();
  let three_rows = chip_height.as_f32() * 3.0 + chip_gap.as_f32() * 2.0;

  if available_for_chips >= three_rows {
    3
  } else if available_for_chips >= two_rows {
    2
  } else if available_for_chips >= one_row {
    1
  } else {
    1
  }
}

fn command_columns(commands: &[RibbonCommand], max_rows: usize) -> Vec<Vec<&RibbonCommand>> {
  let rows = max_rows.max(1);
  commands
    .chunks(rows)
    .map(|chunk| chunk.iter().collect())
    .collect()
}

fn rows_that_fit_width(
  groups: &[RibbonCommandGroup],
  metrics: RibbonLayoutMetrics,
  available_width: gpui::Pixels,
  window: &mut Window,
  cx: &mut Context<EditorRibbon>,
) -> usize {
  // Keep room for the collapse button overlay.
  let available_width = (available_width - metrics.outer_padding_x * 2.0 - metrics.inner_padding_x * 2.0 - px(48.0)).max(px(0.0));
  let max_rows = groups
    .iter()
    .map(|group| group.commands.len())
    .max()
    .unwrap_or(1)
    .min(3)
    .max(1);

  (1..=max_rows)
    .find(|rows| total_group_row_width(groups, metrics, *rows, window, cx) <= available_width)
    .unwrap_or(max_rows)
}

fn total_group_row_width(
  groups: &[RibbonCommandGroup],
  metrics: RibbonLayoutMetrics,
  rows: usize,
  window: &mut Window,
  cx: &mut Context<EditorRibbon>,
) -> gpui::Pixels {
  let group_widths = groups
    .iter()
    .map(|group| group_row_width(group, metrics, rows, window, cx).as_f32())
    .sum::<f32>();
  let group_gaps = metrics.group_gap.as_f32() * groups.len().saturating_sub(1) as f32;
  let divider_padding = metrics.group_divider_padding_left.as_f32() * groups.len().saturating_sub(1) as f32;

  px(group_widths + group_gaps + divider_padding)
}

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

fn command_chip_width(command: &RibbonCommand, metrics: RibbonLayoutMetrics, window: &mut Window, cx: &mut Context<EditorRibbon>) -> gpui::Pixels {
  let label_width = measure_ribbon_text(command.label, metrics.chip_text_size, window, cx).as_f32();
  let shortcut_width = command
    .shortcut
    .as_ref()
    .map(|shortcut| measure_ribbon_text(shortcut, px(10.0), window, cx).as_f32() + 16.0)
    .unwrap_or(0.0);
  let accent_width = if command.accent.is_some() { 14.0 } else { 0.0 };
  let component_padding_x = px(4.0);
  let caret_width = if matches!(command.id, RibbonCommandId::HighlightMenu) { 10.0 } else { 0.0 };
  let chrome_width = metrics.chip_padding_x.as_f32() * 2.0 + component_padding_x.as_f32() * 2.0 + 10.0 + caret_width;

  px(label_width + shortcut_width + accent_width + chrome_width)
}

fn measure_ribbon_text(text: &str, font_size: gpui::Pixels, window: &mut Window, _cx: &mut App) -> gpui::Pixels {
  if text.is_empty() {
    return px(0.0);
  }
  let text_style = window.text_style();
  let runs = vec![text_style.to_run(text.len())];
  window.text_system().layout_line(text, font_size, &runs, None).width
}

fn modern_command_chip(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  options: ModernRibbonOptions,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let command_id = command.id;
  let tooltip = command_tooltip(command);
  let shortcut = command.shortcut.clone();

  Button::new(("modern-ribbon-command", ribbon_command_key(command_id)))
    .xsmall()
    .compact()
    .outline()
    .h(metrics.chip_height)
    .max_w(metrics.chip_max_width)
    .px(metrics.chip_padding_x)
    .rounded(px(6.0))
    .selected(command.selected)
    .disabled(command.disabled)
    .tooltip(tooltip)
    .when(command.selected, |this| {
      this
        .border_color(cx.theme().blue)
        .bg(cx.theme().blue_light.opacity(0.22))
        .text_color(cx.theme().foreground)
    })
    .when_some(command.accent, |this, accent| this.child(accent_dot(accent_color(accent, cx))))
    .child(
      div()
        .flex_none()
        .text_size(metrics.chip_text_size)
        .line_height(relative(1.0))
        .whitespace_nowrap()
        .text_ellipsis()
        .child(command.label),
    )
    .when(show_shortcut(options), |this| {
      this.when_some(shortcut, |this, shortcut| this.child(keycap(shortcut, cx)))
    })
    .on_click(move |_, _, cx| {
      editor.update(cx, |editor, cx| {
        perform_ribbon_command(editor, command_id, cx);
      });
    })
    .into_any_element()
}

fn modern_highlight_menu(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  document_theme: &DocumentTheme,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let accent = command.accent.unwrap_or(RibbonAccent::Gray);
  let selected_highlight = if let RibbonCommandId::ToggleHighlightMode(style) = command.id {
    Some(style)
  } else {
    None
  };
  let mode_active = command.selected;
  let document_theme = document_theme.clone();

  let chip_height = metrics.chip_height;
  let caret_width = px(f32::from(chip_height).round());

  div()
    .flex()
    .flex_row()
    .items_center()
    .gap_0()
    .child(
      div()
        .relative()
        .h(chip_height)
        .child(
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
                .selected(mode_active)
                .tooltip_with_action("Highlight mode", &ApplyHighlightToSelection, Some("RichTextEditor"))
                .child(match accent {
                  RibbonAccent::Transparent => transparent_accent_bar(cx),
                  _ => accent_bar(accent_color(accent, cx)),
                })
                .child(Icon::default().path("icons/highlighter.svg").xsmall())
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
                      editor.select_highlight_style(style, cx);
                    });
                  }),
                )
              });

              let editor = editor.clone();

              menu.item(
                PopupMenuItem::new("Clear highlight")
                  .on_click(move |_, _, cx| {
                    editor.update(cx, |editor, cx| {
                      editor.clear_armed_inline_tool(cx);
                      editor.set_highlight_for_selection(None, cx);
                    });
                  }),
              )
            }),
        )
    )
    .into_any_element()
}

fn modern_command_groups(
  state: &RichTextEditorStyleState,
  armed_tool: Option<ArmedInlineTool>,
  document_theme: &DocumentTheme,
  current_highlight: HighlightStyle,
  highlight_mode_active: bool,
) -> Vec<RibbonCommandGroup> {
  vec![
    RibbonCommandGroup {
      id: "styles",
      label: "Styles",
      commands: PARAGRAPH_STYLE_SPECS
        .iter()
        .map(|spec| {
          let command_id = paragraph_command_id(spec.style);
          RibbonCommand {
            id: RibbonCommandId::Paragraph(spec.style),
            label: spec.label,
            group_id: "styles",
            shortcut: command_id.and_then(shortcut_for),
            command_id,
            priority: paragraph_priority(spec.style),
            accent: None,
            selected: EditorRibbon::paragraph_selected(state, spec.style),
            disabled: false,
            overflow_behavior: paragraph_overflow_behavior(spec.style),
          }
        })
        .collect(),
    },
    RibbonCommandGroup {
      id: "inline",
      label: "Inline",
      commands: inline_commands(state, armed_tool),
    },
    RibbonCommandGroup {
      id: "highlight",
      label: "Highlight",
      commands: highlight_commands(document_theme, current_highlight, highlight_mode_active),
    },
    RibbonCommandGroup {
      id: "reset",
      label: "Reset",
      commands: vec![RibbonCommand {
        id: RibbonCommandId::ClearFormatting,
        label: "Clear Formatting",
        group_id: "reset",
        shortcut: shortcut_for(CommandId::ClearFormatting),
        command_id: Some(CommandId::ClearFormatting),
        priority: 90,
        accent: None,
        selected: false,
        disabled: false,
        overflow_behavior: OverflowBehavior::KeepVisible,
      }],
    },
  ]
}

fn inline_commands(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> Vec<RibbonCommand> {
  let mut commands = SEMANTIC_STYLE_SPECS
    .iter()
    .map(|spec| {
      let command_id = semantic_command_id(spec.style);
      RibbonCommand {
        id: RibbonCommandId::Semantic(spec.style),
        label: spec.label,
        group_id: "inline",
        shortcut: command_id.and_then(shortcut_for),
        command_id,
        priority: semantic_priority(spec.style),
        accent: None,
        selected: EditorRibbon::semantic_selected(state, armed_tool, spec.style),
        disabled: false,
        overflow_behavior: semantic_overflow_behavior(spec.style),
      }
    })
    .collect::<Vec<_>>();

  commands.push(RibbonCommand {
    id: RibbonCommandId::Underline,
    label: "Underline",
    group_id: "inline",
    shortcut: shortcut_for(CommandId::ToggleUnderline),
    command_id: Some(CommandId::ToggleUnderline),
    priority: 82,
    accent: None,
    selected: EditorRibbon::underline_selected(state, armed_tool),
    disabled: false,
    overflow_behavior: OverflowBehavior::KeepVisible,
  });
  commands.push(RibbonCommand {
    id: RibbonCommandId::Strikethrough,
    label: "Strike",
    group_id: "inline",
    shortcut: shortcut_for(CommandId::ToggleStrikethrough),
    command_id: Some(CommandId::ToggleStrikethrough),
    priority: 81,
    accent: None,
    selected: EditorRibbon::strikethrough_selected(state, armed_tool),
    disabled: false,
    overflow_behavior: OverflowBehavior::KeepVisible,
  });

  commands
}

fn highlight_commands(document_theme: &DocumentTheme, current_highlight: HighlightStyle, highlight_mode_active: bool) -> Vec<RibbonCommand> {
  vec![RibbonCommand {
    id: RibbonCommandId::ToggleHighlightMode(current_highlight),
    label: "",
    group_id: "highlight",
    shortcut: None,
    command_id: None,
    priority: 74,
    accent: Some(RibbonAccent::Color(highlight_color(current_highlight, document_theme))),
    selected: highlight_mode_active,
    disabled: false,
    overflow_behavior: OverflowBehavior::KeepVisible,
  }]
}

fn perform_ribbon_command(editor: &mut RichTextEditor, command_id: RibbonCommandId, cx: &mut Context<RichTextEditor>) {
  match command_id {
    RibbonCommandId::Paragraph(style) => {
      editor.set_paragraph_style_for_selection(style, cx);
    },
    RibbonCommandId::Semantic(style) => {
      editor.toggle_inline_tool(ArmedInlineTool::Semantic(style), cx);
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

fn paragraph_overflow_behavior(style: ParagraphStyle) -> OverflowBehavior {
  match style {
    ParagraphStyle::Normal | ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block => OverflowBehavior::KeepVisible,
    ParagraphStyle::Tag | ParagraphStyle::Analytic | ParagraphStyle::Undertag => OverflowBehavior::MoveToOverflow,
  }
}

fn semantic_overflow_behavior(style: RunSemanticStyle) -> OverflowBehavior {
  match style {
    RunSemanticStyle::Cite | RunSemanticStyle::Emphasis | RunSemanticStyle::Underline => OverflowBehavior::KeepVisible,
    RunSemanticStyle::Condensed | RunSemanticStyle::Ultracondensed => OverflowBehavior::MoveToOverflow,
    RunSemanticStyle::Plain => OverflowBehavior::HideInCompact,
  }
}

fn highlight_color(style: HighlightStyle, theme: &DocumentTheme) -> Hsla {
  match style {
    HighlightStyle::Spoken => theme.highlight_spoken,
    HighlightStyle::Insert => theme.highlight_insert,
    HighlightStyle::Alternative => theme.highlight_alternative,
  }
}

fn shortcut_for(command_id: CommandId) -> Option<String> {
  default_keys_for(command_id).first().map(|key| {
    Keystroke::parse(key)
      .map(|stroke| Kbd::format(&stroke))
      .unwrap_or_else(|_| (*key).to_string())
  })
}

fn show_shortcut(options: ModernRibbonOptions) -> bool {
  match options.shortcut_visibility {
    ShortcutVisibility::Always => true,
    ShortcutVisibility::HideInCompact => options.density == RibbonDensity::Full,
    ShortcutVisibility::HoverOnly | ShortcutVisibility::Hidden => false,
  }
}

fn command_tooltip(command: &RibbonCommand) -> String {
  match &command.shortcut {
    Some(shortcut) => format!("{} ({})", command.label, shortcut),
    None => command.label.to_string(),
  }
}

fn keycap(shortcut: String, cx: &mut Context<EditorRibbon>) -> AnyElement {
  div()
    .flex_none()
    .whitespace_nowrap()
    .rounded(px(4.0))
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

fn accent_dot(color: Hsla) -> AnyElement {
  div()
    .flex_none()
    .size(px(6.0))
    .rounded(px(3.0))
    .bg(color)
    .into_any_element()
}

fn accent_bar(color: Hsla) -> AnyElement {
  div()
    .flex_none()
    .w(px(3.0))
    .h(px(12.0))
    .rounded(px(2.0))
    .bg(color)
    .into_any_element()
}

fn transparent_accent_bar(cx: &mut Context<EditorRibbon>) -> AnyElement {
  div()
    .flex_none()
    .w(px(3.0))
    .h(px(12.0))
    .rounded(px(2.0))
    .border_1()
    .border_color(cx.theme().border.opacity(0.62))
    .bg(cx.theme().background.opacity(0.0))
    .into_any_element()
}

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

fn ribbon_command_key(command_id: RibbonCommandId) -> u64 {
  match command_id {
    RibbonCommandId::Paragraph(style) => 1_000 + style as u64,
    RibbonCommandId::Semantic(style) => 2_000 + style as u64,
    RibbonCommandId::Underline => 3_000,
    RibbonCommandId::Strikethrough => 3_100,
    RibbonCommandId::Highlight(style) => 4_000 + style as u64,
    RibbonCommandId::ClearHighlight => 5_000,
    RibbonCommandId::HighlightMenu => 5_002,
    RibbonCommandId::ToggleHighlightMode(style) => 5_100 + style as u64,
    RibbonCommandId::ClearFormatting => 5_001,
  }
}
