use gpui::{
  Context, Entity, IntoElement, ParentElement as _, Render, Window, div, prelude::*, rgb,
};
use gpui_component::button::{
  Button, ButtonGroup, ButtonVariants as _, Toggle, ToggleVariants as _,
};
use gpui_component::{Selectable as _, Sizable as _};

use crate::rich_text_element::{
  HighlightStyle, ParagraphStyle, RichTextEditor, RichTextEditorStyleState, RunSemanticStyle,
  SelectionState,
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

  fn semantic_selected(state: &RichTextEditorStyleState, style: RunSemanticStyle) -> bool {
    matches!(state.semantic, SelectionState::Uniform(current) if current == style)
  }

  fn underline_selected(state: &RichTextEditorStyleState) -> bool {
    matches!(state.underline, SelectionState::Uniform(true))
  }

  fn highlight_selected(state: &RichTextEditorStyleState, style: HighlightStyle) -> bool {
    matches!(state.highlight, SelectionState::Uniform(Some(current)) if current == style)
  }
}

impl Render for EditorRibbon {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let style_state = self.editor.read(cx).style_state();

    div()
      .w_full()
      .flex()
      .flex_row()
      .items_center()
      .gap_3()
      .px_3()
      .py_2()
      .border_b_1()
      .border_color(rgb(0xd8dce2))
      .bg(rgb(0xf6f7f9))
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
              .checked(Self::semantic_selected(&style_state, style))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.toggle_semantic_style_for_selection(style, cx);
                });
              })
          }))
          .child({
            let editor = self.editor.clone();
            Toggle::new("underline-style")
              .label("Underline")
              .small()
              .outline()
              .checked(Self::underline_selected(&style_state))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.toggle_underline(cx);
                });
              })
          }),
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
              .checked(Self::highlight_selected(&style_state, highlight))
              .on_click(move |_, _, cx| {
                editor.update(cx, |editor, cx| {
                  editor.set_highlight_for_selection(Some(highlight), cx);
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
                  editor.set_highlight_for_selection(None, cx);
                });
              })
          }),
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
      ))
  }
}

fn ribbon_group(label: &'static str, controls: impl IntoElement) -> impl IntoElement {
  div()
    .flex()
    .flex_col()
    .gap_1()
    .child(div().text_xs().text_color(rgb(0x5c6675)).child(label))
    .child(controls)
}
