use gpui_component::avatar::Avatar;


struct CollaborationPeerStatusItem {
  session_id: SessionId,
  label: String,
  role: Role,
  cursor: Option<String>,
  focus: Option<String>,
  viewport_hint: Option<String>,
  has_presence: bool,
}

#[hotpath::measure_all]
impl Workspace {
  fn next_untitled_title(&self, cx: &App) -> String {
    let used = self
      .document_panels
      .iter()
      .filter_map(|panel| untitled_index(panel.read(cx).title_text().as_ref()))
      .collect::<HashSet<_>>();
    let mut index = 1usize;
    while used.contains(&index) {
      index += 1;
    }
    format!("Untitled{index}.db8")
  }

  fn next_untitled_flow_title(&self, cx: &App) -> String {
    let used = self
      .flow_panels
      .iter()
      .filter_map(|panel| untitled_flow_index(panel.read(cx).title_text().as_ref()))
      .collect::<HashSet<_>>();
    let mut index = 1usize;
    while used.contains(&index) {
      index += 1;
    }
    format!("Untitled{index}.fl0")
  }

  fn render_document_tab_bar_prefix(&self, active_index: usize, tab_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    h_flex()
      .h_full()
      .items_center()
      .gap_1()
      .px_1()
      .child(
        icon_button("tab-bar-new-file", AppIcon::NewFile)
          .tooltip("New")
          .dropdown_menu(move |menu, _, _| {
            menu
              .item(file_menu_item(workspace.clone(), "Doc", false, |workspace, window, cx| {
                workspace.new_document(window, cx);
              }))
              .item(file_menu_item(workspace.clone(), "Flow", false, |workspace, window, cx| {
                workspace.new_flow(window, cx);
              }))
          }),
      )
      .child(
        icon_button("tab-bar-save-file", AppIcon::SaveFile)
          .tooltip("Save current file")
          .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.save_active(window, cx);
          })),
      )
      .child(
        icon_button("tab-bar-navigate-left", AppIcon::TabLeft)
          .tooltip("Navigate tab left")
          .disabled(active_index == 0)
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.navigate_active_tab(-1, cx);
          })),
      )
      .child(
        icon_button("tab-bar-navigate-right", AppIcon::TabRight)
          .tooltip("Navigate tab right")
          .disabled(active_index + 1 >= tab_count)
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.navigate_active_tab(1, cx);
          })),
      )
  }

  fn render_document_tab_bar_suffix(&self) -> impl IntoElement {
    h_flex().h_full().items_center().gap_1().px_1().child(
      icon_button("tab-bar-multipanel-placeholder", AppIcon::MultiPanel)
        .tooltip("Multi-panel layout")
        .disabled(true),
    )
  }

  fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
    // These buttons call command methods directly for now. When command
    // dispatch grows beyond direct callbacks, keep the buttons mapped to
    // `CommandId::NewDocument` and `CommandId::OpenDemoDocument`.
    let new_doc = cx.listener(|workspace, _, window, cx| workspace.new_document(window, cx));
    let new_flow = cx.listener(|workspace, _, window, cx| workspace.new_flow(window, cx));
    let open_document = cx.listener(|workspace, _, window, cx| workspace.prompt_open_document(window, cx));
    let open_search = cx.listener(|workspace, _, window, cx| workspace.open_file_search_overlay(window, cx));
    let recent_documents = self
      .recent_documents
      .iter()
      .take(3)
      .cloned()
      .collect::<Vec<_>>();
    v_flex()
      .size_full()
      .items_center()
      .justify_start()
      .gap_3()
      .pt(px(32.0))
      .bg(cx.theme().background)
      .child(
        div()
          .text_xl()
          .font_weight(gpui::FontWeight::SEMIBOLD)
          .text_color(cx.theme().foreground)
          .child("No document open"),
      )
      .child(
        h_flex()
          .gap_2()
          .child(
            Button::new("empty-new-document")
              .icon(Icon::new(IconName::Plus).text_color(cx.theme().primary_foreground))
              .label("New Doc")
              .primary()
              .on_click(new_doc),
          )
          .child(
            Button::new("empty-new-flow")
              .icon(Icon::new(IconName::Plus).text_color(cx.theme().primary_foreground))
              .label("New Flow")
              .primary()
              .on_click(new_flow),
          )
          .child(
            Button::new("empty-open-document")
              .icon(Icon::new(IconName::FolderOpen).text_color(cx.theme().secondary_foreground))
              .label("Open")
              .on_click(open_document),
          )
          .child(
            Button::new("empty-search-document")
              .icon(Icon::new(IconName::Search).text_color(cx.theme().secondary_foreground))
              .label("Search")
              .on_click(open_search),
          ),
      )
      .when(!recent_documents.is_empty(), |this| {
        this.child(
          h_flex()
            .w_full()
            .flex_1()
            .items_start()
            .gap_4()
            .px_8()
            .pb_8()
            .pt_4()
            .children(recent_documents.into_iter().enumerate().map(|(ix, path)| {
              let title = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
              let path_text = path.display().to_string();
              let preview_document = self.recent_document_previews.get(&path).cloned();
              let preview_unavailable = preview_document.is_none();
              let hover_group = format!("empty-recent-document-hover-{ix}");
              let preview_radius = cx.theme().radius.max(px(1.0)) - px(1.0);
              let open_recent = cx.listener({
                let path = path.clone();
                move |workspace, _, window, cx| workspace.open_document_path(path.clone(), window, cx)
              });
              v_flex()
                .id(("empty-recent-document", ix))
                .h_full()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                  div()
                    .group(hover_group.clone())
                    .w_full()
                    .flex_1()
                    .min_h(px(320.0))
                    .max_h(px(520.0))
                    .relative()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .overflow_hidden()
                    .when_some(preview_document, |this, document| {
                      this.child(
                        div()
                          .size_full()
                          .rounded(preview_radius)
                          .overflow_hidden()
                          .bg(document.theme.document_background_color)
                          .child(RichTextDocumentElement::new(document)),
                      )
                    })
                    .when(preview_unavailable, |this| {
                      this.child(
                        div()
                          .size_full()
                          .flex()
                          .items_center()
                          .justify_center()
                          .text_sm()
                          .text_color(cx.theme().secondary_foreground)
                          .child("Preview unavailable"),
                      )
                    })
                    .child(
                      div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .invisible()
                        .bg(cx.theme().muted.opacity(0.18))
                        .group_hover(hover_group.clone(), |this| this.visible()),
                    )
                    .child(
                      div()
                        .id(("empty-recent-document-overlay", ix))
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_click(open_recent),
                    ),
                )
                .child(
                  div()
                    .min_w_0()
                    .flex_none()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(title),
                )
                .child(
                  div()
                    .min_w_0()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().secondary_foreground)
                    .child(path_text),
                )
            })),
        )
      })
  }

  fn render_collaboration_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let role = self.collaboration.role.unwrap_or("No role");
    let queued = self.collaboration_pending_updates.len();
    let lag = self.collaboration_delta_updates_since_checkpoint;
    let last_hash = self
      .collaboration_last_published_hash
      .as_ref()
      .map(short_hash)
      .unwrap_or_else(|| "none".to_string());
    let last_rejected_update = self.collaboration.last_error.as_deref().unwrap_or("none");
    let mut peers = self
      .collaboration
      .peers
      .iter()
      .map(|(session_id, peer)| {
        CollaborationPeerStatusItem {
          session_id: *session_id,
          label: collaboration_peer_display_name(peer),
          role: peer.role,
          cursor: peer.cursor.clone(),
          focus: peer.focus.clone(),
          viewport_hint: peer.viewport_hint.clone(),
          has_presence: peer.last_seen_millis.is_some(),
        }
      })
      .collect::<Vec<_>>();
    peers.sort_by(|left, right| {
      left
        .label
        .cmp(&right.label)
        .then_with(|| left.session_id.0.cmp(&right.session_id.0))
    });
    let visible_peer_count = 4usize;
    let hidden_peer_count = peers.len().saturating_sub(visible_peer_count);

    h_flex()
      .min_w_0()
      .max_w(px(760.0))
      .items_center()
      .gap_1()
      .px_2()
      .text_xs()
      .text_color(cx.theme().muted_foreground)
      .child(collaboration_status_chip(format!("{:?}", self.collaboration.state), cx))
      .child(collaboration_status_chip(role.to_string(), cx))
      .child(collaboration_status_chip(format!("hash {last_hash}"), cx))
      .when(queued > 0, |this| {
        this.child(collaboration_status_chip(format!("{queued} queued"), cx))
      })
      .when(lag > 0, |this| {
        this.child(collaboration_status_chip(format!("lag {lag}"), cx))
      })
      .when(self.collaboration.last_error.is_some(), |this| {
        this.child(collaboration_status_chip(format!("reject {last_rejected_update}"), cx))
      })
      .children(
        peers
          .iter()
          .take(visible_peer_count)
          .map(|peer| collaboration_peer_status_chip(peer, cx)),
      )
      .when(hidden_peer_count > 0, |this| {
        this.child(collaboration_status_chip(format!("+{hidden_peer_count} peers"), cx))
      })
  }

  fn render_status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let zoom = self
      .active_editor
      .as_ref()
      .map(|editor| editor.read(cx).zoom_percent());
    if let Some(percent) = zoom {
      self.sync_zoom_slider(percent, window, cx);
    }
    h_flex()
      .h(px(26.0))
      .flex_none()
      .w_full()
      .items_center()
      .px_2()
      .border_t(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .bg(cx.theme().background)
      .child(div().flex_1())
      .child(self.render_collaboration_status(cx))
      .when_some(zoom, |this, percent| this.child(self.render_zoom_slider(percent, cx)))
      .child(div().flex_1())
  }
}

