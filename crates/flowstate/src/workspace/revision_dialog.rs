use flowstate_collab::crdt_runtime::RuntimeRevisionInfo;
use gpui::{
  App, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, WeakEntity, Window, div,
  prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, WindowExt as _,
  StyledExt as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  scroll::ScrollableElement,
  v_flex,
};
use uuid::Uuid;

use super::Workspace;

pub struct RevisionDialog {
  workspace: WeakEntity<Workspace>,
  panel_id: Uuid,
  focus: FocusHandle,
  loading: bool,
  error: Option<SharedString>,
  revisions: Vec<RuntimeRevisionInfo>,
}

impl RevisionDialog {
  pub fn new(
    workspace: WeakEntity<Workspace>,
    panel_id: Uuid,
    receiver: Option<async_channel::Receiver<anyhow::Result<Vec<RuntimeRevisionInfo>>>>,
    cx: &mut Context<Self>,
  ) -> Self {
    let loading = receiver.is_some();
    if let Some(receiver) = receiver {
      cx.spawn(async move |dialog, cx| {
        let result = receiver.recv().await;
        let _ = dialog.update(cx, |dialog, cx| {
          dialog.loading = false;
          match result {
            Ok(Ok(revisions)) => dialog.revisions = revisions,
            Ok(Err(error)) => dialog.error = Some(error.to_string().into()),
            Err(error) => dialog.error = Some(format!("Revision request closed: {error}").into()),
          }
          cx.notify();
        });
      })
      .detach();
    }
    Self {
      workspace,
      panel_id,
      focus: cx.focus_handle(),
      loading,
      error: (!loading).then(|| "This document has no revision runtime.".into()),
      revisions: Vec::new(),
    }
  }

  pub fn focus(&self, window: &mut Window, _: &mut App) {
    self.focus.focus(window);
  }
}

impl Focusable for RevisionDialog {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus.clone()
  }
}

impl Render for RevisionDialog {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let content = if self.loading {
      div()
        .py_6()
        .text_color(cx.theme().muted_foreground)
        .child("Loading revisions...")
        .into_any_element()
    } else if let Some(error) = self.error.clone() {
      div()
        .py_6()
        .text_color(cx.theme().danger)
        .child(error)
        .into_any_element()
    } else if self.revisions.is_empty() {
      div()
        .py_6()
        .text_color(cx.theme().muted_foreground)
        .child("No saved revisions yet.")
        .into_any_element()
    } else {
      v_flex()
        .gap_1()
        .children(self.revisions.iter().map(|revision| {
          let workspace = self.workspace.clone();
          let panel_id = self.panel_id;
          let revision_id = revision.revision_id;
          let title: SharedString = revision.title.clone().into();
          let summary: SharedString = revision.summary.clone().into();
          Button::new(("open-revision", revision_id as u64))
            .ghost()
            .w_full()
            .child(
              h_flex()
                .w_full()
                .justify_between()
                .gap_3()
                .child(
                  v_flex()
                    .min_w_0()
                    .items_start()
                    .child(div().font_semibold().text_ellipsis().child(title))
                    .child(
                      div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .text_ellipsis()
                        .child(summary),
                    ),
                )
                .child(
                  div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{:08x}", revision_id as u64)),
                ),
            )
            .on_click(move |_, window, cx| {
              let opened = workspace
                .update(cx, |workspace, cx| workspace.open_document_revision(panel_id, revision_id, window, cx))
                .unwrap_or(false);
              if opened {
                window.close_dialog(cx);
              }
            })
        }))
        .into_any_element()
    };

    v_flex()
      .max_h(px(520.0))
      .min_h(px(160.0))
      .child(
        div()
          .flex_1()
          .overflow_y_scrollbar()
          .child(content),
      )
  }
}
