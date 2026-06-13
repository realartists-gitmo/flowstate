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

  fn render_document_tab_bar_prefix(&self, _active_index: usize, _tab_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
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

  fn render_status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let speech_word_count = if let (Some(document_id), Some(editor)) = (self.active_document_id, self.active_editor.as_ref()) {
      let editor = editor.read(cx);
      let generation = editor.edit_generation();
      if let Some((_, count)) = self
        .speech_word_count_cache
        .get(&document_id)
        .filter(|(cached_generation, _)| *cached_generation == generation)
      {
        Some(*count)
      } else if self.speech_word_count_pending.contains(&document_id) {
        None
      } else {
        self.speech_word_count_pending.insert(document_id);
        let document = editor.document().clone();
        cx.spawn(async move |this, cx| {
          let count = speech_word_count(&document);
          let _ = this.update(cx, |this, cx| {
            let is_open = this.document_panels.iter().any(|panel| panel.read(cx).id() == document_id);
            if is_open {
              this.speech_word_count_cache.insert(document_id, (generation, count));
            }
            this.speech_word_count_pending.remove(&document_id);
            cx.notify();
          });
        })
        .detach();
        None
      }
    } else {
      None
    };
    let zoom = self.active_editor.as_ref().map(|editor| editor.read(cx).zoom_percent());
    let collab_phase = self
      .active_document_id
      .and_then(|panel_id| crate::collab::phase_for_panel(panel_id, cx));
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
      .when_some(collab_phase, |this, phase| {
        if matches!(phase, crate::collab::SessionPhase::Detached(_)) {
          this
        } else {
          this.child(
            Button::new("collaboration-status-pill")
              .text()
              .compact()
              .child(crate::collab::status::status_pill(&phase, cx))
              .on_click(cx.listener(|workspace, _, window, cx| workspace.open_collaboration_dialog(window, cx))),
          )
        }
      })
      .when_some(zoom, |this, percent| this.child(self.render_zoom_slider(percent, cx)))
      .when_some(speech_word_count, |this, count| {
        this.child(
          div()
            .flex_none()
            .pl_2()
            .text_size(px(10.0))
            .text_color(cx.theme().muted_foreground.opacity(0.82))
            .child(format!("Speech: {count} words")),
        )
      })
  }
}

fn speech_word_count(document: &Document) -> usize {
  document
    .paragraphs
    .iter()
    .map(|paragraph| {
      let paragraph_is_tag = paragraph.style == flowstate_document::PARAGRAPH_TAG;
      let mut run_start = paragraph.byte_range.start;
      paragraph
        .runs
        .iter()
        .map(|run| {
          let run_end = run_start + run.len;
          let count = if paragraph_is_tag || run.styles.semantic == flowstate_document::SEMANTIC_CITE || run.styles.highlight.is_some() {
            count_words(&document_text_slice(document, run_start..run_end))
          } else {
            0
          };
          run_start = run_end;
          count
        })
        .sum::<usize>()
    })
    .sum()
}

fn count_words(text: &str) -> usize {
  text
    .split_whitespace()
    .filter(|word| word.chars().any(char::is_alphanumeric))
    .count()
}
