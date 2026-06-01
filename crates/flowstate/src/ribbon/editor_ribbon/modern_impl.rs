#[hotpath::measure_all]
impl ModernStylesRibbon {
  fn render(
    editor: Entity<RichTextEditor>,
    style_state: &RichTextEditorStyleState,
    armed_tool: Option<ArmedInlineTool>,
    document_theme: &DocumentTheme,
    current_highlight: Option<HighlightStyle>,
    highlight_mode_active: bool,
    invisibility_mode: bool,
    options: ModernRibbonOptions,
    height: gpui::Pixels,
    _available_width: gpui::Pixels,
    window: &mut Window,
    cx: &mut Context<EditorRibbon>,
  ) -> AnyElement {
    let groups = modern_command_groups(style_state, armed_tool, document_theme, current_highlight, highlight_mode_active);
    let metrics = RibbonLayoutMetrics::from_height(height);
    // Use vertical ribbon room proactively. Width pressure can force wrapping,
    // but when there is spare height we still prefer balanced columns over one
    // long horizontal strip.
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
          .child(export_section(editor.clone(), metrics, cx))
          .child(invisibility_mode_button(editor.clone(), invisibility_mode, metrics, cx)),
      )
      .into_any_element()
  }
}
