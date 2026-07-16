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
          .tooltip_with_action("Save current file", &Save, context_for(CommandId::Save))
          .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.save_active(window, cx);
          })),
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
    self.sync_collab_notice_subscriptions(window, cx);

    let speech_word_count = if let (Some(document_id), Some(editor)) = (self.active_document_id, self.active_editor.as_ref()) {
      let editor = editor.read(cx);
      let generation = editor.edit_generation();
      if let Some((_, speech, total)) = self
        .speech_word_count_cache
        .get(&document_id)
        .filter(|(cached_generation, _, _)| *cached_generation == generation)
      {
        Some((*speech, *total))
      } else if self.speech_word_count_pending.contains(&document_id) {
        None
      } else {
        self.speech_word_count_pending.insert(document_id);
        // §A10.6: the O(doc) walk used to run right here on the FOREGROUND
        // executor, once per keystroke. Now: clone the projection (cheap —
        // persistent, structurally-shared trees), take this document's
        // per-paragraph memo, and count on the background executor. Only
        // paragraphs whose (id, version) memo went stale are recounted.
        let document = editor.document().clone();
        let paragraph_cache = SPEECH_WORD_COUNT_PARAGRAPH_CACHES
          .lock()
          .expect("speech word-count paragraph cache lock poisoned")
          .remove(&document_id)
          .unwrap_or_default();
        cx.spawn(async move |this, cx| {
          let (speech, total, paragraph_cache) = cx
            .background_executor()
            .spawn(async move {
              let mut paragraph_cache = paragraph_cache;
              let (speech, total, _recounted) = speech_word_count_incremental(&document, &mut paragraph_cache);
              (speech, total, paragraph_cache)
            })
            .await;
          let _ = this.update(cx, |this, cx| {
            let is_open = this.document_panels.iter().any(|panel| panel.read(cx).id() == document_id);
            if is_open {
              this.speech_word_count_cache.insert(document_id, (generation, speech, total));
            }
            this.speech_word_count_pending.remove(&document_id);
            // Return the memo for the next recount, and prune memos of
            // documents that closed while a count was in flight (the
            // workspace-side total cache is pruned in `close_document`; this
            // module-scope map is pruned here).
            let open_ids = this
              .document_panels
              .iter()
              .map(|panel| panel.read(cx).id())
              .collect::<FxHashSet<_>>();
            let mut caches = SPEECH_WORD_COUNT_PARAGRAPH_CACHES
              .lock()
              .expect("speech word-count paragraph cache lock poisoned");
            if is_open {
              caches.insert(document_id, paragraph_cache);
            }
            caches.retain(|id, _| open_ids.contains(id));
            drop(caches);
            cx.notify();
          });
        })
        .detach();
        None
      }
    } else {
      None
    };
    let zoom = self
      .active_editor
      .as_ref()
      .map(|editor| editor.read(cx).zoom_percent())
      .or_else(|| self.active_flow.as_ref().map(|flow| flow.read(cx).zoom_percent()));
    let collab_phase = self
      .active_document_id
      .and_then(|panel_id| crate::collab::phase_for_panel(panel_id, cx));
    let collab_roster = self
      .active_document_id
      .map_or_else(Vec::new, |panel_id| crate::collab::roster_for_panel(panel_id, cx));
    if let Some(percent) = zoom {
      self.sync_zoom_slider(percent, window, cx);
    }
    // SB-S2 (Patient 8, B2): zoned instrument strip — left: document identity
    // (format badge, title, save state); center: the activity slot; right:
    // collab / counters / zoom. The old bar was one flex_1 spacer and a
    // right-cluster; every zone below has an address now.
    let identity = self.active_panel_identity(cx);
    let activity = self.activity_event.clone();
    h_flex()
      .h(px(26.0))
      .flex_none()
      .w_full()
      .items_center()
      .px_2()
      .gap_2()
      .border_t(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .bg(cx.theme().background)
      .when_some(identity, |this, identity| this.child(self.render_status_identity(identity, cx)))
      .child(div().flex_1())
      .when_some(activity, |this, event| this.child(self.render_activity_slot(&event, cx)))
      .child(div().flex_1())
      .when_some(collab_phase, |this, phase| {
        // CO-S4: while attached, the session STRIP is the collab surface —
        // the pill only covers the in-between phases (creating/joining).
        if matches!(
          phase,
          crate::collab::SessionPhase::Detached(_) | crate::collab::SessionPhase::Attached(_)
        ) {
          this
        } else {
          let tooltip = format!("Collaboration: {} — click to manage", crate::collab::status::phase_label(&phase, cx));
          this.child(
            Button::new("collaboration-status-pill")
              .text()
              .compact()
              .tooltip(tooltip)
              .child(crate::collab::status::participant_group(&phase, collab_roster, cx))
              .on_click(cx.listener(|workspace, _, window, cx| workspace.open_collaboration_dialog(window, cx))),
          )
        }
      })
      .when_some(zoom, |this, percent| this.child(self.render_zoom_slider(percent, cx)))
      // SB-S4: the click-to-cycle counter slot with per-document mode memory.
      .when_some(self.status_counter_slot(speech_word_count, cx), |this, slot| this.child(slot))
  }

  /// SB-S4: what the counter slot shows for the active panel, cycling on
  /// click. Documents: speech words → total words → paragraphs. Flows:
  /// cells → sheets (cheap sync reads off the board).
  fn status_counter_slot(&self, doc_counts: Option<(usize, usize)>, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
    let panel_id = self.active_document_id?;
    let mode = self.status_counter_modes.get(&panel_id).copied().unwrap_or(0);
    let (label, tooltip): (String, &'static str) = if let Some(flow) = self.active_flow.as_ref() {
      let board = flow.read(cx).board();
      match mode % 2 {
        0 => {
          let cells: usize = board.sheets.iter().map(|sheet| sheet.cells.len()).sum();
          (format!("{cells} cells"), "Cells on this board — click to cycle")
        },
        _ => (
          format!("{} sheets", board.sheets.len()),
          "Sheets on this board — click to cycle",
        ),
      }
    } else {
      let (speech, total) = doc_counts?;
      match mode % 3 {
        0 => (
          format!("Speech: {speech} words"),
          "Words in tags, cites, and highlighted text — what gets spoken. Click to cycle",
        ),
        1 => (format!("{total} words"), "Every word in the document — click to cycle"),
        _ => {
          let paragraphs = self
            .active_editor
            .as_ref()
            .map_or(0, |editor| editor.read(cx).document().paragraphs.len());
          (format!("{paragraphs} paragraphs"), "Paragraph count — click to cycle")
        },
      }
    };
    Some(
      Button::new("status-counter-slot")
        .text()
        .compact()
        .tooltip(tooltip)
        .child(
          div()
            .text_size(px(10.0))
            .text_color(cx.theme().muted_foreground.opacity(0.82))
            .child(label),
        )
        .on_click(cx.listener(move |workspace, _, _, cx| {
          let entry = workspace.status_counter_modes.entry(panel_id).or_insert(0);
          *entry = entry.wrapping_add(1);
          cx.notify();
        }))
        .into_any_element(),
    )
  }

  /// The identity zone's inputs: format badge, title, and save state for the
  /// active panel (document or flow).
  fn active_panel_identity(&self, cx: &App) -> Option<(&'static str, SharedString, PanelSaveState, bool, Uuid)> {
    let panel_id = self.active_document_id?;
    if let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      let panel = panel.read(cx);
      let default_state = if panel.editor().read(cx).document_path().is_none() {
        PanelSaveState::Unsaved
      } else {
        PanelSaveState::Saved
      };
      let state = self.panel_save_states.get(&panel_id).cloned().unwrap_or(default_state);
      return Some((".db8", panel.title_text(), state, panel.is_dirty(cx), panel_id));
    }
    if let Some(panel) = self.flow_panels.iter().find(|panel| panel.read(cx).id() == panel_id) {
      let panel = panel.read(cx);
      let default_state = if panel.editor().read(cx).document_path().is_none() {
        PanelSaveState::Unsaved
      } else {
        PanelSaveState::Saved
      };
      let state = self.panel_save_states.get(&panel_id).cloned().unwrap_or(default_state);
      return Some((".fl0", panel.title_text(), state, panel.is_dirty(cx), panel_id));
    }
    None
  }

  fn render_status_identity(
    &self,
    (format, title, state, dirty, _panel_id): (&'static str, SharedString, PanelSaveState, bool, Uuid),
    cx: &Context<Self>,
  ) -> impl IntoElement {
    // Quiet dot, loud failure (Patient 8 pick): the dot + tooltip carry the
    // healthy states; text appears only when something is wrong.
    let (dot_color, tooltip, failure_text): (Hsla, SharedString, Option<SharedString>) = match &state {
      PanelSaveState::Failed { message } => (
        cx.theme().danger,
        format!("Save failed: {message}").into(),
        Some("Autosave failed".into()),
      ),
      PanelSaveState::Saving => (cx.theme().info, "Saving…".into(), None),
      PanelSaveState::Unsaved => (
        cx.theme().muted_foreground,
        "Not saved to disk yet — Save As to choose a location".into(),
        None,
      ),
      PanelSaveState::Saved if dirty => (
        cx.theme().muted_foreground,
        "Unsaved changes — autosave will catch up".into(),
        None,
      ),
      PanelSaveState::Saved => (cx.theme().success, "Saved".into(), None),
    };
    h_flex()
      .flex_none()
      .items_center()
      .gap_1p5()
      .child(
        div()
          .text_size(px(9.0))
          .px_1()
          .rounded_sm()
          .border_1()
          .border_color(cx.theme().border)
          .text_color(cx.theme().muted_foreground)
          .child(format),
      )
      .child(
        div()
          .text_size(px(10.5))
          .max_w(px(220.0))
          .overflow_hidden()
          .text_ellipsis()
          .text_color(cx.theme().foreground.opacity(0.85))
          .child(title),
      )
      .child(
        div()
          .id("save-state-dot")
          .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
          .child(div().size(px(6.0)).rounded_full().bg(dot_color)),
      )
      .when_some(failure_text, |this, text| {
        this.child(div().text_size(px(10.0)).text_color(cx.theme().danger).child(text))
      })
      // CT-S1: the invisibility MODE INDICATOR. The ribbon chip's selected
      // tint was the only signal that the document is showing a filtered
      // read view — a mode that changes what every keystroke means deserves
      // a standing marker in the identity zone.
      .when(self.active_editor_invisibility_mode(cx), |this| {
        this.child(
          div()
            .id("invisibility-mode-chip")
            .text_size(px(9.0))
            .px_1()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().primary.opacity(0.7))
            .text_color(cx.theme().primary)
            .tooltip(|window, cx| {
              gpui_component::tooltip::Tooltip::new(
                "Invisibility mode \u{2014} showing only what gets read in round. Typing on hidden text restyles its paragraph to Analytic.",
              )
              .build(window, cx)
            })
            .child("INVIS"),
        )
      })
  }

  /// CT-S1: whether the active tab's editor is in invisibility mode.
  fn active_editor_invisibility_mode(&self, cx: &App) -> bool {
    self
      .active_editor
      .as_ref()
      .is_some_and(|editor| editor.read(cx).invisibility_mode())
  }

  fn render_activity_slot(&self, event: &ActivityEvent, cx: &mut Context<Self>) -> impl IntoElement {
    let failure = event.kind == ActivityKind::Failure;
    let color = if failure {
      cx.theme().danger
    } else {
      cx.theme().muted_foreground.opacity(0.8)
    };
    // D-S3: failures sit on an engine-derived surface (elevation law) so the
    // one persistent chip in the bar reads as an object, not loose text.
    let surface = failure.then(|| crate::visual_engine::chrome_surface(cx, cx.theme().danger, 0.04));
    h_flex()
      .flex_none()
      .items_center()
      .gap_1()
      .when_some(surface, |this, surface| {
        this
          .px_1p5()
          .rounded_sm()
          .bg(surface.fill)
          .when_some(surface.hairline, |this, hairline| this.border_1().border_color(hairline))
      })
      .child(div().text_size(px(10.0)).text_color(color).child(event.message.clone()))
      .when(failure, |this| {
        let action = event.action.clone();
        this
          .when_some(action, |this, action| {
            this.child(
              Button::new("activity-action")
                .text()
                .compact()
                .text_color(cx.theme().danger)
                .child(div().text_size(px(10.0)).child("Retry"))
                .on_click(cx.listener(move |workspace, _, window, cx| {
                  workspace.run_activity_action(action.clone(), window, cx);
                })),
            )
          })
          .child(
            Button::new("activity-dismiss")
              .text()
              .compact()
              .child(div().text_size(px(10.0)).text_color(cx.theme().muted_foreground).child("✕"))
              .on_click(cx.listener(|workspace, _, _, cx| workspace.dismiss_activity(cx))),
          )
      })
  }

  fn sync_collab_notice_subscriptions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    // §perf: with no existing subscriptions and no panel in a collab session, the
    // full scan/retain is a no-op — skip building the set entirely (the common case).
    if self.collab_notice_subscriptions.is_empty()
      && !self
        .document_panels
        .iter()
        .any(|panel| crate::collab::session_for_panel(panel.read(cx).id(), cx).is_some())
    {
      return;
    }

    // §perf: FxHashSet (SessionId keys are trusted/local) with capacity for the panels.
    let mut active_sessions: FxHashSet<flowstate_collab::SessionId> =
      FxHashSet::with_capacity_and_hasher(self.document_panels.len(), rustc_hash::FxBuildHasher);
    let window_handle = window.window_handle();

    for panel in &self.document_panels {
      let panel_id = panel.read(cx).id();
      let Some(session) = crate::collab::session_for_panel(panel_id, cx) else {
        continue;
      };
      let session_id = session.read(cx).session_id();
      active_sessions.insert(session_id);
      if self.collab_notice_subscriptions.contains_key(&session_id) {
        continue;
      }

      let notice_window_handle = window_handle;
      let subscription = cx.subscribe(&session, move |workspace, _, notice: &crate::collab::SessionNotice, cx| {
        let notice = notice.clone();
        if let crate::collab::SessionNotice::IncompatibleVersion(peer) = &notice
          && !workspace.collab_incompatible_version_notices.insert(peer.clone())
        {
          return;
        }
        if let Err(error) = notice_window_handle.update(cx, |_, window, cx| {
          crate::collab::notify::show_session_notice(&notice, window, cx);
        }) {
          tracing::warn!("showing collaboration notification failed: {error}");
        }
      });
      self.collab_notice_subscriptions.insert(session_id, subscription);
    }

    self
      .collab_notice_subscriptions
      .retain(|session_id, _| active_sessions.contains(session_id));
  }
}

