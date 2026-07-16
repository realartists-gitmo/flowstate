//! C-S3: the comments rail panel (Patient 1's decided surface). Lives in the
//! toolkit rail beside Tub; replaces the 620px modal. Laws in force: live
//! surface (observes the editor, so peer edits, local edits, and anchor
//! movement all refresh it), per-thread busy, guarded destructive actions,
//! Enter-submits keyboard law, honest states for every dead end.

use std::collections::HashSet;
use std::future::Future;

use flowstate_collab::{SessionId, crdt_runtime::RuntimeCommentThread, doc_io::DocIoHandle, presence::CommentTyping};
use gpui::{
  App, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
  Subscription, WeakEntity, Window, div, prelude::*, px,
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

/// Debounce for editor-change refreshes: anchors move per keystroke but the
/// panel only needs to settle once per burst.
const REFRESH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// The review-mark color pushed onto the editor for unresolved anchored
/// threads (C-S4). Deliberately uniform (C-S5 puts peer colors on author
/// names and typing indicators instead): a page of mixed-color underlines
/// reads as noise, not review dress.
const COMMENT_MARK_RGB: u32 = 0x00d9_9a20;

pub struct CommentsPanel {
  /// The unread read-state lives workspace-side (C-S5); C-S6 history-jump
  /// opens a workspace view through this too.
  workspace: WeakEntity<Workspace>,
  io: Option<DocIoHandle>,
  editor: Option<Entity<RichTextEditor>>,
  panel_id: Option<Uuid>,
  author_user_id: u128,
  author_display_name: String,
  composer_input: Entity<InputState>,
  reply_input: Entity<InputState>,
  edit_input: Entity<InputState>,
  threads: Vec<RuntimeCommentThread>,
  loading: bool,
  /// Per-thread busy (the floor kill of the audited single global flag).
  busy_threads: HashSet<u128>,
  composer_busy: bool,
  error: Option<SharedString>,
  replying_to: Option<u128>,
  editing_message: Option<(u128, u128)>,
  show_resolved: bool,
  filter_mine: bool,
  refresh_generation: u64,
  refresh_pending: bool,
  /// Review mode is ON exactly while the rail shows this panel (M3 decision:
  /// marks exist only with the panel open). `detach` flips it off and clears
  /// the editor's annotation overlay.
  open: bool,
  /// C-S5: threads that arrived with activity newer than the workspace's
  /// read-state. Dots persist for the viewing session; clicking a card (or
  /// closing the panel) clears its dot.
  unread_threads: HashSet<u128>,
  /// C-S5: the composer state last published to the collab session, so
  /// presence frames only refresh on real transitions.
  published_typing: Option<CommentTyping>,
  _editor_observation: Option<Subscription>,
  _typing_subscriptions: Vec<Subscription>,
}

impl CommentsPanel {
  pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let profile = crate::app_settings::load_local_user_profile();
    let composer_input = cx.new(|cx| InputState::new(window, cx).placeholder("Comment (no selection = general note)"));
    let reply_input = cx.new(|cx| InputState::new(window, cx).placeholder("Write a reply"));
    let edit_input = cx.new(|cx| InputState::new(window, cx).placeholder("Edit comment"));
    // C-S5: composer keystrokes drive the presence typing indicator.
    let _typing_subscriptions = vec![
      cx.subscribe(&composer_input, |panel: &mut Self, _, event: &InputEvent, cx| {
        if let InputEvent::Change = event {
          panel.sync_typing_state(cx);
        }
      }),
      cx.subscribe(&reply_input, |panel: &mut Self, _, event: &InputEvent, cx| {
        if let InputEvent::Change = event {
          panel.sync_typing_state(cx);
        }
      }),
    ];
    Self {
      workspace,
      io: None,
      editor: None,
      panel_id: None,
      author_user_id: profile.user_id,
      author_display_name: profile.display_name,
      composer_input,
      reply_input,
      edit_input,
      threads: Vec::new(),
      loading: false,
      busy_threads: HashSet::new(),
      composer_busy: false,
      error: None,
      replying_to: None,
      editing_message: None,
      show_resolved: false,
      filter_mine: false,
      refresh_generation: 0,
      refresh_pending: false,
      open: false,
      unread_threads: HashSet::new(),
      published_typing: None,
      _editor_observation: None,
      _typing_subscriptions,
    }
  }

  /// C-S5: how many threads currently carry the new-activity dot (headless
  /// tests assert the unread lifecycle through this).
  #[must_use]
  pub fn unread_thread_count(&self) -> usize {
    self.unread_threads.len()
  }

  /// C-S5: publish (or clear) our comment-composer activity on the document's
  /// collab session, if one is live. Solo documents have no session — no-op.
  fn sync_typing_state(&mut self, cx: &mut Context<Self>) {
    let desired = if !self.open {
      None
    } else if self.replying_to.is_some() && !self.reply_input.read(cx).value().trim().is_empty() {
      self.replying_to.map(CommentTyping::InThread)
    } else if !self.composer_input.read(cx).value().trim().is_empty() {
      Some(CommentTyping::NewThread)
    } else {
      None
    };
    if self.published_typing == desired {
      return;
    }
    self.published_typing = desired;
    if let Some(session) = self.panel_id.and_then(|id| crate::collab::session_for_panel(id, cx)) {
      session.update(cx, |session, cx| session.set_comment_typing(desired, cx));
    }
  }

  /// Attach the active document's runtime + editor. Called by the workspace
  /// whenever the active tab changes; a `None` io renders the honest
  /// attach-race state instead of the old silent no-op.
  pub fn set_context(
    &mut self,
    io: Option<DocIoHandle>,
    editor: Option<Entity<RichTextEditor>>,
    panel_id: Option<Uuid>,
    cx: &mut Context<Self>,
  ) {
    let changed = self.panel_id != panel_id || self.io.is_some() != io.is_some();
    let reopened = !self.open;
    self.open = true;
    self.io = io;
    // A document switch orphans the previous editor's marks: clear them before
    // letting go of the handle.
    if changed && self.editor != editor {
      self.clear_editor_marks(cx);
    }
    match &editor {
      // Law 6: the panel is live — every editor notify (local typing, peer
      // imports, anchor motion, the session's comment nudge) schedules a
      // debounced reload. Re-observe only when the editor actually changes;
      // set_context runs every rail frame.
      Some(observed) if self.editor.as_ref() != Some(observed) => {
        self._editor_observation = Some(cx.observe(observed, |panel, _, cx| panel.schedule_refresh(cx)));
      },
      Some(_) => {},
      None => self._editor_observation = None,
    }
    self.editor = editor;
    if changed {
      self.threads.clear();
      self.error = None;
      self.reload(cx);
    } else if reopened {
      // Same document, panel re-shown: the cached threads are already right,
      // but review marks were cleared on detach — reload to re-arm them.
      self.reload(cx);
    }
  }

  /// The rail switched to another tool (or closed): review mode ends. Keeps
  /// the thread cache for an instant reopen but stops observing the editor and
  /// removes the marks (M3: marks exist only while the panel is open).
  pub fn detach(&mut self, cx: &mut Context<Self>) {
    if !self.open {
      return;
    }
    self.open = false;
    self._editor_observation = None;
    self.clear_editor_marks(cx);
    // C-S5: dots are "new since last viewed" — leaving the panel is the view.
    self.unread_threads.clear();
    self.sync_typing_state(cx);
  }

  fn clear_editor_marks(&self, cx: &mut Context<Self>) {
    if let Some(editor) = &self.editor {
      editor.update(cx, |editor, cx| editor.set_annotation_selections(Vec::new(), cx));
    }
  }

  /// Push the review marks for the current thread set: every unresolved
  /// thread with a live anchor gets a dashed underline in the editor.
  fn push_editor_marks(&self, cx: &mut Context<Self>) {
    let Some(editor) = &self.editor else { return };
    if !self.open {
      return;
    }
    let marks: Vec<crate::rich_text_element::ExternalSelection> = self
      .threads
      .iter()
      .filter(|thread| !thread.resolved)
      .filter_map(|thread| thread.anchor)
      .map(|(start, end)| crate::rich_text_element::ExternalSelection {
        selection: crate::rich_text_element::EditorSelection::range(start, end),
        color_rgb: COMMENT_MARK_RGB,
      })
      .collect();
    editor.update(cx, |editor, cx| editor.set_annotation_selections(marks, cx));
  }

  pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.composer_input.focus_handle(cx).focus(window);
  }

  fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
    if self.refresh_pending || self.io.is_none() {
      return;
    }
    self.refresh_pending = true;
    cx.spawn(async move |panel, cx| {
      cx.background_executor().timer(REFRESH_DEBOUNCE).await;
      let _ = panel.update(cx, |panel, cx| {
        panel.refresh_pending = false;
        panel.reload(cx);
      });
    })
    .detach();
  }

  fn reload(&mut self, cx: &mut Context<Self>) {
    let Some(io) = self.io.clone() else {
      cx.notify();
      return;
    };
    self.refresh_generation = self.refresh_generation.wrapping_add(1);
    let generation = self.refresh_generation;
    self.loading = self.threads.is_empty();
    cx.spawn(async move |panel, cx| {
      let result = io.comments().await;
      let _ = panel.update(cx, |panel, cx| {
        if panel.refresh_generation != generation {
          return;
        }
        panel.loading = false;
        match result {
          Ok(threads) => {
            panel.threads = threads;
            panel.error = None;
            // Review marks follow the thread set: every reload re-arms the
            // editor overlay (this is also the solo-editing anchor refresh —
            // no collab session required).
            panel.push_editor_marks(cx);
            // C-S5: collect dots against the workspace read-state, then mark
            // everything on screen as seen (the panel IS the view).
            let stamps: Vec<(u128, i64)> = panel
              .threads
              .iter()
              .map(|thread| (thread.comment_id, thread_latest_activity(thread)))
              .collect();
            let _ = panel.workspace.update(cx, |workspace, cx| {
              for (comment_id, latest) in &stamps {
                if *latest > workspace.comment_seen_stamp(*comment_id) {
                  panel.unread_threads.insert(*comment_id);
                }
              }
              workspace.mark_comment_threads_seen(&stamps, cx);
            });
          },
          Err(error) => panel.error = Some(format!("Loading comments failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn create_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(io) = self.io.clone() else { return };
    let body = self.composer_input.read(cx).value().trim().to_string();
    if body.is_empty() || self.composer_busy {
      return;
    }
    // Live selection (the frozen-snapshot bug's fix): read it NOW, at send.
    let selection = self.editor.as_ref().and_then(|editor| {
      let selection = editor.read(cx).selection().clone();
      (selection.anchor != selection.head).then_some(selection)
    });
    self.composer_busy = true;
    self
      .composer_input
      .update(cx, |input, cx| input.set_value(SharedString::default(), window, cx));
    // C-S5: the composer is empty now — stop broadcasting "writing".
    self.sync_typing_state(cx);
    let (user, name) = (self.author_user_id, self.author_display_name.clone());
    cx.spawn(async move |panel, cx| {
      let result = io.create_comment(selection, body, user, name).await;
      let _ = panel.update(cx, |panel, cx| {
        panel.composer_busy = false;
        match result {
          Ok(_) => panel.reload(cx),
          Err(error) => panel.error = Some(format!("Comment failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn thread_action<F, Fut>(&mut self, comment_id: u128, action: F, cx: &mut Context<Self>)
  where
    F: FnOnce(DocIoHandle) -> Fut + 'static,
    Fut: Future<Output = anyhow::Result<()>> + 'static,
  {
    let Some(io) = self.io.clone() else { return };
    if !self.busy_threads.insert(comment_id) {
      return;
    }
    cx.spawn(async move |panel, cx| {
      let result = action(io).await;
      let _ = panel.update(cx, |panel, cx| {
        panel.busy_threads.remove(&comment_id);
        if let Err(error) = result {
          panel.error = Some(format!("Comment action failed: {error:#}").into());
        }
        panel.reload(cx);
        cx.notify();
      });
    })
    .detach();
  }

  fn jump_to(&self, thread: &RuntimeCommentThread, window: &mut Window, cx: &mut Context<Self>) {
    let Some(editor) = self.editor.clone() else { return };
    let Some((start, end)) = thread.anchor else { return };
    editor.update(cx, |editor, cx| {
      // JF: peek — scroll + flash, the caret stays where the user left it.
      editor.peek_paragraph(start.paragraph, crate::rich_text_element::DEFAULT_JUMP_FLASH_RGB, window, cx);
      editor.flash_range(
        crate::rich_text_element::EditorSelection::range(start, end),
        crate::rich_text_element::DEFAULT_JUMP_FLASH_RGB,
        cx,
      );
    });
  }

  fn on_composer_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    if event.keystroke.key.as_str() == "enter" && !event.keystroke.modifiers.shift {
      self.create_comment(window, cx);
      cx.stop_propagation();
    }
  }
}

impl Focusable for CommentsPanel {
  fn focus_handle(&self, cx: &App) -> FocusHandle {
    self.composer_input.focus_handle(cx)
  }
}

impl Render for CommentsPanel {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let theme_accent = cx.theme().warning;
    let mine = self.author_user_id;
    let filter_mine = self.filter_mine;
    let visible = |thread: &&RuntimeCommentThread| !filter_mine || thread.author_user_id == Some(mine);
    let open: Vec<&RuntimeCommentThread> = self.threads.iter().filter(|t| !t.resolved).filter(visible).collect();
    let resolved: Vec<&RuntimeCommentThread> = self.threads.iter().filter(|t| t.resolved).filter(visible).collect();
    let open_count = open.len();
    let resolved_count = resolved.len();
    let has_selection = self
      .editor
      .as_ref()
      .is_some_and(|editor| {
        let selection = editor.read(cx).selection();
        selection.anchor != selection.head
      });
    // C-S5: remote composer activity from the live session's presence roster.
    let typing_roster: Vec<(String, u32, CommentTyping)> = self
      .panel_id
      .and_then(|id| crate::collab::session_for_panel(id, cx))
      .map(|session| session.read(cx).comment_typing_roster())
      .unwrap_or_default();
    // C-S4 reverse link: caret inside a marked span → that thread lights up.
    // Clicking a mark places the caret, so click-to-focus rides the same path.
    let linked_thread = self.editor.as_ref().and_then(|editor| {
      let caret = editor.read(cx).selection().head;
      self
        .threads
        .iter()
        .find(|thread| {
          !thread.resolved
            && thread
              .anchor
              .is_some_and(|(start, end)| start <= caret && caret <= end)
        })
        .map(|thread| thread.comment_id)
    });

    v_flex()
      .size_full()
      .min_w_0()
      .bg(cx.theme().background)
      .border_l_1()
      .border_color(cx.theme().border)
      // ---- header ----
      .child(
        h_flex()
          .h(px(34.0))
          .flex_none()
          .items_center()
          .justify_between()
          .px_2()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(div().text_sm().font_semibold().child(if self.unread_thread_count() > 0 {
            format!("Comments · {open_count} · {} new", self.unread_thread_count())
          } else {
            format!("Comments · {open_count}")
          }))
          .child(
            Button::new("comments-filter-mine")
              .text()
              .compact()
              .tooltip("Show only threads I started")
              .child(
                div()
                  .text_xs()
                  .text_color(if self.filter_mine { theme_accent } else { cx.theme().muted_foreground })
                  .child("Mine"),
              )
              .on_click(cx.listener(|panel, _, _, cx| {
                panel.filter_mine = !panel.filter_mine;
                cx.notify();
              })),
          ),
      )
      // ---- composer ----
      .child(
        v_flex()
          .flex_none()
          .p_2()
          .gap_1()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(
            div()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(if has_selection {
                "Commenting on the selected text"
              } else {
                "No selection — this will be a general note"
              }),
          )
          .child(
            div()
              .on_key_down(cx.listener(Self::on_composer_key))
              .child(Input::new(&self.composer_input).w_full()),
          )
          .child(
            h_flex().justify_end().child(
              Button::new("comments-send")
                .primary()
                .xsmall()
                .label(if has_selection { "Comment" } else { "Add general note" })
                .disabled(self.composer_busy || self.io.is_none())
                .on_click(cx.listener(|panel, _, window, cx| panel.create_comment(window, cx))),
            ),
          )
          // C-S5: live peers writing a new comment right now.
          .children(typing_roster.iter().filter(|(_, _, typing)| matches!(typing, CommentTyping::NewThread)).map(|(name, color, _)| {
            let (name, color) = (name.clone(), *color);
            h_flex()
              .gap_1()
              .items_center()
              .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(peer_color(color)))
              .child(
                div()
                  .text_xs()
                  .italic()
                  .text_color(cx.theme().muted_foreground)
                  .child(format!("{name} is writing a comment…")),
              )
          })),
      )
      // ---- states ----
      .when(self.io.is_none(), |this| {
        this.child(
          div()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Waiting for the document to finish opening…"),
        )
      })
      .when_some(self.error.clone(), |this, error| {
        this.child(div().px_2().py_1().text_xs().text_color(cx.theme().danger).child(error))
      })
      .when(self.io.is_some() && self.threads.is_empty() && !self.loading, |this| {
        this.child(
          div()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No comments yet. Select text and press Enter above, or add a general note."),
        )
      })
      // ---- threads ----
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .overflow_y_scrollbar()
          .p_2()
          .gap_2()
          .children(open.iter().map(|thread| {
            let typers: Vec<(String, u32)> = typing_roster
              .iter()
              .filter(|(_, _, typing)| *typing == CommentTyping::InThread(thread.comment_id))
              .map(|(name, color, _)| (name.clone(), *color))
              .collect();
            self.render_thread(thread, linked_thread == Some(thread.comment_id), typers, cx)
          }))
          // ---- resolved accordion (the decided archive) ----
          .when(resolved_count > 0, |this| {
            this.child(
              Button::new("comments-resolved-accordion")
                .text()
                .compact()
                .child(
                  div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} Resolved ({resolved_count})", if self.show_resolved { "▾" } else { "▸" })),
                )
                .on_click(cx.listener(|panel, _, _, cx| {
                  panel.show_resolved = !panel.show_resolved;
                  cx.notify();
                })),
            )
          })
          .when(self.show_resolved, |this| {
            this.children(resolved.iter().map(|thread| self.render_thread(thread, false, Vec::new(), cx)))
          }),
      )
  }
}

impl CommentsPanel {
  fn render_thread(
    &self,
    thread: &RuntimeCommentThread,
    linked: bool,
    typers: Vec<(String, u32)>,
    cx: &mut Context<Self>,
  ) -> impl IntoElement + use<> {
    let comment_id = thread.comment_id;
    let busy = self.busy_threads.contains(&comment_id);
    let unread = self.unread_threads.contains(&comment_id);
    let is_thread_author = thread.author_user_id == Some(self.author_user_id);
    let orphaned = !thread.general && thread.anchor.is_none();
    let can_jump = thread.anchor.is_some();
    let quote: SharedString = if thread.general {
      "General".into()
    } else if orphaned {
      format!("Original text was removed: “{}”", thread.quoted_text).into()
    } else {
      format!("“{}”", thread.quoted_text).into()
    };
    let resolved = thread.resolved;

    v_flex()
      .p_2()
      .gap_1()
      .rounded_md()
      .border_1()
      .border_color(if linked { cx.theme().warning } else { cx.theme().border })
      .when(resolved, |this| this.opacity(0.75))
      // C-S5: interacting with the card is "viewing" it — the dot clears.
      .when(unread, |this| {
        this.on_mouse_down(
          gpui::MouseButton::Left,
          cx.listener(move |panel, _, _, cx| {
            if panel.unread_threads.remove(&comment_id) {
              cx.notify();
            }
          }),
        )
      })
      // quote / jump target
      .child(
        h_flex()
          .gap_1()
          .items_center()
          // C-S5: the per-thread new-activity dot.
          .when(unread, |this| {
            this.child(div().flex_none().w(px(7.0)).h(px(7.0)).rounded_full().bg(cx.theme().warning))
          })
          .child(
            Button::new(("comment-jump", comment_id as u64))
            .text()
            .compact()
            .disabled(!can_jump)
            .tooltip(if can_jump { "Jump to the text" } else { "No live anchor" })
            .child(
              div()
                .text_xs()
                .italic()
                .max_w_full()
                .overflow_hidden()
                .text_ellipsis()
                .text_color(if orphaned {
                  cx.theme().muted_foreground
                } else {
                  cx.theme().warning
                })
                .child(quote),
            )
            .on_click(cx.listener(move |panel, _, window, cx| {
              if let Some(thread) = panel.threads.iter().find(|thread| thread.comment_id == comment_id).cloned() {
                panel.jump_to(&thread, window, cx);
              }
            })),
        ),
      )
      // messages
      .children(thread.messages.iter().map(|message| {
        let message_id = message.message_id;
        let is_message_author = message.author_user_id == self.author_user_id;
        let editing = self.editing_message == Some((comment_id, message_id));
        let body: SharedString = if message.deleted {
          "message deleted".into()
        } else {
          message.body.clone().into()
        };
        let edited = !message.deleted && message.updated_at_unix_secs > message.created_at_unix_secs;
        v_flex()
          .gap_0p5()
          .child(
            h_flex()
              .gap_1()
              .items_baseline()
              .child(
                div()
                  .text_xs()
                  .font_semibold()
                  // C-S5: author names carry the palette color their id maps
                  // to — the same accent presence uses, online or off.
                  .text_color(peer_color(SessionId::color_for_user(message.author_user_id)))
                  .child(SharedString::from(message.author_display_name.clone())),
              )
              .when(edited, |this| {
                this.child(div().text_xs().text_color(cx.theme().muted_foreground).child("(edited)"))
              }),
          )
          .when(!editing, |this| {
            this.child(
              div()
                .text_sm()
                .when(message.deleted, |this| this.italic().text_color(cx.theme().muted_foreground))
                .child(body),
            )
          })
          .when(editing, |this| {
            this.child(
              h_flex()
                .gap_1()
                .child(Input::new(&self.edit_input).w_full())
                .child(
                  Button::new(("comment-edit-save", message_id as u64))
                    .xsmall()
                    .primary()
                    .label("Save")
                    .disabled(busy)
                    .on_click(cx.listener(move |panel, _, _, cx| {
                      let body = panel.edit_input.read(cx).value().trim().to_string();
                      if body.is_empty() {
                        return;
                      }
                      panel.editing_message = None;
                      let user = panel.author_user_id;
                      panel.thread_action(
                        comment_id,
                        move |io| async move { io.edit_comment_message(comment_id, message_id, body, user).await },
                        cx,
                      );
                    })),
                ),
            )
          })
          .when(is_message_author && !message.deleted && !editing, |this| {
            this.child(
              h_flex()
                .gap_2()
                .child(
                  Button::new(("comment-edit", message_id as u64))
                    .text()
                    .compact()
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Edit"))
                    .on_click(cx.listener(move |panel, _, window, cx| {
                      panel.editing_message = Some((comment_id, message_id));
                      if let Some(thread) = panel.threads.iter().find(|thread| thread.comment_id == comment_id)
                        && let Some(message) = thread.messages.iter().find(|message| message.message_id == message_id)
                      {
                        let body = SharedString::from(message.body.clone());
                        panel.edit_input.update(cx, |input, cx| input.set_value(body, window, cx));
                      }
                      cx.notify();
                    })),
                )
                .child(
                  Button::new(("comment-msg-delete", message_id as u64))
                    .text()
                    .compact()
                    .child(div().text_xs().text_color(cx.theme().danger).child("Delete"))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                      let user = panel.author_user_id;
                      panel.thread_action(
                        comment_id,
                        move |io| async move { io.delete_comment_message(comment_id, message_id, user).await },
                        cx,
                      );
                    })),
                ),
            )
          })
          .into_any_element()
      }))
      // C-S5: live peers replying in THIS thread.
      .children(typers.into_iter().map(|(name, color)| {
        h_flex()
          .gap_1()
          .items_center()
          .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(peer_color(color)))
          .child(
            div()
              .text_xs()
              .italic()
              .text_color(cx.theme().muted_foreground)
              .child(format!("{name} is replying…")),
          )
      }))
      // reply composer
      .when(self.replying_to == Some(comment_id), |this| {
        this.child(
          h_flex()
            .gap_1()
            .child(Input::new(&self.reply_input).w_full())
            .child(
              Button::new(("comment-reply-send", comment_id as u64))
                .xsmall()
                .primary()
                .label("Send")
                .disabled(busy)
                .on_click(cx.listener(move |panel, _, window, cx| {
                  let body = panel.reply_input.read(cx).value().trim().to_string();
                  if body.is_empty() {
                    return;
                  }
                  panel.replying_to = None;
                  panel
                    .reply_input
                    .update(cx, |input, cx| input.set_value(SharedString::default(), window, cx));
                  // C-S5: reply sent — stop broadcasting "replying".
                  panel.sync_typing_state(cx);
                  let (user, name) = (panel.author_user_id, panel.author_display_name.clone());
                  panel.thread_action(
                    comment_id,
                    move |io| async move { io.reply_to_comment(comment_id, body, user, name).await.map(|_| ()) },
                    cx,
                  );
                })),
            ),
        )
      })
      // actions
      .child(
        h_flex()
          .gap_2()
          .child(
            Button::new(("comment-reply", comment_id as u64))
              .text()
              .compact()
              .disabled(busy)
              .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Reply"))
              .on_click(cx.listener(move |panel, _, _, cx| {
                panel.replying_to = if panel.replying_to == Some(comment_id) { None } else { Some(comment_id) };
                cx.notify();
              })),
          )
          .child(
            Button::new(("comment-resolve", comment_id as u64))
              .text()
              .compact()
              .disabled(busy)
              .child(
                div()
                  .text_xs()
                  .text_color(cx.theme().muted_foreground)
                  .child(if resolved { "Reopen" } else { "Resolve" }),
              )
              .on_click(cx.listener(move |panel, _, _, cx| {
                let resolved = panel
                  .threads
                  .iter()
                  .find(|thread| thread.comment_id == comment_id)
                  .is_some_and(|thread| thread.resolved);
                panel.thread_action(
                  comment_id,
                  move |io| async move { io.set_comment_resolved(comment_id, !resolved).await },
                  cx,
                );
              })),
          )
          .when(is_thread_author, |this| {
            this.child(
              Button::new(("comment-delete", comment_id as u64))
                .text()
                .compact()
                .disabled(busy)
                .child(div().text_xs().text_color(cx.theme().danger).child("Delete thread"))
                .on_click(cx.listener(move |_, _, window, cx| {
                  // Law 7: whole-thread delete is guarded.
                  let answer = window.prompt(
                    gpui::PromptLevel::Warning,
                    "Delete this comment thread?",
                    Some("Every message in the thread is removed for all collaborators."),
                    &[gpui::PromptButton::ok("Delete"), gpui::PromptButton::cancel("Cancel")],
                    cx,
                  );
                  cx.spawn(async move |panel, cx| {
                    if answer.await == Ok(0) {
                      let _ = panel.update(cx, |panel, cx| {
                        let user = panel.author_user_id;
                        panel.thread_action(
                          comment_id,
                          move |io| async move { io.delete_comment(comment_id, user).await },
                          cx,
                        );
                      });
                    }
                  })
                  .detach();
                })),
            )
          }),
      )
      .into_any_element()
  }
}

/// A peer accent at full strength (presence colors are 24-bit RGB).
fn peer_color(color_rgb: u32) -> gpui::Hsla {
  gpui::Hsla::from(gpui::rgb(color_rgb))
}

/// C-S5: a thread's newest activity stamp — mirrors the workspace's badge
/// measure so dots and badge always agree.
fn thread_latest_activity(thread: &RuntimeCommentThread) -> i64 {
  thread
    .messages
    .iter()
    .map(|message| message.updated_at_unix_secs.max(message.created_at_unix_secs))
    .fold(thread.updated_at_unix_secs.max(thread.created_at_unix_secs), i64::max)
}
