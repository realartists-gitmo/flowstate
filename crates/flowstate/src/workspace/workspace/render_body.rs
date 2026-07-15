#[hotpath::measure_all]
impl Workspace {
  fn render_resizable_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    if self.document_panels.is_empty() && self.flow_panels.is_empty() {
      return div()
        .flex_1()
        .overflow_hidden()
        .child(self.render_document_pane(cx))
        .into_any_element();
    }

    if self.ribbon_collapsed {
      return v_flex()
        .flex_1()
        .overflow_hidden()
        .child(self.render_collapsed_ribbon_bar(cx))
        .child(self.render_workspace_body(window, cx))
        .into_any_element();
    }

    let ribbon_height = self.committed_ribbon_height;
    let workspace = cx.entity().downgrade();

    v_resizable("workspace-ribbon-resizable")
      .with_state(&self.ribbon_resizable_state)
      .on_resize(move |state, _, cx| {
        let Some(height) = state.read(cx).sizes().first().copied() else {
          return;
        };
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.committed_ribbon_height = height;
          cx.notify();
        });
      })
      .child(
        resizable_panel()
          .size(px(112.0))
          .size_range(px(56.0)..px(158.0))
          .grow(false)
          .child(self.render_ribbon(ribbon_height, cx)),
      )
      .child(
        resizable_panel()
          .size(px(640.0))
          .size_range(px(320.0)..Pixels::MAX)
          .child(self.render_workspace_body(window, cx)),
      )
      .into_any_element()
  }

  fn render_workspace_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let panel_sizes = self.body_resizable_state.read(cx).sizes().clone();
    let outline_width = if self.outline_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH
    } else {
      px(240.0)
    };
    let nav_width = panel_sizes
      .first()
      .copied()
      .or(self.restored_nav_width)
      .unwrap_or(outline_width)
      .max(outline_width);
    let outline_range_end = if self.outline_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH
    } else {
      px(420.0)
    };

    h_resizable("workspace-body-resizable")
      .with_state(&self.body_resizable_state)
      .child(
        resizable_panel()
          .size(if self.outline_collapsed { outline_width } else { nav_width })
          .size_range(outline_width..outline_range_end)
          .grow(false)
          .child(if self.outline_collapsed {
            // The collapsed rail names what it actually reopens (Law 1).
            let reopen_label = if self.left_nav_mode == LeftNavMode::Tub {
              "Show tub"
            } else if self.active_flow.is_some() {
              "Show flow outline"
            } else {
              "Show outline"
            };
            self
              .render_collapsed_side_panel(reopen_label, IconName::PanelLeftOpen, |workspace, cx| workspace.toggle_outline(cx), cx)
              .into_any_element()
          } else {
            self.render_left_nav(nav_width, window, cx).into_any_element()
          }),
      )
      .child(
        resizable_panel()
          .size(px(860.0))
          .size_range(px(160.0)..Pixels::MAX)
          .child(div().size_full().min_w_0().overflow_hidden().child(self.render_content_area(window, cx))),
      )
  }

  fn render_collapsed_ribbon_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .h(px(30.0))
      .flex_none()
      .w_full()
      .items_center()
      .justify_end()
      .px_2()
      .border_b(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .bg(cx.theme().background)
      .child(
        Button::new("restore-ribbon-panel")
          .icon(Icon::default().path("icons/panel-top-open.svg").text_color(cx.theme().muted_foreground))
          .xsmall()
          .ghost()
          .tooltip("Show ribbon")
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.toggle_ribbon(cx);
          })),
      )
  }

  fn render_collapsed_side_panel(
    &self,
    tooltip: &'static str,
    icon: IconName,
    toggle: fn(&mut Workspace, &mut Context<Workspace>),
    cx: &mut Context<Self>,
  ) -> impl IntoElement {
    v_flex()
      .size_full()
      .items_center()
      .pt_2()
      .bg(cx.theme().background)
      .child(
        Button::new(tooltip)
          .icon(Icon::new(icon).text_color(cx.theme().sidebar_foreground))
          .xsmall()
          .ghost()
          .tooltip(tooltip)
          .on_click(cx.listener(move |workspace, _, _, cx| {
            toggle(workspace, cx);
          })),
      )
  }

}