// §A10.6 — status-bar speech word count.
//
// Per-document, per-paragraph word-count memo. It lives at module scope keyed
// by the owning document panel's id (this file is spliced into the `Workspace`
// module via `include!`, and the recount round-trips the map through the
// background executor); the workspace-side `speech_word_count_cache` keeps only
// the finished `(edit_generation, total)` per document.
//
// A memo entry is reused only while BOTH stamps match:
// - `version`: bumped by every editor content/formatting edit to the paragraph;
// - `shape`: FxHash of `(style, runs)` — guards the full projection rebuilds
//   (remote/structural imports) that reset `version` to 0, whenever they change
//   a paragraph's run structure, lengths, or styling.
struct SpeechWordCountEntry {
  version: u64,
  shape: u64,
  words: usize,
  /// SB-S4: total words in the paragraph (same walk, second accumulator).
  total_words: usize,
}

type SpeechWordCountParagraphCache = FxHashMap<flowstate_document::ParagraphId, SpeechWordCountEntry>;

// §perf: FxHash for trusted, locally-generated Uuid keys.
static SPEECH_WORD_COUNT_PARAGRAPH_CACHES: std::sync::LazyLock<std::sync::Mutex<FxHashMap<Uuid, SpeechWordCountParagraphCache>>> =
  std::sync::LazyLock::new(|| std::sync::Mutex::new(FxHashMap::default()));

