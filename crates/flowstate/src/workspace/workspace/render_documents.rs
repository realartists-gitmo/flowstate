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
          // H-S3: history mode commandeers the viewport for its panel.
          .when_some(
            self
              .history_takeover
              .clone()
              .filter(|takeover| Some(takeover.read(cx).panel_id) == self.active_document_id),
            |this, takeover| this.child(div().flex_1().overflow_hidden().child(takeover)),
          )
          .when_some(
            self.active_editor.clone().filter(|_| {
              self
                .history_takeover
                .as_ref()
                .is_none_or(|takeover| Some(takeover.read(cx).panel_id) != self.active_document_id)
            }),
            |this, editor| this.child(div().flex_1().overflow_hidden().child(editor)),
          )
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
    let workspace = cx.entity().downgrade();
    TabBar::new("document-tab-bar")
      .small()
      .track_scroll(&self.tab_bar_scroll_handle)
      .menu(true)
      .prefix(self.render_document_tab_bar_prefix(active_index, tabs.len(), cx))
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
        let workspace = workspace.clone();
        let collab_phase = crate::collab::phase_for_panel(panel_id, cx);
        // TB-S4 cleaned badges: pin chips are kept; every other mark (dirty,
        // speech, collab) shows at most TWO, the rest fold into a tooltip.
        let mut marks: Vec<(&'static str, gpui::AnyElement)> = Vec::new();
        if tab.dirty {
          marks.push((
            "Unsaved changes",
            div()
              .w(px(6.0))
              .h(px(6.0))
              .rounded_full()
              .bg(cx.theme().warning)
              .into_any_element(),
          ));
        }
        if tab.speech {
          marks.push((
            "Speech document",
            div()
              .text_xs()
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .text_color(cx.theme().success)
              .child("S")
              .into_any_element(),
          ));
        }
        if let Some(badge) = collab_phase.as_ref().and_then(|phase| crate::collab::status::tab_badge(phase, cx)) {
          marks.push(("Collaboration", badge.into_any_element()));
        }
        let overflow: Vec<&'static str> = marks.iter().skip(2).map(|(name, _)| *name).collect();
        let overflow_tooltip: SharedString = overflow.join(" · ").into();
        let overflow_count = overflow.len();
        let visible_marks: Vec<gpui::AnyElement> = marks.into_iter().take(2).map(|(_, mark)| mark).collect();
        let tab_prefix = h_flex()
          .ml(px(5.0))
          .mr(px(-3.0))
          .gap(px(2.0))
          .items_center()
          .children(visible_marks)
          .when(overflow_count > 0, |this| {
            this.child(
              div()
                .id(("tab-mark-overflow", panel_id.as_u128() as u64))
                .text_size(px(9.0))
                .text_color(cx.theme().muted_foreground)
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(overflow_tooltip.clone()).build(window, cx))
                .child(format!("+{overflow_count}")),
            )
          })
          .when_some(tab.pin_index.and_then(pin_shortcut_label), |this, pin_label| {
            let shortcut_hint: SharedString =
              format!("Pinned — Alt+{pin_label} switches here (with pins, Alt+N counts pinned tabs first)").into();
            this.child(
              div()
                .id(("tab-pin-badge", panel_id.as_u128() as u64))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(shortcut_hint.clone()).build(window, cx))
                .w(px(14.0))
                .h(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(cx.theme().warning)
                .border_1()
                .border_color(cx.theme().warning.opacity(0.72))
                .child(pin_label),
            )
          })
          ;
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
          .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |workspace, _, window, cx| {
              cx.stop_propagation();
              workspace.close_document_panel(panel_id, window, cx);
            }),
          )
          .when(tab.speech, |this| this.bg(cx.theme().success.opacity(0.14)))
          .prefix(tab_prefix)
          .suffix(close_button)
          .context_menu(move |menu, _, _| {
            let pin_workspace = workspace.clone();
            let left_workspace = workspace.clone();
            let right_workspace = workspace.clone();
            let tear_workspace = workspace.clone();
            menu
              .item(PopupMenuItem::new(if tab.pinned { "Unpin tab" } else { "Pin tab" }).on_click(move |_, _, cx| {
                let _ = pin_workspace.update(cx, |workspace, cx| workspace.toggle_tab_pin(panel_id, cx));
              }))
              // TB-S3: reorder within the tab's zone (pins stay a zone).
              .item(PopupMenuItem::new("Move tab left").on_click(move |_, _, cx| {
                let _ = left_workspace.update(cx, |workspace, cx| workspace.move_document_tab(panel_id, -1, cx));
              }))
              .item(PopupMenuItem::new("Move tab right").on_click(move |_, _, cx| {
                let _ = right_workspace.update(cx, |workspace, cx| workspace.move_document_tab(panel_id, 1, cx));
              }))
              // TB-S3 tear-off rides the New Window machinery.
              .item(PopupMenuItem::new("Move to new window").on_click(move |_, window, cx| {
                let _ = tear_workspace.update(cx, |workspace, cx| workspace.tear_off_document_tab(panel_id, window, cx));
              }))
          })
      }))
      .last_empty_space(div().flex_1().h_full())
  }
}
