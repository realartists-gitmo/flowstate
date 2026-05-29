impl Workspace {
  fn render_top_bar(&mut self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    TitleBar::new()
      .on_close_window(move |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| workspace.request_close_window(window, cx));
      })
      .child(
        h_flex()
          .h_full()
          .flex_1()
          .items_center()
          .gap_1()
          .child(flowstate_top_bar_button(cx))
          .child(file_top_bar_button(self.active_document_id.is_some(), cx))
          .child(insert_top_bar_button(cx, self.active_editor.is_some()))
          .child(document_top_bar_button(cx))
          .child(view_top_bar_button(cx, !self.outline_collapsed, !self.ribbon_collapsed, !self.toolkit_collapsed))
          .child(settings_top_bar_button(cx))
      )
  }

  fn render_ribbon(&self, ribbon_height: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
    let active_ribbon = self.active_document_id.and_then(|active_id| {
      self
        .document_panels
        .iter()
        .find(|panel| panel.read(cx).id() == active_id)
        .map(|panel| panel.read(cx).ribbon().into_any_element())
        .or_else(|| {
          self
            .flow_panels
            .iter()
            .find(|panel| panel.read(cx).id() == active_id)
            .map(|panel| panel.read(cx).ribbon().into_any_element())
        })
    });
    let show_placeholder = active_ribbon.is_none();

    h_flex()
      .relative()
      .h(ribbon_height)
      .min_h(px(56.0))
      .w_full()
      .items_start()
      .bg(cx.theme().background)
      .when_some(active_ribbon, |this, ribbon| {
        if let Some(active_id) = self.active_document_id {
          if let Some(panel) = self.document_panels.iter().find(|panel| panel.read(cx).id() == active_id) {
            panel.read(cx).ribbon().update(cx, |ribbon, cx| {
              ribbon.set_height(ribbon_height, cx);
            });
          } else if let Some(panel) = self.flow_panels.iter().find(|panel| panel.read(cx).id() == active_id) {
            panel.read(cx).ribbon().update(cx, |ribbon, cx| {
              ribbon.set_height(ribbon_height, cx);
            });
          }
        }
        this.child(ribbon)
      })
      .when(show_placeholder, |this| {
        this.px_2().child(
          div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Ribbon placeholder"),
        )
      })
      .child(
        div()
          .absolute()
          .bottom_0()
          .right_0()
          .p_1()
          .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
          .child(
            Button::new("collapse-ribbon-panel")
              .icon(Icon::default().path("icons/panel-top-close.svg"))
              .xsmall()
              .ghost()
              .tooltip("Collapse ribbon")
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.toggle_ribbon(cx);
              })),
          ),
      )
  }

}
