#[hotpath::measure_all]
impl Workspace {
  fn render_top_bar(&mut self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    let active_collaborating = self
      .active_document_id
      .and_then(|panel_id| crate::collab::phase_for_panel(panel_id, cx))
      .is_some_and(|phase| !matches!(phase, crate::collab::SessionPhase::Detached(_)));
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
          .child(collaboration_top_bar_button(cx, self.active_document_id.is_some(), active_collaborating))
          .child(view_top_bar_button(
            cx,
            !self.outline_collapsed,
            !self.ribbon_collapsed,
            !self.toolkit_collapsed,
          ))
          .child(div().flex_1())
          .child(share_top_bar_button(cx, self.active_editor.is_some(), active_collaborating))
          .child(settings_top_bar_button(cx)),
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
          if let Some(panel) = self
            .document_panels
            .iter()
            .find(|panel| panel.read(cx).id() == active_id)
          {
            let panel_id = panel.read(cx).id();
            let speech_active = self.speech_document_id == Some(panel_id);
            let speech_send_enabled = self.speech_document_id.is_some() && !speech_active;
            let workspace = cx.entity().downgrade();
            panel.read(cx).ribbon().update(cx, |ribbon, cx| {
              ribbon.set_height(ribbon_height, cx);
              ribbon.set_workspace_context(workspace, panel_id, speech_active, speech_send_enabled, cx);
            });
          } else if let Some(panel) = self
            .flow_panels
            .iter()
            .find(|panel| panel.read(cx).id() == active_id)
          {
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
  }
}
