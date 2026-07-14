#[hotpath::measure_all]
impl Workspace {
  fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.zoom_in(cx));
    } else if let Some(flow) = self.active_flow.clone() {
      flow.update(cx, |flow, cx| flow.zoom_in(cx));
    }
  }

  fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.zoom_out(cx));
    } else if let Some(flow) = self.active_flow.clone() {
      flow.update(cx, |flow, cx| flow.zoom_out(cx));
    }
  }

  fn sync_zoom_slider(&mut self, percent: f32, window: &mut Window, cx: &mut Context<Self>) {
    let current = match self.zoom_slider.read(cx).value() {
      SliderValue::Single(value) => value,
      SliderValue::Range(_, value) => value,
    };
    if (current - percent).abs() >= 0.5 {
      self.zoom_slider.update(cx, |slider, cx| {
        slider.set_value(percent, window, cx);
      });
    }
  }

  fn render_zoom_slider(&self, percent: f32, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .items_center()
      .justify_center()
      .gap_2()
      .w(px(260.0))
      .child(
        div()
          .w(px(42.0))
          .text_xs()
          .text_align(gpui::TextAlign::Right)
          .text_color(cx.theme().muted_foreground)
          .child(format!("{percent:.0}%")),
      )
      .child(
        div()
          .w(px(180.0))
          .child(Slider::new(&self.zoom_slider).horizontal()),
      )
  }
}
