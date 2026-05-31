pub struct LegacyStylesRibbon;

#[hotpath::measure_all]
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
                Button::new(("paragraph-style", style.slot()))
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
            Toggle::new(("semantic-style", style.slot()))
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
            Toggle::new(("highlight-style", highlight.slot()))
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

#[hotpath::measure]
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

