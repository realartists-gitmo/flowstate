//! H-S2/H-S3: the history takeover — history mode commandeers the viewport
//! read-only. Ledger column (session groups, pins prominent) + preview +
//! tape (scrub over the revision timeline, debounced checkouts through the
//! H-K0 frontier view) + action bar (Restore / Fork / Name this moment /
//! Checkpoint now / Exit). Replaces the 520px revision-list dialog.
//!
//! Laws in force: rail-vs-takeover (history is comparative reading — it gets
//! the room), live surface (new checkpoints land while open), honest verbs
//! (Restore is a FORWARD op behind a safety pin — the runtime enforces the
//! law), failures speak (activity zone), keyboard paths (Escape exits).

use std::collections::HashSet;

use flowstate_collab::{
  SessionId,
  crdt_runtime::{RuntimeEvent, RuntimeFrontierDiff, RuntimeRevisionInfo},
  doc_io::DocIoHandle,
};
use flowstate_document::RevisionKind;
use gpui::AnimationExt as _;
use gpui::{
  App, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
  SharedString, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Disableable, Sizable as _, StyledExt as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputEvent, InputState},
  scroll::ScrollableElement,
  v_flex,
};
use uuid::Uuid;

use crate::rich_text_element::RichTextEditor;
use crate::workspace::Workspace;

/// Scrub debounce: the tape can fire selections faster than checkouts should
/// run; only the resting position checks out.
const CHECKOUT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);
/// A gap this long between records starts a new ledger session group.
const SESSION_GAP_SECS: i64 = 30 * 60;

pub struct HistoryTakeover {
  workspace: WeakEntity<Workspace>,
  pub(crate) panel_id: Uuid,
  io: DocIoHandle,
  live_editor: Entity<RichTextEditor>,
  /// Read-only display surface (no write authority — Loro-first invariant 5).
  preview: Entity<RichTextEditor>,
  /// Oldest-first for the tape; the ledger reverses per group.
  revisions: Vec<RuntimeRevisionInfo>,
  selected: Option<u128>,
  /// The frontier currently shown in the preview (None = live now).
  shown_frontier: Option<Vec<u8>>,
  loading_preview: bool,
  error: Option<SharedString>,
  rename_input: Entity<InputState>,
  renaming: Option<u128>,
  pin_input: Entity<InputState>,
  pinning: bool,
  busy: bool,
  /// H-S5: paint what changed between the shown moment and now ("vs now"
  /// default; any-two-points selection is a recorded deferral).
  diff_enabled: bool,
  diff: Option<RuntimeFrontierDiff>,
  collapsed_groups: HashSet<usize>,
  checkout_generation: u64,
  refresh_pending: bool,
  _editor_observation: Subscription,
  _rename_subscription: Subscription,
}

impl HistoryTakeover {
  pub fn new(
    workspace: WeakEntity<Workspace>,
    panel_id: Uuid,
    io: DocIoHandle,
    live_editor: Entity<RichTextEditor>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    // The preview opens on "now": the live projection, read-only.
    let current = live_editor.read(cx).document().clone();
    let preview = cx.new(|cx| RichTextEditor::new_with_path(current, None, cx));
    let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name this moment"));
    let pin_input = cx.new(|cx| InputState::new(window, cx).placeholder("Checkpoint name"));
    let _rename_subscription = cx.subscribe(&rename_input, |_takeover: &mut Self, _, _: &InputEvent, cx| cx.notify());
    // Law 6: new checkpoints (autosave grain, peer saves) land while open.
    let observed = live_editor.clone();
    let _editor_observation = cx.observe(&observed, |takeover, _, cx| takeover.schedule_refresh(cx));
    let mut takeover = Self {
      workspace,
      panel_id,
      io,
      live_editor,
      preview,
      revisions: Vec::new(),
      selected: None,
      shown_frontier: None,
      loading_preview: false,
      error: None,
      rename_input,
      renaming: None,
      pin_input,
      pinning: false,
      busy: false,
      diff_enabled: true,
      diff: None,
      collapsed_groups: HashSet::new(),
      checkout_generation: 0,
      refresh_pending: false,
      _editor_observation,
      _rename_subscription,
    };
    takeover.reload_revisions(true, cx);
    takeover
  }

  /// How many records the ledger currently holds (headless tests).
  #[must_use]
  pub fn revision_count(&self) -> usize {
    self.revisions.len()
  }

  fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
    if self.refresh_pending {
      return;
    }
    self.refresh_pending = true;
    cx.spawn(async move |takeover, cx| {
      cx.background_executor().timer(CHECKOUT_DEBOUNCE).await;
      let _ = takeover.update(cx, |takeover, cx| {
        takeover.refresh_pending = false;
        takeover.reload_revisions(false, cx);
      });
    })
    .detach();
  }

  fn reload_revisions(&mut self, collapse_old_groups: bool, cx: &mut Context<Self>) {
    let io = self.io.clone();
    cx.spawn(async move |takeover, cx| {
      let result = io.revisions().await;
      let _ = takeover.update(cx, |takeover, cx| {
        match result {
          Ok(mut revisions) => {
            // revisions() returns newest-first; the tape reads oldest-first.
            revisions.reverse();
            takeover.revisions = revisions;
            takeover.error = None;
            if collapse_old_groups {
              // Every group but the newest starts collapsed; pins stay visible.
              let groups = takeover.session_groups().len();
              takeover.collapsed_groups = (0..groups.saturating_sub(1)).collect();
            }
          },
          Err(error) => takeover.error = Some(format!("Loading history failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  /// Ledger grouping: consecutive records within [`SESSION_GAP_SECS`] share a
  /// group (oldest-first, same order as `revisions`).
  fn session_groups(&self) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (ix, revision) in self.revisions.iter().enumerate() {
      match groups.last_mut() {
        Some(group)
          if revision.created_at_unix_secs - self.revisions[group.end - 1].created_at_unix_secs <= SESSION_GAP_SECS =>
        {
          group.end = ix + 1;
        },
        _ => groups.push(ix..ix + 1),
      }
    }
    groups
  }

  fn revision(&self, revision_id: u128) -> Option<&RuntimeRevisionInfo> {
    self.revisions.iter().find(|revision| revision.revision_id == revision_id)
  }

  fn select(&mut self, revision_id: u128, cx: &mut Context<Self>) {
    if self.selected == Some(revision_id) {
      return;
    }
    self.selected = Some(revision_id);
    self.renaming = None;
    cx.notify();
    self.schedule_checkout(cx);
  }

  /// Debounced H-K0 checkout of the selected record into the preview.
  fn schedule_checkout(&mut self, cx: &mut Context<Self>) {
    self.checkout_generation = self.checkout_generation.wrapping_add(1);
    let generation = self.checkout_generation;
    self.loading_preview = true;
    cx.notify();
    cx.spawn(async move |takeover, cx| {
      cx.background_executor().timer(CHECKOUT_DEBOUNCE).await;
      let Ok(Some((io, frontier))) = takeover.update(cx, |takeover, _| {
        if takeover.checkout_generation != generation {
          return None;
        }
        let frontier = takeover
          .selected
          .and_then(|id| takeover.revision(id))
          .map(|revision| revision.frontier.clone())?;
        Some((takeover.io.clone(), frontier))
      }) else {
        return;
      };
      let result = io.open_frontier(frontier.clone()).await;
      let _ = takeover.update(cx, |takeover, cx| {
        if takeover.checkout_generation != generation {
          return;
        }
        takeover.loading_preview = false;
        match result {
          Ok(events) => {
            let document = events.into_iter().find_map(|event| match event {
              RuntimeEvent::FrontierViewOpened { document, .. } => Some(*document),
              _ => None,
            });
            match document {
              Some(document) => {
                takeover
                  .preview
                  .update(cx, |preview, cx| preview.replace_document_projection(document, cx));
                takeover.shown_frontier = Some(frontier);
                takeover.error = None;
                takeover.refresh_diff(generation, cx);
              },
              None => takeover.error = Some("The historical view returned no document".into()),
            }
          },
          Err(error) => takeover.error = Some(format!("Checking out that moment failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  /// H-S5: fetch the shown-moment→now diff and dress the preview — removed
  /// text gets author-colored dashed underlines (the same review dress the
  /// comments panel uses), insertions are summarized in the header.
  fn refresh_diff(&mut self, generation: u64, cx: &mut Context<Self>) {
    if !self.diff_enabled {
      return;
    }
    let Some(frontier) = self.shown_frontier.clone() else { return };
    let io = self.io.clone();
    cx.spawn(async move |takeover, cx| {
      let result = io.frontier_diff(frontier, None).await;
      let _ = takeover.update(cx, |takeover, cx| {
        if takeover.checkout_generation != generation || !takeover.diff_enabled {
          return;
        }
        match result {
          Ok(diff) => {
            takeover.paint_diff(&diff, cx);
            takeover.diff = Some(diff);
          },
          Err(error) => takeover.error = Some(format!("Computing the diff failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn paint_diff(&self, diff: &RuntimeFrontierDiff, cx: &mut Context<Self>) {
    let marks: Vec<crate::rich_text_element::ExternalSelection> = diff
      .removed_since
      .iter()
      .map(|span| crate::rich_text_element::ExternalSelection {
        selection: crate::rich_text_element::EditorSelection::range(span.start, span.end),
        color_rgb: span
          .author_user_id
          .map_or(0x008a_8a8a, SessionId::color_for_user),
      })
      .collect();
    self
      .preview
      .update(cx, |preview, cx| preview.set_annotation_selections(marks, cx));
  }

  fn clear_diff(&mut self, cx: &mut Context<Self>) {
    self.diff = None;
    self
      .preview
      .update(cx, |preview, cx| preview.set_annotation_selections(Vec::new(), cx));
  }

  fn toggle_diff(&mut self, cx: &mut Context<Self>) {
    self.diff_enabled = !self.diff_enabled;
    if self.diff_enabled {
      self.refresh_diff(self.checkout_generation, cx);
    } else {
      self.clear_diff(cx);
    }
    cx.notify();
  }

  /// H-S4 restore: forward op behind the runtime-enforced safety pin. The
  /// returned projection is applied to the LIVE editor; peers converge on the
  /// broadcast (the collab session drains the publish queue as usual).
  fn restore_selected(&mut self, cx: &mut Context<Self>) {
    let Some(frontier) = self
      .selected
      .and_then(|id| self.revision(id))
      .map(|revision| revision.frontier.clone())
    else {
      return;
    };
    if self.busy {
      return;
    }
    self.busy = true;
    let io = self.io.clone();
    let live = self.live_editor.clone();
    cx.spawn(async move |takeover, cx| {
      let result = io.restore_frontier(frontier).await;
      let _ = takeover.update(cx, |takeover, cx| {
        takeover.busy = false;
        match result {
          Ok(events) => {
            if let Some(document) = events.into_iter().rev().find_map(|event| match event {
              RuntimeEvent::ProjectionUpdated { document, .. } => Some(*document),
              _ => None,
            }) {
              live.update(cx, |editor, cx| editor.replace_document_projection(document, cx));
            }
            // Restore lands: leave history mode showing the result.
            let _ = takeover.workspace.update(cx, |workspace, cx| workspace.close_history_takeover(cx));
          },
          Err(error) => {
            takeover.error = Some(format!("Restore failed: {error:#}").into());
            cx.notify();
          },
        }
      });
    })
    .detach();
  }

  /// H-S4 fork: lineage detach via the existing fork path, labeled honestly
  /// by the workspace ("fork of X @ T" tab title).
  fn fork_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(revision_id) = self.selected else { return };
    let panel_id = self.panel_id;
    let _ = self.workspace.update(cx, |workspace, cx| {
      workspace.open_document_revision(panel_id, revision_id, window, cx);
      workspace.close_history_takeover(cx);
    });
  }

  fn commit_rename(&mut self, cx: &mut Context<Self>) {
    let Some(revision_id) = self.renaming else { return };
    let title = self.rename_input.read(cx).value().trim().to_string();
    if title.is_empty() {
      return;
    }
    self.renaming = None;
    let io = self.io.clone();
    cx.spawn(async move |takeover, cx| {
      let result = io.rename_revision(revision_id, title).await;
      let _ = takeover.update(cx, |takeover, cx| {
        if let Err(error) = result {
          takeover.error = Some(format!("Naming that moment failed: {error:#}").into());
        }
        takeover.reload_revisions(false, cx);
        cx.notify();
      });
    })
    .detach();
  }

  fn commit_pin(&mut self, cx: &mut Context<Self>) {
    let title = self.pin_input.read(cx).value().trim().to_string();
    if title.is_empty() {
      return;
    }
    self.pinning = false;
    let io = self.io.clone();
    cx.spawn(async move |takeover, cx| {
      let result = io.create_named_pin(title).await;
      let _ = takeover.update(cx, |takeover, cx| {
        match result {
          Ok(revision_id) => takeover.selected = Some(revision_id),
          Err(error) => takeover.error = Some(format!("Checkpoint failed: {error:#}").into()),
        }
        takeover.reload_revisions(false, cx);
        cx.notify();
      });
    })
    .detach();
  }

  fn exit(&mut self, cx: &mut Context<Self>) {
    let _ = self.workspace.update(cx, |workspace, cx| workspace.close_history_takeover(cx));
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
    if event.keystroke.key.as_str() == "escape" {
      self.exit(cx);
      cx.stop_propagation();
    }
  }

  fn kind_color(&self, kind: RevisionKind, cx: &App) -> gpui::Hsla {
    match kind {
      RevisionKind::Named => cx.theme().warning,
      RevisionKind::Session => cx.theme().link,
      RevisionKind::Auto => cx.theme().muted_foreground,
    }
  }
}

impl Focusable for HistoryTakeover {
  fn focus_handle(&self, cx: &App) -> FocusHandle {
    self.preview.focus_handle(cx)
  }
}

impl Render for HistoryTakeover {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let groups = self.session_groups();
    let selected = self.selected;
    let busy = self.busy;

    h_flex()
      .size_full()
      .min_h_0()
      .bg(cx.theme().background)
      .on_key_down(cx.listener(Self::on_key_down))
      // ---- ledger column ----
      .child(
        v_flex()
          .w(px(260.0))
          .flex_none()
          .h_full()
          .min_h_0()
          .border_r_1()
          .border_color(cx.theme().border)
          .child(
            h_flex()
              .h(px(34.0))
              .flex_none()
              .items_center()
              .px_2()
              .border_b_1()
              .border_color(cx.theme().border)
              .child(div().text_sm().font_semibold().child("History")),
          )
          .when_some(self.error.clone(), |this, error| {
            this.child(div().px_2().py_1().text_xs().text_color(cx.theme().danger).child(error))
          })
          .child(
            v_flex()
              .flex_1()
              .min_h_0()
              .overflow_y_scrollbar()
              .p_1()
              .children(groups.iter().enumerate().rev().map(|(group_ix, group)| {
                let collapsed = self.collapsed_groups.contains(&group_ix);
                let newest = &self.revisions[group.end - 1];
                let header: SharedString = format!(
                  "{} · {} {}",
                  format_day(newest.created_at_unix_secs),
                  group.len(),
                  if group.len() == 1 { "checkpoint" } else { "checkpoints" }
                )
                .into();
                v_flex()
                  .gap_0p5()
                  .child(
                    Button::new(("history-group", group_ix))
                      .text()
                      .compact()
                      .child(
                        div()
                          .text_xs()
                          .text_color(cx.theme().muted_foreground)
                          .child(format!("{} {header}", if collapsed { "▸" } else { "▾" })),
                      )
                      .on_click(cx.listener(move |takeover, _, _, cx| {
                        if !takeover.collapsed_groups.remove(&group_ix) {
                          takeover.collapsed_groups.insert(group_ix);
                        }
                        cx.notify();
                      })),
                  )
                  .children(group.clone().rev().filter_map(|ix| {
                    let revision = &self.revisions[ix];
                    // Collapsed groups still show their pins (pins prominent).
                    if collapsed && revision.kind != RevisionKind::Named {
                      return None;
                    }
                    Some(self.render_ledger_row(revision, selected == Some(revision.revision_id), cx))
                  }))
                  .into_any_element()
              })),
          )
          .when(self.revisions.is_empty(), |this| {
            this.child(
              div()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No checkpoints yet. Saves and autosaves will appear here."),
            )
          }),
      )
      // ---- preview + tape + actions ----
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .h_full()
          .min_h_0()
          .child(
            h_flex()
              .h(px(30.0))
              .flex_none()
              .items_center()
              .justify_between()
              .px_2()
              .border_b_1()
              .border_color(cx.theme().border)
              .bg(cx.theme().warning.opacity(0.08))
              .child(
                div().text_xs().text_color(cx.theme().muted_foreground).child(
                  match (self.loading_preview, selected.and_then(|id| self.revision(id))) {
                    (true, _) => SharedString::from("Checking out…"),
                    (false, Some(revision)) => {
                      format!("Read-only view · {} · {}", revision.title, format_time(revision.created_at_unix_secs)).into()
                    },
                    (false, None) => SharedString::from("Read-only view · now"),
                  },
                ),
              )
              .child(
                h_flex()
                  .gap_2()
                  .items_center()
                  // H-S5: author legend + change counts for the shown diff.
                  .when_some(self.diff.as_ref().filter(|_| self.diff_enabled), |this, diff| {
                    let mut authors: Vec<(Option<u128>, String)> = Vec::new();
                    for span in &diff.removed_since {
                      let name = span.author_display_name.clone().unwrap_or_else(|| "Unknown".to_string());
                      if !authors.iter().any(|(id, _)| *id == span.author_user_id) {
                        authors.push((span.author_user_id, name));
                      }
                    }
                    this
                      .children(authors.into_iter().map(|(user_id, name)| {
                        h_flex()
                          .gap_1()
                          .items_center()
                          .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(gpui::Hsla::from(gpui::rgb(
                            user_id.map_or(0x008a_8a8a, SessionId::color_for_user),
                          ))))
                          .child(div().text_xs().text_color(cx.theme().muted_foreground).child(name))
                      }))
                      .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!(
                        "−{} · +{} since",
                        diff.removed_chars, diff.inserted_chars
                      )))
                  })
                  .child(
                    Button::new("history-diff-toggle")
                      .text()
                      .compact()
                      .tooltip("Underline what no longer exists in the present, by author")
                      .child(div().text_xs().text_color(if self.diff_enabled { cx.theme().warning } else { cx.theme().muted_foreground }).child("Diff vs now"))
                      .on_click(cx.listener(|takeover, _, _, cx| takeover.toggle_diff(cx))),
                  )
                  .child(
                    Button::new("history-exit-top")
                      .text()
                      .compact()
                      .child(div().text_xs().child("Exit history"))
                      .on_click(cx.listener(|takeover, _, _, cx| takeover.exit(cx))),
                  ),
              ),
          )
          .child(div().flex_1().min_h_0().overflow_hidden().child(self.preview.clone()))
          // ---- the tape ----
          .child(
            h_flex()
              .h(px(44.0))
              .flex_none()
              .items_end()
              .gap_0p5()
              .px_3()
              .pb_1()
              .border_t_1()
              .border_color(cx.theme().border)
              .children(self.revisions.iter().map(|revision| {
                let is_selected = selected == Some(revision.revision_id);
                let (height, width) = match revision.kind {
                  RevisionKind::Named => (px(26.0), px(5.0)),
                  RevisionKind::Session => (px(18.0), px(3.0)),
                  RevisionKind::Auto => (px(10.0), px(3.0)),
                };
                let color = self.kind_color(revision.kind, cx);
                let revision_id = revision.revision_id;
                let tooltip: SharedString = format!("{} · {}", revision.title, format_time(revision.created_at_unix_secs)).into();
                div()
                  .id(("history-tick", revision_id as u64))
                  .flex_1()
                  .min_w(px(3.0))
                  .max_w(px(14.0))
                  .h(px(30.0))
                  .flex()
                  .items_end()
                  .justify_center()
                  .cursor_pointer()
                  .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
                  .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |takeover, _, _, cx| takeover.select(revision_id, cx)),
                  )
                  .child(
                    div()
                      .w(width)
                      .h(height)
                      .rounded_sm()
                      .bg(if is_selected { cx.theme().danger } else { color })
                      .when(revision.kind == RevisionKind::Auto && !is_selected, |this| this.opacity(0.45)),
                  )
                  .into_any_element()
              })),
          )
          // ---- action bar ----
          .child(
            h_flex()
              .h(px(38.0))
              .flex_none()
              .items_center()
              .gap_2()
              .px_2()
              .border_t_1()
              .border_color(cx.theme().border)
              .child(
                Button::new("history-restore")
                  .primary()
                  .xsmall()
                  .label("Restore")
                  .disabled(busy || selected.is_none())
                  .tooltip("Bring this moment back as a new edit — the present is pinned first, and the restore is undoable")
                  .on_click(cx.listener(|takeover, _, _, cx| takeover.restore_selected(cx))),
              )
              .child(
                Button::new("history-fork")
                  .xsmall()
                  .label("Fork")
                  .disabled(busy || selected.is_none())
                  .tooltip("Open this moment as a separate document")
                  .on_click(cx.listener(|takeover, _, window, cx| takeover.fork_selected(window, cx))),
              )
              .when(!self.pinning, |this| {
                this.child(
                  Button::new("history-pin")
                    .xsmall()
                    .label("Checkpoint now")
                    .disabled(busy)
                    .tooltip("Pin the present as a named moment")
                    .on_click(cx.listener(|takeover, _, window, cx| {
                      takeover.pinning = true;
                      takeover.pin_input.focus_handle(cx).focus(window);
                      cx.notify();
                    })),
                )
              })
              .when(self.pinning, |this| {
                this
                  .child(div().w(px(200.0)).child(Input::new(&self.pin_input).w_full()))
                  .child(
                    Button::new("history-pin-commit")
                      .primary()
                      .xsmall()
                      .label("Pin")
                      .on_click(cx.listener(|takeover, _, _, cx| takeover.commit_pin(cx))),
                  )
              })
              .child(div().flex_1())
              .child(
                Button::new("history-exit")
                  .xsmall()
                  .label("Exit")
                  .on_click(cx.listener(|takeover, _, _, cx| takeover.exit(cx))),
              ),
          ),
      )
      // D-S5: the takeover ENTERS — one settled beat, not a hard cut. The
      // reduced-motion gate collapses it to the final frame.
      .with_animation(
        gpui::ElementId::Name("history-takeover-entry".into()),
        crate::motion::settle_animation(crate::motion::SEQUENCE_BEAT),
        |this, delta| this.opacity(0.25 + 0.75 * delta),
      )
  }
}

impl HistoryTakeover {
  fn render_ledger_row(&self, revision: &RuntimeRevisionInfo, is_selected: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
    let revision_id = revision.revision_id;
    let renaming = self.renaming == Some(revision_id);
    let color = self.kind_color(revision.kind, cx);
    v_flex()
      .px_1()
      .py_0p5()
      .rounded_sm()
      .when(is_selected, |this| this.bg(cx.theme().list_active))
      .hover(|this| this.bg(cx.theme().list_hover))
      .on_mouse_down(
        MouseButton::Left,
        cx.listener(move |takeover, _, _, cx| takeover.select(revision_id, cx)),
      )
      .child(
        h_flex()
          .gap_1()
          .items_center()
          .child(div().flex_none().w(px(7.0)).h(px(7.0)).rounded_full().bg(color))
          .child(
            div()
              .text_xs()
              .flex_1()
              .min_w_0()
              .overflow_hidden()
              .text_ellipsis()
              .when(revision.kind == RevisionKind::Named, |this| this.font_semibold())
              .child(SharedString::from(revision.title.clone())),
          )
          .child(
            div()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(format_time(revision.created_at_unix_secs)),
          ),
      )
      .when_some(revision.author_display_name.clone(), |this, author| {
        this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(author))
      })
      .when(is_selected && !renaming, |this| {
        this.child(
          Button::new(("history-rename", revision_id as u64))
            .text()
            .compact()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Name this moment"))
            .on_click(cx.listener(move |takeover, _, window, cx| {
              takeover.renaming = Some(revision_id);
              if let Some(revision) = takeover.revision(revision_id) {
                let title = SharedString::from(revision.title.clone());
                takeover
                  .rename_input
                  .update(cx, |input, cx| input.set_value(title, window, cx));
              }
              takeover.rename_input.focus_handle(cx).focus(window);
              cx.notify();
            })),
        )
      })
      .when(renaming, |this| {
        this.child(
          h_flex()
            .gap_1()
            .child(Input::new(&self.rename_input).w_full())
            .child(
              Button::new(("history-rename-save", revision_id as u64))
                .primary()
                .xsmall()
                .label("Save")
                .on_click(cx.listener(|takeover, _, _, cx| takeover.commit_rename(cx))),
            ),
        )
      })
      .into_any_element()
  }
}

fn format_time(unix_secs: i64) -> String {
  chrono::DateTime::from_timestamp(unix_secs, 0)
    .map(|time| time.with_timezone(&chrono::Local).format("%H:%M").to_string())
    .unwrap_or_default()
}

fn format_day(unix_secs: i64) -> String {
  chrono::DateTime::from_timestamp(unix_secs, 0)
    .map(|time| time.with_timezone(&chrono::Local).format("%b %e, %H:%M").to_string())
    .unwrap_or_default()
}
