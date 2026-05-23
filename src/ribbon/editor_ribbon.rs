use gpui::{
  AnyElement, App, Context, Entity, Hsla, IntoElement, Keystroke, ParentElement as _, Render, Styled as _, Window, div, prelude::*, px, relative,
};
use gpui_component::Size;
use gpui_component::button::DropdownButton;
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _, Toggle, ToggleVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, IconName, PixelsExt as _, Selectable as _, Sizable as _, StyledExt as _};
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
  read_mode: RibbonReadMode,
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
      read_mode: RibbonReadMode::Base,
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
    let (style_state, armed_tool, document_theme, current_highlight, highlight_mode_active, read_enabled) = {
      let editor = self.editor.read(cx);
      (
        editor.style_state(),
        editor.armed_inline_tool(),
        editor.document_theme(),
        editor.current_highlight_choice(),
        editor.highlight_mode_active(),
        editor.invisibility_mode(),
      )
    };
    if read_enabled {
      self.read_mode = RibbonReadMode::Read;
    } else if self.read_mode == RibbonReadMode::Read {
      self.read_mode = RibbonReadMode::Base;
    }

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
        self.read_mode,
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
          .children(
            PARAGRAPH_STYLE_SPECS
              .iter()
              .filter(|spec| spec.style != ParagraphStyle::Normal)
              .map(|spec| {
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
              }),
          ),
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
              .label("Strikethrough")
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
            .label("Clear")
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RibbonReadMode {
  Base,
  Read,
  Send,
}

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

