#[hotpath::measure_all]
impl Render for EditorRibbon {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let (style_state, armed_tool, document_theme, current_highlight, highlight_mode_active, invisibility_mode) = {
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

    match self.mode {
      RibbonMode::Legacy => LegacyStylesRibbon::render(self.editor.clone(), &style_state, armed_tool, cx),
      RibbonMode::Modern => ModernStylesRibbon::render(
        self.editor.clone(),
        &style_state,
        armed_tool,
        &document_theme,
        current_highlight,
        highlight_mode_active,
        invisibility_mode,
        self.modern_options,
        self.height,
        window.viewport_size().width,
        window,
        cx,
      ),
    }
  }
}
