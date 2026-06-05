#[hotpath::measure_all]
impl Workspace {
  fn render_document_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
    let active_index = self.active_document_index(cx).unwrap_or(0);
    let active_search_bar = self.active_document_id.and_then(|active_document_id| {
      self
        .document_panels
        .iter()
        .find(|panel| panel.read(cx).id() == active_document_id)
        .and_then(|panel| {
          let panel = panel.read(cx);
          panel.search_bar_open().then(|| panel.search_bar())
        })
    });

    v_flex()
      .flex_1()
      .w_full()
      .min_w_0()
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
          .min_w_0()
          .h_full()
          .overflow_hidden()
          .flex()
          .flex_col()
          .when_some(active_search_bar, |this, search_bar| this.child(search_bar))
          .when_some(self.active_editor.clone(), |this, editor| {
            this.child(div().flex_1().overflow_hidden().child(editor))
          })
          .when_some(self.active_flow.clone(), |this, editor| {
            this.child(div().flex_1().overflow_hidden().child(editor))
          })
          .when(self.active_editor.is_none() && self.active_flow.is_none(), |this| {
            this.child(self.render_empty_state(cx))
          }),
      )
  }

  fn render_document_tab_bar(&self, active_index: usize, cx: &mut Context<Self>) -> impl IntoElement {
    let tabs = self.document_tabs(cx);
    let active_is_speech = tabs.get(active_index).is_some_and(|tab| tab.speech);
    let active_tab_bg = if active_is_speech {
      cx.theme().success.opacity(0.18)
    } else {
      cx.theme().background
    };
    let active_tab_fg = cx.theme().foreground;
    TabBar::new("document-tab-bar")
      .small()
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
        let speech_button = Button::new(("speech-tab", panel_id.as_u128() as u64))
          .label("S")
          .xsmall()
          .ghost()
          .tooltip(if tab.speech { "Unset speech document" } else { "Set speech document" })
          .text_color(if tab.speech { cx.theme().success } else { cx.theme().muted_foreground })
          .on_click(cx.listener(move |workspace, _, _, cx| {
            cx.stop_propagation();
            workspace.toggle_speech_document(panel_id, cx);
          }));
        let pin_button = Button::new(("pin-tab", panel_id.as_u128() as u64))
          .icon(
            Icon::new(if tab.pinned { IconName::Star } else { IconName::StarOff })
              .xsmall()
              .text_color(if tab.pinned { cx.theme().warning } else { cx.theme().muted_foreground }),
          )
          .xsmall()
          .ghost()
          .tooltip(if tab.pinned { "Unpin tab" } else { "Pin tab" })
          .on_click(cx.listener(move |workspace, _, _, cx| {
            cx.stop_propagation();
            workspace.toggle_tab_pin(panel_id, cx);
          }));
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
          .prefix(h_flex().gap_0p5().child(speech_button).child(pin_button))
          .suffix(close_button)
      }))
      .last_empty_space(div().flex_1().h_full())
  }
}
