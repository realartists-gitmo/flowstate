#[hotpath::measure_all]
impl Render for Workspace {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size_full()
      .relative()
      .child(
        v_flex()
          .on_action(cx.listener(Self::on_save))
          .size_full()
          .bg(cx.theme().background)
          .child(self.render_top_bar(window, cx))
          .when(self.styles_settings_open, |this| this.child(self.render_styles_settings_view(cx)))
          .when(!self.styles_settings_open, |this| {
            this
              .child(self.render_resizable_workspace(cx))
              .child(self.render_status_bar(cx))
          }),
      )
      .when_some(self.file_search_overlay.clone(), |this, overlay| this.child(overlay))
  }
}

