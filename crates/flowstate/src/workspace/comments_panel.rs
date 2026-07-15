//! C-S3: the comments rail panel (Patient 1's decided surface). Lives in the
//! toolkit rail beside Tub; replaces the 620px modal. Laws in force: live
//! surface (observes the editor, so peer edits, local edits, and anchor
//! movement all refresh it), per-thread busy, guarded destructive actions,
//! Enter-submits keyboard law, honest states for every dead end.

use std::collections::HashSet;
use std::future::Future;

use flowstate_collab::{crdt_runtime::RuntimeCommentThread, doc_io::DocIoHandle};
use gpui::{
  App, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
  Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Disableable, Sizable as _, StyledExt as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputState},
  scroll::ScrollableElement,
  v_flex,
};
use uuid::Uuid;

use crate::rich_text_element::RichTextEditor;
use crate::workspace::Workspace;

/// Debounce for editor-change refreshes: anchors move per keystroke but the
/// panel only needs to settle once per burst.
const REFRESH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

pub struct CommentsPanel {
  /// Held for C-S5 (unread state persists workspace-side) and C-S6
  /// (history-jump opens a workspace view); unused until those land.
  #[allow(dead_code, reason = "consumed by C-S5/C-S6")]
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
  _editor_observation: Option<Subscription>,
}

impl CommentsPanel {
  pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let profile = crate::app_settings::load_local_user_profile();
    let composer_input = cx.new(|cx| InputState::new(window, cx).placeholder("Comment (no selection = general note)"));
    let reply_input = cx.new(|cx| InputState::new(window, cx).placeholder("Write a reply"));
    let edit_input = cx.new(|cx| InputState::new(window, cx).placeholder("Edit comment"));
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
      _editor_observation: None,
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
    self.io = io;
    self.panel_id = panel_id;
    if let Some(editor) = &editor {
      // Law 6: the panel is live — every editor notify (local typing, peer
      // imports, anchor motion) schedules a debounced reload.
      let observed = editor.clone();
      self._editor_observation = Some(cx.observe(&observed, |panel, _, cx| panel.schedule_refresh(cx)));
    } else {
      self._editor_observation = None;
    }
    self.editor = editor;
    if changed {
      self.threads.clear();
      self.error = None;
      self.reload(cx);
    }
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
          .child(div().text_sm().font_semibold().child(format!("Comments · {open_count}")))
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
          ),
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
          .children(open.iter().map(|thread| self.render_thread(thread, cx)))
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
            this.children(resolved.iter().map(|thread| self.render_thread(thread, cx)))
          }),
      )
  }
}

impl CommentsPanel {
  fn render_thread(&self, thread: &RuntimeCommentThread, cx: &mut Context<Self>) -> impl IntoElement + use<> {
    let comment_id = thread.comment_id;
    let busy = self.busy_threads.contains(&comment_id);
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
      .border_color(cx.theme().border)
      .when(resolved, |this| this.opacity(0.75))
      // quote / jump target
      .child(
        h_flex().child(
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
                .on_click(cx.listener(move |panel, _, _, cx| {
                  let body = panel.reply_input.read(cx).value().trim().to_string();
                  if body.is_empty() {
                    return;
                  }
                  panel.replying_to = None;
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