fn paragraph_shape_stamp(paragraph: &flowstate_document::Paragraph) -> u64 {
  use std::hash::{Hash, Hasher};
  let mut hasher = rustc_hash::FxHasher::default();
  paragraph.style.hash(&mut hasher);
  paragraph.runs.hash(&mut hasher);
  hasher.finish()
}

/// Incremental speech word count: sums per-paragraph counts, reusing the memo
/// for every paragraph whose `(version, shape)` stamps are unchanged and
/// recounting only the rest. `cache` is rebuilt to exactly the live paragraph
/// ids, so entries of deleted paragraphs are evicted. Returns
/// `(total, recounted_paragraphs)`; the second element backs the
/// incrementality tripwire in tests.
fn speech_word_count_incremental(document: &DocumentProjection, cache: &mut SpeechWordCountParagraphCache) -> (usize, usize, usize) {
  let mut fresh: SpeechWordCountParagraphCache =
    FxHashMap::with_capacity_and_hasher(document.paragraphs.len(), rustc_hash::FxBuildHasher);
  let mut total = 0usize;
  let mut total_words = 0usize;
  let mut recounted = 0usize;
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let paragraph_id = document.ids.paragraph_ids.get(paragraph_ix).copied();
    let shape = paragraph_shape_stamp(paragraph);
    let entry = match paragraph_id.and_then(|id| cache.remove(&id)) {
      Some(entry) if entry.version == paragraph.version && entry.shape == shape => entry,
      _ => {
        recounted += 1;
        SpeechWordCountEntry {
          version: paragraph.version,
          shape,
          words: paragraph_speech_word_count(document, paragraph_ix, paragraph),
          total_words: count_words_in_document_range(document, flowstate_document::paragraph_byte_range(document, paragraph_ix)),
        }
      },
    };
    total += entry.words;
    total_words += entry.total_words;
    if let Some(paragraph_id) = paragraph_id {
      fresh.insert(paragraph_id, entry);
    }
  }
  *cache = fresh;
  (total, total_words, recounted)
}