impl RibbonLabel {
  fn for_command(command: &RibbonCommand) -> Self {
    let icon_path = match command.id {
      RibbonCommandId::Semantic(RunSemanticStyle::Emphasis) => Some("icons/bold.svg"),
      RibbonCommandId::Underline => Some("icons/underline.svg"),
      RibbonCommandId::Strikethrough => Some("icons/strikethrough.svg"),
      RibbonCommandId::CondensedMenu => Some("icons/shrink.svg"),
      RibbonCommandId::ToggleHighlightMode(_) => Some("icons/highlighter.svg"),
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

impl ModernStylesRibbon {
  fn render(
    editor: Entity<RichTextEditor>,
    style_state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    document_theme: &DocumentTheme,
    current_highlight: Option<HighlightStyle>,
    highlight_mode_active: bool,
    options: ModernRibbonOptions,
    height: gpui::Pixels,
    read_mode: RibbonReadMode,
    _available_width: gpui::Pixels,
    window: &mut Window,
    cx: &mut Context<EditorRibbon>,
  ) -> AnyElement {
    let groups = modern_command_groups(style_state, armed_tool, document_theme, current_highlight, highlight_mode_active);
    let mut metrics = RibbonLayoutMetrics::from_height(height);
    let rows_allowed_by_height = metrics.max_chip_rows;
    // Use vertical ribbon room proactively. Width pressure can force wrapping,
    // but when there is spare height we still prefer balanced columns over one
    // long horizontal strip.
    metrics.max_chip_rows = rows_allowed_by_height;
    let wrap_widths = groups
      .iter()
      .map(|group| {
        (metrics.max_chip_rows > 1).then(|| {
          if group.id == "keyed" {
            balanced_group_width(group, metrics, metrics.max_chip_rows, window, cx)
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
              .items_start()
              .gap(metrics.group_gap)
              .min_w_0()
              .children(
                groups.iter().enumerate().map(|(index, group)| {
                  modern_group(index > 0, group, editor.clone(), document_theme, options, metrics, wrap_widths[index], cx)
                }),
              ),
          )
          .child(read_mode_grid(editor.clone(), read_mode, metrics, cx)),
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
    let group_label_height = px(12.0);
    let group_body_gap = px(3.0);
    let group_bottom_guard = px(5.0);
    let max_chip_rows = chip_rows_for_height(
      height,
      chip_height,
      chip_gap,
      group_padding_top,
      group_label_height,
      group_body_gap,
      group_bottom_guard,
    );
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

fn group_outer_width(content_width: gpui::Pixels, has_divider: bool, metrics: RibbonLayoutMetrics) -> gpui::Pixels {
  let divider_chrome = if has_divider {
    metrics.group_divider_padding_left.as_f32() + 1.0
  } else {
    0.0
  };
  px(content_width.as_f32() + divider_chrome)
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
            .when_some(wrap_width, |this, wrap_width| this.w(wrap_width))
            .children(group.commands.iter().map(|command| {
              if matches!(command.id, RibbonCommandId::ToggleHighlightMode(_)) {
                modern_highlight_menu(command, editor.clone(), document_theme, metrics, cx)
              } else if matches!(command.id, RibbonCommandId::CondensedMenu) {
                modern_condensed_menu(command, editor.clone(), metrics, cx)
              } else {
                modern_command_chip(command, editor.clone(), options, metrics, cx)
              }
            })),
        ),
    )
    .into_any_element()
}

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

fn command_columns(commands: &[RibbonCommand], max_rows: usize) -> Vec<Vec<&RibbonCommand>> {
  let rows = max_rows.max(1);
  commands
    .chunks(rows)
    .map(|chunk| chunk.iter().collect())
    .collect()
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

fn command_chip_width(
  command: &RibbonCommand,
  metrics: RibbonLayoutMetrics,
  window: &mut Window,
  cx: &mut Context<EditorRibbon>,
) -> gpui::Pixels {
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
}

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
  let label = RibbonLabel::for_command(command);
  let icon_path = label.prefers_icon().then_some(label.icon_path).flatten();

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
    .when_some(icon_path, |this, path| this.child(Icon::default().path(path).xsmall()))
    .when(icon_path.is_none(), |this| {
      this.child(
        div()
          .flex_none()
          .text_size(metrics.chip_text_size)
          .line_height(relative(1.0))
          .whitespace_nowrap()
          .text_ellipsis()
          .child(label.text),
      )
    })
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
  let selected_highlight = match command.id {
    RibbonCommandId::ToggleHighlightMode(style) => style,
    _ => None,
  };
  let mode_active = command.selected;
  let document_theme = document_theme.clone();

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
                  .bg(cx.theme().secondary_active)
                  .border_color(cx.theme().border)
                  .text_color(cx.theme().foreground)
              })
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
            .bg(cx.theme().secondary_active)
            .border_color(cx.theme().border)
            .text_color(cx.theme().foreground)
        })
        .tooltip("Condensed")
        .when_some(label.icon_path, |this, path| this.child(Icon::default().path(path).xsmall()))
        .when(!label.prefers_icon(), |this| {
          this.child(
            div()
              .flex_none()
              .text_size(metrics.chip_text_size)
              .line_height(relative(1.0))
              .whitespace_nowrap()
              .text_ellipsis()
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

fn read_mode_grid(
  editor: Entity<RichTextEditor>,
  read_mode: RibbonReadMode,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let stack_rows = read_mode_rows_fit(metrics);
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
        .text_color(cx.theme().muted_foreground)
        .child("Views"),
    )
    .child(
      div()
        .flex()
        .when(stack_rows, |this| this.flex_col())
        .when(!stack_rows, |this| this.flex_row().flex_wrap())
        .gap(metrics.chip_gap)
        .child(read_mode_row("Base", RibbonReadMode::Base, read_mode, editor.clone(), metrics, cx))
        .child(read_mode_row("Read", RibbonReadMode::Read, read_mode, editor.clone(), metrics, cx))
        .child(read_mode_row("Send", RibbonReadMode::Send, read_mode, editor, metrics, cx)),
    )
    .into_any_element()
}

fn read_mode_rows_fit(metrics: RibbonLayoutMetrics) -> bool {
  let label_height = 12.0;
  let label_gap = 2.0;
  let bottom_guard = 5.0;
  let fixed_height = metrics.group_padding_top.as_f32() + label_height + label_gap + bottom_guard;
  let rows_height = metrics.chip_height.as_f32() * 3.0 + metrics.chip_gap.as_f32() * 2.0;
  metrics.height.as_f32() - fixed_height >= rows_height
}

fn read_mode_row(
  label: &'static str,
  mode: RibbonReadMode,
  current: RibbonReadMode,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let selected = mode == current;
  let eye_editor = editor.clone();
  // Keep these controls aligned with the rest of the ribbon: all ribbon
  // buttons should use the same xsmall + compact scaling unless the whole
  // ribbon sizing model changes.
  div()
    .flex()
    .flex_row()
    .items_center()
    .gap(metrics.chip_gap)
    .child(
      Button::new(("read-mode-label", read_mode_key(mode)))
        .xsmall()
        .compact()
        .outline()
        .h(metrics.chip_height)
        .w(px(52.0))
        .px(metrics.chip_padding_x)
        .label(label),
    )
    .child(
      Button::new(("read-mode-eye", read_mode_key(mode)))
        .xsmall()
        .compact()
        .outline()
        .h(metrics.chip_height)
        .w(metrics.chip_height)
        .icon(if selected { IconName::Eye } else { IconName::EyeOff })
        .selected(selected)
        .on_click(cx.listener(move |ribbon, _, _, cx| {
          ribbon.read_mode = mode;
          eye_editor.update(cx, |editor, cx| {
            editor.set_invisibility_mode(mode == RibbonReadMode::Read, cx);
          });
          cx.notify();
        })),
    )
    .child(save_dropdown(mode, editor, metrics, cx))
    .into_any_element()
}

fn save_dropdown(
  mode: RibbonReadMode,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  _cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let enabled = mode == RibbonReadMode::Base;
  let chip_height = metrics.chip_height;
  DropdownButton::new(("read-mode-save-dropdown", read_mode_key(mode)))
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .button(
      Button::new(("read-mode-save", read_mode_key(mode)))
        .xsmall()
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        .tooltip("Save")
        .child(Icon::default().path("icons/save.svg").xsmall())
        .on_click({
          let editor = editor.clone();
          move |_, _, cx| {
            if enabled {
              editor.update(cx, |editor, cx| {
                let _ = editor.save(cx);
              });
            }
          }
        }),
    )
    .dropdown_menu(move |menu, _, _| {
      let db8_editor = editor.clone();
      menu
        .min_w(px(150.0))
        .item(PopupMenuItem::new("Save as DB8").on_click(move |_, _, cx| {
          if enabled {
            db8_editor.update(cx, |editor, cx| {
              let _ = editor.save(cx);
            });
          }
        }))
        .item(PopupMenuItem::new("Save as DOCX"))
        .item(PopupMenuItem::new("Save as PDF"))
    })
    .into_any_element()
}

fn read_mode_key(mode: RibbonReadMode) -> u64 {
  match mode {
    RibbonReadMode::Base => 1,
    RibbonReadMode::Read => 2,
    RibbonReadMode::Send => 3,
  }
}

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

fn keyed_inline_commands(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> Vec<RibbonCommand> {
  let mut commands = SEMANTIC_STYLE_SPECS
    .iter()
    .filter(|spec| matches!(spec.style, RunSemanticStyle::Cite | RunSemanticStyle::Emphasis))
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

fn unkeyed_inline_commands(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> Vec<RibbonCommand> {
  vec![
    RibbonCommand {
      id: RibbonCommandId::CondensedMenu,
      label: "Condensed",
      group_id: "unkeyed",
      shortcut: None,
      command_id: None,
      priority: 76,
      accent: None,
      selected: matches!(
        armed_tool,
        Some(ArmedInlineTool::Semantic(RunSemanticStyle::Condensed | RunSemanticStyle::Ultracondensed))
      ) || matches!(
        state.semantic,
        SelectionState::Uniform(RunSemanticStyle::Condensed | RunSemanticStyle::Ultracondensed)
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
    RibbonCommandId::CondensedMenu => 2_900,
    RibbonCommandId::Underline => 3_000,
    RibbonCommandId::Strikethrough => 3_100,
    RibbonCommandId::Highlight(style) => 4_000 + style as u64,
    RibbonCommandId::ClearHighlight => 5_000,
    RibbonCommandId::HighlightMenu => 5_002,
    RibbonCommandId::ToggleHighlightMode(style) => {
      5_100
        + match style {
          Some(style) => style as u64,
          None => 999,
        }
    },
    RibbonCommandId::ClearFormatting => 5_001,
  }
}
