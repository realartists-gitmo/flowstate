use gpui::{
  Context, Entity, IntoElement, ParentElement as _, Render, Window, div, prelude::*,
};
use gpui_component::button::{
  Button, ButtonGroup, ButtonVariants as _, Toggle, ToggleVariants as _,
};
use gpui_component::{ActiveTheme as _, Selectable as _, Sizable as _};

use crate::rich_text_element::{
  ArmedInlineTool, HighlightStyle, ParagraphStyle, RichTextEditor, RichTextEditorStyleState,
  RunSemanticStyle, SelectionState,
};
use crate::ribbon::style_catalog::{
  HIGHLIGHT_STYLE_SPECS, PARAGRAPH_STYLE_SPECS, SEMANTIC_STYLE_SPECS,
};

/// Word-like formatting ribbon for a rich text editor.
///
/// The ribbon reads the editor's current `style_state` each render and sends
/// formatting requests back through `RichTextEditor`'s public API.
pub struct EditorRibbon {
  editor: Entity<RichTextEditor>,
}

impl EditorRibbon {
  pub fn new(editor: Entity<RichTextEditor>) -> Self {
    Self { editor }
  }

  fn paragraph_selected(state: &RichTextEditorStyleState, style: ParagraphStyle) -> bool {
    matches!(state.paragraph_style, SelectionState::Uniform(current) if current == style)
  }

  fn semantic_selected(
    state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    style: RunSemanticStyle,
  ) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Semantic(current)) if current == style)
      || matches!(state.semantic, SelectionState::Uniform(current) if current == style)
  }

  fn underline_selected(
    state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
  ) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Underline))
      || matches!(state.underline, SelectionState::Uniform(true))
  }

  fn highlight_selected(
    state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    style: HighlightStyle,
  ) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Highlight(current)) if current == style)
      || matches!(state.highlight, SelectionState::Uniform(Some(current)) if current == style)
  }
}

impl Render for EditorRibbon {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let (style_state, armed_tool) = {
      let editor = self.editor.read(cx);
      (editor.style_state(), editor.armed_inline_tool())
    };

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
      .child(ribbon_group(
        "Styles",
        ButtonGroup::new("paragraph-styles")
          .compact()
          .outline()
          .children(PARAGRAPH_STYLE_SPECS.iter().map(|spec| {
            let editor = self.editor.clone();
            let style = spec.style;
            Button::new(("paragraph-style", style as u64))
              .label(spec.label)
              .selected(Self::paragraph_selected(&style_state, style))
              .tooltip(spec.label)
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.set_paragraph_style_for_selection(style, cx);
                });
              })
          })),
        cx,
      ))
      .child(ribbon_group(
        "Inline",
        div()
          .flex()
          .flex_row()
          .items_center()
          .gap_1()
          .children(SEMANTIC_STYLE_SPECS.iter().map(|spec| {
            let editor = self.editor.clone();
            let style = spec.style;
            Toggle::new(("semantic-style", style as u64))
              .label(spec.label)
              .small()
              .outline()
              .checked(Self::semantic_selected(&style_state, armed_tool, style))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.activate_inline_tool(ArmedInlineTool::Semantic(style), cx);
                });
              })
          }))
          .child({
            let editor = self.editor.clone();
            Toggle::new("underline-style")
              .label("Underline")
              .small()
              .outline()
              .checked(Self::underline_selected(&style_state, armed_tool))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.activate_inline_tool(ArmedInlineTool::Underline, cx);
                });
              })
          }),
        cx,
      ))
      .child(ribbon_group(
        "Highlight",
        div()
          .flex()
          .flex_row()
          .items_center()
          .gap_1()
          .children(HIGHLIGHT_STYLE_SPECS.iter().map(|spec| {
            let editor = self.editor.clone();
            let highlight = spec.style;
            Toggle::new(("highlight-style", highlight as u64))
              .label(spec.label)
              .small()
              .outline()
              .checked(Self::highlight_selected(&style_state, armed_tool, highlight))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.activate_inline_tool(ArmedInlineTool::Highlight(highlight), cx);
                });
              })
          }))
          .child({
            let editor = self.editor.clone();
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
      .child(ribbon_group(
        "Reset",
        {
          let editor = self.editor.clone();
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
  }
}

fn ribbon_group(label: &'static str, controls: impl IntoElement, cx: &mut Context<EditorRibbon>) -> impl IntoElement {
  div()
    .flex()
    .flex_col()
    .gap_1()
    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label))
    .child(controls)
}
