#[hotpath::measure_all]
impl Render for Workspace {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    // Window-level mouse listeners may only be registered during the paint
    // phase (gpui debug-asserts this; render runs during layout), so the
    // ctrl+scroll zoom hook rides a zero-cost canvas child's paint pass.
    let workspace = cx.entity().downgrade();
    let zoom_wheel_hook = gpui::canvas(
      |_, _, _| (),
      move |_, _, window, _| {
        window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, _, window, cx| {
          if event.modifiers.control {
            let delta = event.delta.pixel_delta(window.line_height());
            if let Some(workspace) = workspace.upgrade() {
              workspace.update(cx, |workspace, cx| {
                if let Some(editor) = workspace.active_editor.clone() {
                  if delta.y < px(0.0) {
                    editor.update(cx, |editor, cx| editor.zoom_in(cx));
                  } else {
                    editor.update(cx, |editor, cx| editor.zoom_out(cx));
                  }
                } else if let Some(flow) = workspace.active_flow.clone() {
                  if delta.y < px(0.0) {
                    flow.update(cx, |flow, cx| flow.zoom_in(cx));
                  } else {
                    flow.update(cx, |flow, cx| flow.zoom_out(cx));
                  }
                }
              });
            }
            cx.stop_propagation();
          }
        });
      },
    );

    div()
      .size_full()
      .relative()
      .child(zoom_wheel_hook)
      .child(
        v_flex()
          .on_action(cx.listener(Self::on_save))
          .on_action(cx.listener(Self::on_find_in_document))
          .on_action(cx.listener(Self::on_fidelity_mark))
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
              .child(self.render_resizable_workspace(window, cx)),
          )
          .child(self.render_status_bar(window, cx)),
      )
      .when_some(self.settings_overlay, |this, overlay| {
        this.child(self.render_settings_overlay(overlay, cx))
      })
      .when_some(self.file_search_overlay.clone(), |this, overlay| this.child(overlay))
      .when_some(self.command_palette.clone(), |this, palette| this.child(palette))
      .when_some(self.equation_composer.clone(), |this, composer| this.child(composer))
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
      .when_some(Root::render_dialog_layer(window, cx), |this, layer| this.child(layer))
      .when_some(Root::render_notification_layer(window, cx), |this, layer| this.child(layer))
  }
}