/// One paragraph's word count, with EXACTLY the original full walk's
/// semantics: a run's text counts iff the paragraph is a tag paragraph or the
/// run is cite-semantic or highlighted, and each counted run is word-split
/// independently (`split_whitespace` boundaries, keeping only words containing
/// at least one alphanumeric char) — so a word straddling a run boundary still
/// contributes once per counted run it touches.
fn paragraph_speech_word_count(document: &DocumentProjection, paragraph_ix: usize, paragraph: &flowstate_document::Paragraph) -> usize {
  let paragraph_is_tag = paragraph.style == flowstate_document::PARAGRAPH_TAG;
  let run_counts = |run: &flowstate_document::TextRun| {
    paragraph_is_tag || run.styles.semantic == flowstate_document::SEMANTIC_CITE || run.styles.highlight.is_some()
  };
  // Skip the byte-range derivation for paragraphs with no counted runs.
  if !paragraph.runs.iter().any(run_counts) {
    return 0;
  }
  let mut run_start = flowstate_document::paragraph_byte_range(document, paragraph_ix).start;
  let mut words = 0usize;
  for run in &paragraph.runs {
    let run_end = run_start + run.len;
    if run_counts(run) {
      words += count_words_in_document_range(document, run_start..run_end);
    }
    run_start = run_end;
  }
  words
}

