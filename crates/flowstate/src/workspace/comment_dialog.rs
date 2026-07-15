use flowstate_collab::{crdt_runtime::RuntimeCommentThread, doc_io::DocIoHandle};
use gpui::{App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Window, div, prelude::*, px};
use gpui_component::{
  ActiveTheme as _, Disableable, Sizable as _, StyledExt as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputState},
  scroll::ScrollableElement,
  v_flex,
};

pub struct CommentDialog {
  io: DocIoHandle,
  editor: Entity<crate::rich_text_element::RichTextEditor>,
  selection: crate::rich_text_element::EditorSelection,
  author_user_id: u128,
  author_display_name: String,
  create_input: gpui::Entity<InputState>,
  reply_input: gpui::Entity<InputState>,
  edit_input: gpui::Entity<InputState>,
  focus: FocusHandle,
  comments: Vec<RuntimeCommentThread>,
  loading: bool,
  busy: bool,
  error: Option<SharedString>,
  replying_to: Option<u128>,
  editing_message: Option<(u128, u128)>,
}

impl CommentDialog {
  pub fn new(
    io: DocIoHandle,
    editor: Entity<crate::rich_text_element::RichTextEditor>,
    selection: crate::rich_text_element::EditorSelection,
    author_user_id: u128,
    author_display_name: String,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let create_input = cx.new(|cx| InputState::new(window, cx).placeholder("Comment on the selected text"));
    let reply_input = cx.new(|cx| InputState::new(window, cx).placeholder("Write a reply"));
    let edit_input = cx.new(|cx| InputState::new(window, cx).placeholder("Edit comment"));
    let mut dialog = Self {
      io,
      editor,
      selection,
      author_user_id,
      author_display_name,
      create_input,
      reply_input,
      edit_input,
      focus: cx.focus_handle(),
      comments: Vec::new(),
      loading: false,
      busy: false,
      error: None,
      replying_to: None,
      editing_message: None,
    };
    dialog.reload(cx);
    dialog
  }