fn collaboration_status_chip(label: impl Into<SharedString>, cx: &mut Context<Workspace>) -> AnyElement {
  div()
    .h(px(20.0))
    .flex_none()
    .flex()
    .items_center()
    .rounded(px(4.0))
    .border_1()
    .border_color(cx.theme().border.opacity(0.72))
    .bg(cx.theme().secondary.opacity(0.36))
    .px_1()
    .text_xs()
    .text_color(cx.theme().muted_foreground)
    .child(label.into())
    .into_any_element()
}

fn collaboration_peer_status_chip(peer: &CollaborationPeerStatusItem, cx: &mut Context<Workspace>) -> AnyElement {
  let role_label = collaboration_sync_role_label(peer.role);
  let tooltip = collaboration_peer_status_tooltip(peer, role_label);
  let presence_color = if peer.has_presence {
    cx.theme().green
  } else {
    cx.theme().muted_foreground.opacity(0.48)
  };
  h_flex()
    .h(px(22.0))
    .max_w(px(176.0))
    .min_w_0()
    .items_center()
    .gap_1()
    .rounded(px(4.0))
    .border_1()
    .border_color(cx.theme().border.opacity(0.72))
    .bg(cx.theme().background)
    .px_1()
    .map(|mut this| {
      this
        .interactivity()
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx));
      this
    })
    .child(Avatar::new().name(peer.label.clone()).xsmall())
    .child(
      div()
        .size(px(6.0))
        .flex_none()
        .rounded_full()
        .bg(presence_color),
    )
    .child(
      div()
        .min_w_0()
        .max_w(px(92.0))
        .truncate()
        .text_color(cx.theme().foreground)
        .child(peer.label.clone()),
    )
    .child(
      div()
        .flex_none()
        .text_color(cx.theme().muted_foreground)
        .child(role_label),
    )
    .into_any_element()
}

fn collaboration_peer_status_tooltip(peer: &CollaborationPeerStatusItem, role_label: &str) -> String {
  let mut lines = vec![
    format!("{} ({role_label})", peer.label),
    format!("Session {}", collaboration_short_uuid(peer.session_id.0)),
    if peer.has_presence {
      "Presence active".to_string()
    } else {
      "Presence not published yet".to_string()
    },
  ];
  if let Some(cursor) = peer.cursor.as_deref().filter(|value| !value.trim().is_empty()) {
    lines.push(format!("Cursor: {}", cursor.trim()));
  }
  if let Some(focus) = peer.focus.as_deref().filter(|value| !value.trim().is_empty()) {
    lines.push(format!("Focus: {}", focus.trim()));
  }
  if let Some(viewport_hint) = peer
    .viewport_hint
    .as_deref()
    .filter(|value| !value.trim().is_empty())
  {
    lines.push(format!("Viewport: {}", viewport_hint.trim()));
  }
  lines.join("\n")
}

fn short_hash(hash: &[u8; 32]) -> String {
  hash[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}