/// Word count over a byte range of the document rope, streamed chunk-by-chunk
/// with no per-run String materialization (`document_text_slice` here was
/// 853k calls / 306.8 MB in the hotpath profile). Semantically identical to
/// `slice.split_whitespace().filter(|word| word.chars().any(char::is_alphanumeric)).count()`
/// over the materialized slice: `split_whitespace` breaks on
/// `char::is_whitespace`, and a token counts iff any of its chars is
/// alphanumeric.
fn count_words_in_document_range(document: &DocumentProjection, range: std::ops::Range<usize>) -> usize {
  let len = document.text.byte_len();
  let start = range.start.min(len);
  let end = range.end.min(len);
  if start >= end {
    return 0;
  }
  let mut words = 0usize;
  let mut in_word = false;
  let mut word_has_alphanumeric = false;
  for chunk in document.text.byte_slice(start..end).chunks() {
    for character in chunk.chars() {
      if character.is_whitespace() {
        if in_word && word_has_alphanumeric {
          words += 1;
        }
        in_word = false;
        word_has_alphanumeric = false;
      } else {
        in_word = true;
        word_has_alphanumeric = word_has_alphanumeric || character.is_alphanumeric();
      }
    }
  }
  if in_word && word_has_alphanumeric {
    words += 1;
  }
  words
}

