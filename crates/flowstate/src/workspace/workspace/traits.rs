#[hotpath::measure_all]
impl Render for Workspace {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size_full()
      .relative()
      .child(
        v_flex()
          .on_action(cx.listener(Self::on_save))
          .on_action(cx.listener(Self::on_find_in_document))
          .on_action(cx.listener(Self::on_zoom_in))
          .on_action(cx.listener(Self::on_zoom_out))
          .size_full()
          .bg(cx.theme().background)
          .child(self.render_top_bar(window, cx))
          .child(
            v_flex()
              .flex_1()
              .min_h_0()
              .overflow_hidden()
              .child(self.render_resizable_workspace(cx)),
          )
          .child(self.render_status_bar(window, cx)),
      )
      .when_some(self.settings_overlay, |this, overlay| {
        this.child(self.render_settings_overlay(overlay, cx))
      })
      .when_some(self.file_search_overlay.clone(), |this, overlay| this.child(overlay))
      .when_some(self.outline_context_menu.as_ref(), |this, ctx| {
        let workspace = cx.entity().downgrade();
        let menu = ctx.menu_view.clone();
        let position = ctx.position;
        this.child(
          deferred(
            anchored().child(
              div()
                .w(window.bounds().size.width)
                .h(window.bounds().size.height)
                .occlude()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                  let _ = workspace.update(cx, |workspace, cx| {
                    workspace.outline_context_menu = None;
                    cx.notify();
                  });
                })
                .child(
                  anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .anchor(Corner::TopLeft)
                    .child(menu),
                ),
            ),
          )
          .with_priority(1),
        )
      })
  }
}