  fn reload(&mut self, cx: &mut Context<Self>) {
    self.loading = true;
    let io = self.io.clone();
    cx.spawn(async move |dialog, cx| {
      let result = io.comments().await;
      let _ = dialog.update(cx, |dialog, cx| {
        dialog.loading = false;
        match result {
          Ok(comments) => {
            refresh_comment_annotations(&dialog.editor, &comments, cx);
            dialog.comments = comments;
            dialog.error = None;
          },
          Err(error) => dialog.error = Some(format!("Loading comments failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn create_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.busy {
      return;
    }
    let body = self.create_input.read(cx).value().trim().to_string();
    if body.is_empty() {
      self.error = Some("Write a comment first.".into());
      cx.notify();
      return;
    }
    self.busy = true;
    let io = self.io.clone();
    let selection = self.selection.clone();
    let user_id = self.author_user_id;
    let name = self.author_display_name.clone();
    let input = self.create_input.clone();
    let window = window.window_handle();
    cx.spawn(async move |dialog, cx| {
      let result = io.create_comment(Some(selection), body, user_id, name).await;
      let _ = window.update(cx, |_, window, cx| {
        let _ = dialog.update(cx, |dialog, cx| {
          dialog.busy = false;
          match result {
            Ok(_) => {
              input.update(cx, |input, cx| input.set_value("", window, cx));
              dialog.reload(cx);
            },
            Err(error) => dialog.error = Some(format!("Adding comment failed: {error:#}").into()),
          }
          cx.notify();
        });
      });
    })
    .detach();
  }

  fn reply(&mut self, comment_id: u128, window: &mut Window, cx: &mut Context<Self>) {
    if self.busy {
      return;
    }
    let body = self.reply_input.read(cx).value().trim().to_string();
    if body.is_empty() {
      self.error = Some("Write a reply first.".into());
      cx.notify();
      return;
    }
    self.busy = true;
    let io = self.io.clone();
    let user_id = self.author_user_id;
    let name = self.author_display_name.clone();
    let input = self.reply_input.clone();
    let window = window.window_handle();
    cx.spawn(async move |dialog, cx| {
      let result = io.reply_to_comment(comment_id, body, user_id, name).await;
      let _ = window.update(cx, |_, window, cx| {
        let _ = dialog.update(cx, |dialog, cx| {
          dialog.busy = false;
          match result {
            Ok(_) => {
              input.update(cx, |input, cx| input.set_value("", window, cx));
              dialog.replying_to = None;
              dialog.reload(cx);
            },
            Err(error) => dialog.error = Some(format!("Adding reply failed: {error:#}").into()),
          }
          cx.notify();
        });
      });
    })
    .detach();
  }

  fn set_resolved(&mut self, comment_id: u128, resolved: bool, cx: &mut Context<Self>) {
    if self.busy {
      return;
    }
    self.busy = true;
    let io = self.io.clone();
    cx.spawn(async move |dialog, cx| {
      let result = io.set_comment_resolved(comment_id, resolved).await;
      let _ = dialog.update(cx, |dialog, cx| {
        dialog.busy = false;
        match result {
          Ok(()) => dialog.reload(cx),
          Err(error) => dialog.error = Some(format!("Updating comment failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn begin_edit(&mut self, comment_id: u128, message_id: u128, body: String, window: &mut Window, cx: &mut Context<Self>) {
    self
      .edit_input
      .update(cx, |input, cx| input.set_value(body, window, cx));
    self.editing_message = Some((comment_id, message_id));
    self.edit_input.focus_handle(cx).focus(window);
    cx.notify();
  }

  fn save_edit(&mut self, comment_id: u128, message_id: u128, cx: &mut Context<Self>) {
    if self.busy {
      return;
    }
    let body = self.edit_input.read(cx).value().trim().to_string();
    if body.is_empty() {
      self.error = Some("A comment cannot be empty.".into());
      cx.notify();
      return;
    }
    self.busy = true;
    let io = self.io.clone();
    let user_id = self.author_user_id;
    cx.spawn(async move |dialog, cx| {
      let result = io
        .edit_comment_message(comment_id, message_id, body, user_id)
        .await;
      let _ = dialog.update(cx, |dialog, cx| {
        dialog.busy = false;
        match result {
          Ok(()) => {
            dialog.editing_message = None;
            dialog.reload(cx);
          },
          Err(error) => dialog.error = Some(format!("Editing comment failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn delete(&mut self, comment_id: u128, cx: &mut Context<Self>) {
    if self.busy {
      return;
    }
    self.busy = true;
    let io = self.io.clone();
    let user_id = self.author_user_id;
    cx.spawn(async move |dialog, cx| {
      let result = io.delete_comment(comment_id, user_id).await;
      let _ = dialog.update(cx, |dialog, cx| {
        dialog.busy = false;
        match result {
          Ok(()) => dialog.reload(cx),
          Err(error) => dialog.error = Some(format!("Deleting comment failed: {error:#}").into()),
        }
        cx.notify();
      });
    })
    .detach();
  }
}

pub(crate) fn refresh_comment_annotations(
  editor: &Entity<crate::rich_text_element::RichTextEditor>,
  comments: &[RuntimeCommentThread],
  cx: &mut App,
) {
  let selections = comments
    .iter()
    .filter(|thread| !thread.resolved)
    .filter_map(|thread| thread.anchor)
    .map(|(start, end)| crate::rich_text_element::ExternalSelection {
      selection: crate::rich_text_element::EditorSelection::range(start, end),
      color_rgb: 0xd99a20,
    })
    .collect();
  editor.update(cx, |editor, cx| editor.set_annotation_selections(selections, cx));
}

impl Focusable for CommentDialog {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus.clone()
  }
}

impl Render for CommentDialog {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let has_selection = self.selection.anchor != self.selection.head;
    let mut content = v_flex().gap_3();
    if has_selection {
      content = content.child(
        v_flex()
          .gap_2()
          .child(div().font_semibold().child("New comment"))
          .child(Input::new(&self.create_input).w_full())
          .child(
            h_flex().justify_end().child(
              Button::new("comment-create")
                .label(if self.busy { "Adding..." } else { "Comment" })
                .small()
                .primary()
                .disabled(self.busy)
                .on_click(cx.listener(|dialog, _, window, cx| dialog.create_comment(window, cx))),
            ),
          ),
      );
    } else {
      content = content.child(
        div()
          .text_sm()
          .text_color(cx.theme().muted_foreground)
          .child("Select text before creating a comment. Existing threads remain available below."),
      );
    }
    if self.loading {
      content = content.child(div().text_sm().child("Loading comments..."));
    }
    if self.comments.is_empty() && !self.loading {
      content = content.child(
        div()
          .text_sm()
          .text_color(cx.theme().muted_foreground)
          .child("No comments yet."),
      );
    }
    for thread in self.comments.clone() {
      let comment_id = thread.comment_id;
      let resolved = thread.resolved;
      let is_author = thread
        .messages
        .first()
        .is_some_and(|message| message.author_user_id == self.author_user_id);
      let mut card = v_flex()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .p_3()
        .child(
          div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(if thread.anchor.is_some() {
              format!("“{}”", thread.quoted_text)
            } else {
              format!("Original text was removed: “{}”", thread.quoted_text)
            }),
        );
      for message in thread.messages {
        let message_id = message.message_id;
        let message_body = message.body.clone();
        let message_is_author = message.author_user_id == self.author_user_id;
        let editing = self.editing_message == Some((comment_id, message_id));
        let mut message_view = v_flex()
          .gap_1()
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .child(
                div()
                  .text_xs()
                  .font_semibold()
                  .child(message.author_display_name),
              )
              .when(message_is_author && !editing, |row| {
                row.child(
                  Button::new(("comment-edit", message_id as u64))
                    .label("Edit")
                    .xsmall()
                    .outline()
                    .on_click(
                      cx.listener(move |dialog, _, window, cx| dialog.begin_edit(comment_id, message_id, message_body.clone(), window, cx)),
                    ),
                )
              }),
          )
          .when(!editing, |view| {
            view.child(
              h_flex()
                .gap_1()
                .child(div().text_sm().child(message.body))
                .when(message.updated_at_unix_secs > message.created_at_unix_secs, |row| {
                  row.child(
                    div()
                      .text_xs()
                      .text_color(cx.theme().muted_foreground)
                      .child("(edited)"),
                  )
                }),
            )
          });
        if editing {
          message_view = message_view
            .child(Input::new(&self.edit_input).w_full())
            .child(
              h_flex()
                .gap_2()
                .justify_end()
                .child(
                  Button::new(("comment-edit-cancel", message_id as u64))
                    .label("Cancel")
                    .xsmall()
                    .outline()
                    .on_click(cx.listener(|dialog, _, _, cx| {
                      dialog.editing_message = None;
                      cx.notify();
                    })),
                )
                .child(
                  Button::new(("comment-edit-save", message_id as u64))
                    .label("Save")
                    .xsmall()
                    .primary()
                    .disabled(self.busy)
                    .on_click(cx.listener(move |dialog, _, _, cx| dialog.save_edit(comment_id, message_id, cx))),
                ),
            );
        }
        card = card.child(message_view);
      }
      card = card.child(
        h_flex()
          .gap_2()
          .child(
            Button::new(("comment-reply-toggle", comment_id as u64))
              .label("Reply")
              .xsmall()
              .outline()
              .on_click(cx.listener(move |dialog, _, _, cx| {
                dialog.replying_to = Some(comment_id);
                cx.notify();
              })),
          )
          .child(
            Button::new(("comment-resolve", comment_id as u64))
              .label(if resolved { "Reopen" } else { "Resolve" })
              .xsmall()
              .outline()
              .disabled(self.busy)
              .on_click(cx.listener(move |dialog, _, _, cx| dialog.set_resolved(comment_id, !resolved, cx))),
          )
          .when(is_author, |row| {
            row.child(
              Button::new(("comment-delete", comment_id as u64))
                .label("Delete")
                .xsmall()
                .danger()
                .disabled(self.busy)
                .on_click(cx.listener(move |dialog, _, _, cx| dialog.delete(comment_id, cx))),
            )
          }),
      );
      if self.replying_to == Some(comment_id) {
        card = card.child(
          h_flex()
            .gap_2()
            .child(Input::new(&self.reply_input).w_full())
            .child(
              Button::new(("comment-reply", comment_id as u64))
                .label("Send")
                .small()
                .primary()
                .disabled(self.busy)
                .on_click(cx.listener(move |dialog, _, window, cx| dialog.reply(comment_id, window, cx))),
            ),
        );
      }
      content = content.child(card.opacity(if resolved { 0.68 } else { 1.0 }));
    }

    v_flex()
      .max_h(px(600.0))
      .gap_2()
      .when_some(self.error.clone(), |this, error| {
        this.child(div().text_sm().text_color(cx.theme().danger).child(error))
      })
      .child(div().flex_1().overflow_y_scrollbar().child(content))
  }
}