#[cfg(test)]
mod speech_word_count_tests {
  use super::*;
  use crate::rich_text_element::{HighlightStyle, RunStyles};

  fn plain(text: &str) -> InputRun {
    InputRun {
      text: text.to_string(),
      styles: RunStyles::default(),
    }
  }

  fn cite(text: &str) -> InputRun {
    InputRun {
      text: text.to_string(),
      styles: RunStyles {
        semantic: flowstate_document::SEMANTIC_CITE,
        ..RunStyles::default()
      },
    }
  }

  fn highlighted(text: &str) -> InputRun {
    InputRun {
      text: text.to_string(),
      styles: RunStyles {
        highlight: Some(HighlightStyle::Custom(0)),
        ..RunStyles::default()
      },
    }
  }

  fn fixture_inputs() -> Vec<InputParagraph> {
    vec![
      // Tag paragraph: EVERY run counts, styled or not (6 words).
      InputParagraph {
        style: flowstate_document::PARAGRAPH_TAG,
        runs: vec![plain("Tag heading words here"), cite(" plus cite")],
      },
      // Normal paragraph: only the cite run counts; "spl|it" straddles the
      // cite→plain boundary and must split per run (3 words, not 2 or 5).
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![cite("cited evidence spl"), plain("it uncounted words")],
      },
      // A straddle between two counted runs stays two run-local tokens
      // ("foo" + "bar baz" = 3 words, not "foobar baz" = 2).
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![highlighted("foo"), cite("bar baz")],
      },
      // Punctuation-only tokens are filtered; U+2028 is whitespace (2 words).
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![highlighted("-- !! a1\u{2028}b2"), plain("nothing here counts")],
      },
      // No counted runs at all (0 words).
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("entirely uncounted text")],
      },
    ]
  }

  fn build_document(inputs: Vec<InputParagraph>, ids: &[u128]) -> DocumentProjection {
    assert_eq!(inputs.len(), ids.len());
    let mut document = document_from_input(flowstate_document_theme(), inputs);
    document.ids.paragraph_ids = Arc::new(ids.iter().copied().map(flowstate_document::ParagraphId).collect());
    document
  }

  /// The pre-A10.6 full walk, verbatim — the counting-semantics oracle.
  fn speech_word_count_reference(document: &DocumentProjection) -> usize {
    document
      .paragraphs
      .iter()
      .enumerate()
      .map(|(paragraph_ix, paragraph)| {
        let paragraph_is_tag = paragraph.style == flowstate_document::PARAGRAPH_TAG;
        let mut run_start = flowstate_document::paragraph_byte_range(document, paragraph_ix).start;
        paragraph
          .runs
          .iter()
          .map(|run| {
            let run_end = run_start + run.len;
            let count = if paragraph_is_tag || run.styles.semantic == flowstate_document::SEMANTIC_CITE || run.styles.highlight.is_some() {
              document_text_slice(document, run_start..run_end)
                .split_whitespace()
                .filter(|word| word.chars().any(char::is_alphanumeric))
                .count()
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

  #[test]
  fn incremental_speech_word_count_matches_fresh_full_count_across_edits() {
    let ids: Vec<u128> = (1..=5).collect();
    let base = build_document(fixture_inputs(), &ids);
    let mut cache = SpeechWordCountParagraphCache::default();

    // Cold count == the oracle, every paragraph recounted.
    let (initial, _, recounted) = speech_word_count_incremental(&base, &mut cache);
    assert_eq!(initial, speech_word_count_reference(&base));
    assert_eq!(initial, 14, "fixture semantics moved: tag=6, cite-straddle=3, two-run straddle=3, filtered=2, uncounted=0");
    assert_eq!(recounted, base.paragraphs.len());

    // Unchanged document: everything served from the memo.
    let (unchanged, _, recounted) = speech_word_count_incremental(&base, &mut cache);
    assert_eq!(unchanged, initial);
    assert_eq!(recounted, 0);

    // (a1) Same-length text swap + the editor's content-edit version bump:
    // version alone must invalidate the memo (shape is unchanged).
    let mut inputs = fixture_inputs();
    inputs[1].runs[0] = cite("citedevidences spl"); // same 18 bytes, 2 words instead of 3
    let mut edited = build_document(inputs, &ids);
    let mut bumped = edited.paragraphs[1].clone();
    bumped.version = bumped.version.wrapping_add(1);
    edited.paragraphs.set(1, bumped);
    let (count, _, recounted) = speech_word_count_incremental(&edited, &mut cache);
    assert_eq!(count, speech_word_count_reference(&edited));
    assert_eq!(count, 13);
    assert_eq!(recounted, 1, "only the edited paragraph recounts");

    // (a2) Length-changing edit to the same paragraph.
    let mut inputs = fixture_inputs();
    inputs[1].runs[0] = cite("cited evidence rewritten with more words spl");
    let mut edited = build_document(inputs, &ids);
    let mut bumped = edited.paragraphs[1].clone();
    bumped.version = bumped.version.wrapping_add(1);
    edited.paragraphs.set(1, bumped);
    let (count, _, recounted) = speech_word_count_incremental(&edited, &mut cache);
    assert_eq!(count, speech_word_count_reference(&edited));
    assert_eq!(count, 18);
    assert_eq!(recounted, 1);

    // (b) Delete a paragraph: nothing recounts, its memo entry is evicted.
    let mut inputs = fixture_inputs();
    inputs.remove(1);
    let mut remaining_ids = ids.clone();
    remaining_ids.remove(1);
    let deleted = build_document(inputs, &remaining_ids);
    let (count, _, recounted) = speech_word_count_incremental(&deleted, &mut cache);
    assert_eq!(count, speech_word_count_reference(&deleted));
    assert_eq!(count, 11);
    assert_eq!(recounted, 0, "surviving paragraphs are all memo hits");
    assert!(!cache.contains_key(&flowstate_document::ParagraphId(2)), "deleted paragraph's memo must be evicted");
    assert_eq!(cache.len(), deleted.paragraphs.len());

    // (c) Insert a paragraph: only the new one recounts.
    let mut inputs = fixture_inputs();
    inputs.remove(1);
    inputs.insert(
      2,
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![cite("brand new inserted card"), plain(" tail")],
      },
    );
    let mut inserted_ids = remaining_ids.clone();
    inserted_ids.insert(2, 99);
    let inserted = build_document(inputs, &inserted_ids);
    let (count, _, recounted) = speech_word_count_incremental(&inserted, &mut cache);
    assert_eq!(count, speech_word_count_reference(&inserted));
    assert_eq!(count, 15);
    assert_eq!(recounted, 1, "only the inserted paragraph recounts");
    assert!(cache.contains_key(&flowstate_document::ParagraphId(99)));
    assert_eq!(cache.len(), inserted.paragraphs.len());
  }
}
