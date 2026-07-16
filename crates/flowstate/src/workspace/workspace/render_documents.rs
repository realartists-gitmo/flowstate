#[hotpath::measure_all]
impl Workspace {
  /// W-S4 P1: the viewing surface is the pane TREE — recursive splits whose
  /// leaves each render their own tab strip + body. One pane = exactly the
  /// historical window body.
  fn render_document_pane(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
    let root = self.pane_tree.root().clone();
    div()
      .flex_1()
      .w_full()
      .min_w_0()
      .h_full()
      .overflow_hidden()
      .bg(cx.theme().background)
      .child(self.render_pane_node(&root, cx))
  }

  fn render_pane_node(&mut self, node: &PaneNode, cx: &mut Context<Self>) -> gpui::AnyElement {
    match node {
      PaneNode::Pane(leaf) => self.render_pane_leaf(leaf, cx),
      PaneNode::Split { axis, ratio, children } => {
        let first = self.render_pane_node(&children[0], cx);
        let second = self.render_pane_node(&children[1], cx);
        let ratio = ratio.clamp(0.15, 0.85);
        match axis {
          SplitAxis::Horizontal => h_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
              div()
                .w(gpui::relative(ratio))
                .h_full()
                .min_w_0()
                .overflow_hidden()
                .child(first),
            )
            .child(div().w(px(1.0)).h_full().flex_none().bg(cx.theme().border))
            .child(div().flex_1().h_full().min_w_0().overflow_hidden().child(second))
            .into_any_element(),
          SplitAxis::Vertical => v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
              div()
                .h(gpui::relative(ratio))
                .w_full()
                .min_h_0()
                .overflow_hidden()
                .child(first),
            )
            .child(div().h(px(1.0)).w_full().flex_none().bg(cx.theme().border))
            .child(div().flex_1().w_full().min_h_0().overflow_hidden().child(second))
            .into_any_element(),
        }
      },
    }
  }

  fn render_pane_leaf(&mut self, leaf: &PaneLeaf, cx: &mut Context<Self>) -> gpui::AnyElement {
    let focused = leaf.id == self.pane_tree.focused;
    let pane_id = leaf.id;
    let body: gpui::AnyElement = match leaf.active {
      Some(active) if focused && self.active_document_id == Some(active) => self.render_focused_pane_body(cx),
      Some(active) => {
        // An unfocused pane paints its live editor entity read-hot; the
        // first click anywhere inside focuses the pane (capture phase, so
        // the click still lands where it was aimed).
        let editor: Option<gpui::AnyElement> = self
          .document_panels
          .iter()
          .find(|panel| panel.read(cx).id() == active)
          .map(|panel| panel.read(cx).editor().into_any_element())
          .or_else(|| {
            self
              .flow_panels
              .iter()
              .find(|panel| panel.read(cx).id() == active)
              .map(|panel| panel.read(cx).editor().into_any_element())
          });
        match editor {
          Some(editor) => div().size_full().overflow_hidden().child(editor).into_any_element(),
          None => self.render_empty_state(cx).into_any_element(),
        }
      },
      // Q3-A: an empty pane hosts the home surface.
      None => self.render_empty_state(cx).into_any_element(),
    };
    v_flex()
      .size_full()
      .min_w_0()
      .min_h_0()
      .overflow_hidden()
      .capture_any_mouse_down(cx.listener(move |workspace, _, _, cx| {
        workspace.focus_pane(pane_id, cx);
      }))
      .when(!leaf.tab_order.is_empty(), |this| {
        this.child(self.render_pane_tab_bar(leaf, focused, cx))
      })
      .child(div().flex_1().w_full().min_h_0().overflow_hidden().child(body))
      .into_any_element()
  }

  /// The FOCUSED pane's body — the historical window body: session strip,
  /// search bar, alt-text bar, history takeover, the active editor/flow.
  fn render_focused_pane_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
      .size_full()
      .min_w_0()
      .min_h_0()
      .overflow_hidden()
      // CO-S4: the live session strip — attached-only chrome (the status-bar
      // pill defers to this band while it shows).
      .when_some(self.render_session_strip(cx), |this, strip| this.child(strip))
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
          // B-S10: the inline alt-text editor floats over the pane.
          .when_some(self.alt_text_editor.clone(), |this, input| {
            this.child(
              h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(div().text_xs().text_color(cx.theme().muted_foreground).flex_none().child("Alt text"))
                .child(div().flex_1().child(Input::new(&input)))
                .child(
                  Button::new("alt-text-save")
                    .xsmall()
                    .primary()
                    .label("Save")
                    .on_click(cx.listener(move |workspace, _, _, cx| {
                      let value = workspace
                        .alt_text_editor
                        .as_ref()
                        .map(|input| input.read(cx).value().to_string());
                      if let (Some(value), Some(editor)) = (value, workspace.active_editor.clone()) {
                        editor.update(cx, |editor, cx| editor.set_selected_image_alt_text(value, cx));
                      }
                      workspace.close_alt_text_editor(cx);
                    })),
                )
                .child(
                  Button::new("alt-text-cancel")
                    .xsmall()
                    .ghost()
                    .label("Cancel")
                    .on_click(cx.listener(|workspace, _, _, cx| workspace.close_alt_text_editor(cx))),
                ),
            )
          })
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
      .into_any_element()
  }

  /// CO-S4: peer chips + invite/leave for the active document's ATTACHED
  /// session. Absent entirely for solo/joining/detached panels.
  fn render_session_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
    let panel_id = self.active_document_id?;
    let phase = crate::collab::phase_for_panel(panel_id, cx)?;
    let crate::collab::SessionPhase::Attached(attachment) = &phase else {
      return None;
    };
    let roster = crate::collab::roster_for_panel(panel_id, cx);
    let connectivity_label = crate::collab::status::phase_label(&phase, cx);
    Some(
      h_flex()
        .h(px(26.0))
        .flex_none()
        .w_full()
        .items_center()
        .gap_2()
        .px_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().link.opacity(0.06))
        .child(
          div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Live · {connectivity_label} — everyone edits")),
        )
        .children(roster.into_iter().enumerate().map(|(ix, entry)| {
          h_flex()
            .gap_1()
            .items_center()
            .px_1()
            .rounded_sm()
            .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(gpui::Hsla::from(gpui::rgb(entry.color_rgb))))
            .child(
              div()
                .text_xs()
                .when(entry.is_self, |this| this.text_color(cx.theme().muted_foreground))
                .child(if entry.is_self { format!("{} (you)", entry.name) } else { entry.name }),
            )
            .id(("session-strip-peer", ix))
            .into_any_element()
        }))
        .when(attachment.peers_present == 0, |this| {
          this.child(div().text_xs().text_color(cx.theme().muted_foreground).child("No one else here yet"))
        })
        .child(div().flex_1())
        .child(
          Button::new("session-strip-invite")
            .xsmall()
            .ghost()
            .label("Invite")
            .on_click(cx.listener(|workspace, _, window, cx| {
              workspace.open_collaboration_dialog(window, cx);
            })),
        )
        .child(
          Button::new("session-strip-leave")
            .xsmall()
            .ghost()
            .label("Leave")
            .tooltip("Leave the session — the document stays yours, local")
            .on_click(cx.listener(move |_, _, _, cx| {
              crate::collab::leave_session_for_panel(panel_id, cx);
              cx.notify();
            })),
        )
        .into_any_element(),
    )
  }

  /// W-S4 P1: one tab strip PER PANE — its tabs only, its own scroll
  /// handle; the "+"/overflow prefix rides the focused strip.
  fn render_pane_tab_bar(&mut self, leaf: &PaneLeaf, focused: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
    let tabs: Vec<DocumentTab> = self
      .document_tabs(cx)
      .into_iter()
      .filter(|tab| leaf.tab_order.contains(&tab.id))
      .map(|mut tab| {
        tab.active = leaf.active == Some(tab.id);
        tab
      })
      .collect();
    let active_index = tabs.iter().position(|tab| tab.active).unwrap_or(0);
    let scroll = self
      .pane_tab_scrolls
      .entry(leaf.id.0)
      .or_default()
      .clone();
    self
      .render_document_tab_bar(tabs, active_index, leaf.id, focused, scroll, cx)
      .into_any_element()
  }

  fn render_document_tab_bar(
    &self,
    tabs: Vec<DocumentTab>,
    active_index: usize,
    pane: PaneId,
    focused: bool,
    scroll: ScrollHandle,
    cx: &mut Context<Self>,
  ) -> impl IntoElement {
    let active_is_speech = tabs.get(active_index).is_some_and(|tab| tab.speech);
    let active_tab_bg = if active_is_speech {
      cx.theme().success.opacity(0.18)
    } else {
      cx.theme().background
    };
    let active_tab_fg = cx.theme().foreground;
    let workspace = cx.entity().downgrade();
    TabBar::new(("document-tab-bar", pane.0))
      .small()
      .track_scroll(&scroll)
      .menu(true)
      .when(focused, |this| {
        this.prefix(self.render_document_tab_bar_prefix(active_index, tabs.len(), cx))
      })
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
          // CT-S2: the send's success feedback — a transient sent-count pulse
          // beside the badge (no activity-log line, per Adam's CT2 amendment).
          let recent = self.speech_sent_recent;
          marks.push((
            "Speech document",
            h_flex()
              .items_center()
              .gap_0p5()
              .child(
                div()
                  .text_xs()
                  .font_weight(gpui::FontWeight::SEMIBOLD)
                  .text_color(cx.theme().success)
                  .child("S"),
              )
              .when(recent > 0, |this| {
                this.child(
                  div()
                    .text_size(px(9.0))
                    .px_1()
                    .rounded_full()
                    .bg(cx.theme().success.opacity(0.18))
                    .text_color(cx.theme().success)
                    .child(format!("+{recent}")),
                )
              })
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
          .context_menu(move |menu, window, cx| {
            let _ = window;
            let tear_windows = crate::workspace::live_workspace_windows(cx);
            let tear_windows_self = workspace.clone();
            let split_right_workspace = workspace.clone();
            let split_down_workspace = workspace.clone();
            let pin_workspace = workspace.clone();
            let left_workspace = workspace.clone();
            let right_workspace = workspace.clone();
            let tear_workspace = workspace.clone();
            let speech_workspace = workspace.clone();
            menu
              .item(PopupMenuItem::new(if tab.pinned { "Unpin tab" } else { "Pin tab" }).on_click(move |_, _, cx| {
                let _ = pin_workspace.update(cx, |workspace, cx| workspace.toggle_tab_pin(panel_id, cx));
              }))
              // CT-S1: speech designation is reachable where the tab lives —
              // it was ribbon/palette-only. Flows can't be the speech doc.
              .item(
                PopupMenuItem::new(if tab.speech {
                  "Unmark speech document"
                } else if tab.flow {
                  "Mark as speech document (flows can't)"
                } else {
                  "Mark as speech document"
                })
                .disabled(tab.flow)
                .on_click(move |_, _, cx| {
                  let _ = speech_workspace.update(cx, |workspace, cx| workspace.toggle_speech_document(panel_id, cx));
                }),
              )
              // TB-S3: reorder within the tab's zone (pins stay a zone).
              .item(PopupMenuItem::new("Move tab left").on_click(move |_, _, cx| {
                let _ = left_workspace.update(cx, |workspace, cx| workspace.move_document_tab(panel_id, -1, cx));
              }))
              .item(PopupMenuItem::new("Move tab right").on_click(move |_, _, cx| {
                let _ = right_workspace.update(cx, |workspace, cx| workspace.move_document_tab(panel_id, 1, cx));
              }))
              // W-S4: split verbs live beside the move verbs — the tab
              // activates first, then rides the split into the new pane.
              .item(PopupMenuItem::new("Split right with this tab").on_click({
                let workspace = split_right_workspace.clone();
                move |_, _, cx| {
                  let _ = workspace.update(cx, |workspace, cx| {
                    workspace.activate_document_id(panel_id, cx);
                    workspace.split_focused_pane(SplitAxis::Horizontal, cx);
                  });
                }
              }))
              .item(PopupMenuItem::new("Split down with this tab").on_click({
                let workspace = split_down_workspace.clone();
                move |_, _, cx| {
                  let _ = workspace.update(cx, |workspace, cx| {
                    workspace.activate_document_id(panel_id, cx);
                    workspace.split_focused_pane(SplitAxis::Vertical, cx);
                  });
                }
              }))
              // W-S3: rich-text tabs move LIVE (entity handoff — pathless
              // moves too). Flows still reopen by path, so unsaved flows
              // keep the save-first guard.
              .item(
                PopupMenuItem::new(if tab.pathless && tab.flow {
                  "Move to new window (save first)"
                } else {
                  "Move to new window"
                })
                .disabled(tab.pathless && tab.flow)
                .on_click(move |_, window, cx| {
                  let _ = tear_workspace.update(cx, |workspace, cx| workspace.tear_off_document_tab(panel_id, window, cx));
                }),
              )
              // W-S3 re-dock: send the live tab to an existing window.
              .when(!tab.flow, |menu| {
                let mut menu = menu;
                let own = tear_windows_self.clone();
                for (window_ix, (_, target)) in tear_windows.iter().enumerate() {
                  if Some(target) == own.upgrade().as_ref() {
                    continue;
                  }
                  let target = target.downgrade();
                  let mover = tear_windows_self.clone();
                  menu = menu.item(
                    PopupMenuItem::new(format!("Move to window {}", window_ix + 1)).on_click(move |_, _, cx| {
                      let _ = mover.update(cx, |workspace, cx| {
                        workspace.move_document_tab_to_window(panel_id, target.clone(), cx);
                      });
                    }),
                  );
                }
                menu
              })
          })
      }))
      .last_empty_space(div().flex_1().h_full())
  }
}
