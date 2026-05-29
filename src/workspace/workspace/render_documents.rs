impl Workspace {
  fn render_document_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
    let active_index = self.active_document_index(cx).unwrap_or(0);
    v_flex()
      .flex_1()
      .w_full()
      .h_full()
      .overflow_hidden()
      .bg(cx.theme().background)
      .when(!(self.document_panels.is_empty() && self.flow_panels.is_empty()), |this| {
        this.child(self.render_document_tab_bar(active_index, cx))
      })
      .child(
        div()
          .flex_1()
          .w_full()
          .h_full()
          .overflow_hidden()
          .when_some(self.active_editor.clone(), |this, editor| this.child(editor))
          .when_some(self.active_flow.clone(), |this, editor| this.child(editor))
          .when(self.active_editor.is_none() && self.active_flow.is_none(), |this| this.child(self.render_empty_state(cx))),
      )
  }

  fn render_document_tab_bar(&self, active_index: usize, cx: &mut Context<Self>) -> impl IntoElement {
    let tabs = self.document_tabs(cx);
    let (active_tab_bg, active_tab_fg) = if let Some(editor) = &self.active_editor {
      let theme = &editor.read(cx).document().theme;
      (theme.document_background_color, theme.default_text_color)
    } else {
      (cx.theme().background, cx.theme().foreground)
    };
    TabBar::new("document-tab-bar")
      .xsmall()
      .track_scroll(&self.tab_bar_scroll_handle)
      .menu(true)
      .prefix(self.render_document_tab_bar_prefix(active_index, tabs.len(), cx))
      .suffix(self.render_document_tab_bar_suffix())
      .active_tab_bg(active_tab_bg)
      .active_tab_fg(active_tab_fg)
      .selected_index(active_index)
      .on_click({
        let tabs = tabs.clone();
        cx.listener(move |workspace, ix: &usize, _, cx| {
          if let Some(tab) = tabs.get(*ix) {
            workspace.activate_document_id(tab.id, cx);
          }
        })
      })
      .children(tabs.into_iter().map(|tab| {
        let panel_id = tab.id;
        let close_button = icon_button(("close-tab", panel_id.as_u128() as u64), AppIcon::Close)
          .tooltip("Close document")
          .when(tab.active, |this| {
            this.custom(
              ButtonCustomVariant::new(cx)
                .foreground(active_tab_fg)
                .hover(active_tab_fg.opacity(0.12))
                .active(active_tab_fg.opacity(0.18)),
            )
          })
          .on_click(cx.listener(move |workspace, _, window, cx| {
            cx.stop_propagation();
            workspace.close_document_panel(panel_id, window, cx);
          }));
        Tab::new()
          // GPUI-component tabs size to their labels. Keep tab labels bounded
          // before rendering so long filenames cannot break the tab strip.
          .label(tab.label)
          .selected(tab.active)
          .suffix(close_button)
      }))
      .last_empty_space(div().flex_1().h_full())
  }

}
