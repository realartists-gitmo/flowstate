#[hotpath::measure_all]
impl Workspace {
  fn render_top_bar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .h(px(36.0))
      .flex_none()
      .w_full()
      .items_center()
      .pl_2()
      .border_b(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().title_bar_border)
      .bg(cx.theme().title_bar)
      // With a transparent system titlebar, this GPUI-drawn bar becomes the
      // visual titlebar. Let empty space in it drag the native window.
      .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
      .child(
        h_flex()
          .h_full()
          .items_center()
          .gap_1()
          .child(file_top_bar_button(self.active_document_id.is_some(), cx))
          .child(insert_top_bar_button(cx, self.active_editor.is_some()))
          .child(styles_top_bar_button(cx))
          .child(theme_top_bar_button(cx))
          .child(view_top_bar_button(cx, !self.outline_collapsed, !self.ribbon_collapsed, !self.toolkit_collapsed))
          .child(top_bar_button("top-settings", "Settings"))
      )
      .child(div().flex_1())
      .child(self.render_window_controls(window, cx))
  }

  fn render_window_controls(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .h_full()
      .flex_none()
      .child(window_control_button(
        "window-minimize",
        IconName::WindowMinimize,
        WindowControlArea::Min,
        cx.listener(|_, _, window, cx| {
          cx.stop_propagation();
          window.minimize_window();
        }),
        false,
        cx,
      ))
      .child(window_control_button(
        "window-maximize",
        if window.is_maximized() {
          IconName::WindowRestore
        } else {
          IconName::WindowMaximize
        },
        WindowControlArea::Max,
        cx.listener(|_, _, window, cx| {
          cx.stop_propagation();
          window.zoom_window();
        }),
        false,
        cx,
      ))
      .child(window_control_button(
        "window-close",
        IconName::WindowClose,
        WindowControlArea::Close,
        cx.listener(|workspace, _, window, cx| {
          cx.stop_propagation();
          workspace.request_close_window(window, cx);
        }),
        true,
        cx,
      ))
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
