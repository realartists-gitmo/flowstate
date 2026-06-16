use flowstate_collab::{SessionId, ticket::SessionTicket};
use gpui::{
  AnyElement, App, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
  SharedString, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
  Disableable, WindowExt as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputEvent, InputState},
  scroll::ScrollableElement,
  v_flex,
};
use uuid::Uuid;

use crate::workspace::Workspace;

use super::{DetachReason, SessionPhase, status};

#[path = "share_dialog_view.rs"]
mod share_dialog_view;
use share_dialog_view::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CollabDialogMode {
  Share,
  Join,
}

pub struct CollabShareDialog {
  workspace: WeakEntity<Workspace>,
  panel_id: Option<Uuid>,
  mode: CollabDialogMode,
  join_input: Entity<InputState>,
  joining_session: Option<SessionId>,
  ticket_text: Option<SharedString>,
  ticket_loading: bool,
  ticket_error: Option<SharedString>,
  copy_notice: Option<SharedString>,
  join_error: Option<SharedString>,
  _input_subscription: Subscription,
  _join_subscription: Option<Subscription>,
}

#[hotpath::measure_all]
impl CollabShareDialog {
  pub fn new(
    workspace: WeakEntity<Workspace>,
    panel_id: Option<Uuid>,
    mode: CollabDialogMode,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let join_input = cx.new(|cx| InputState::new(window, cx).placeholder("Paste a Flowstate collaboration invite"));
    let input_subscription = cx.subscribe(&join_input, |dialog, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        dialog.joining_session = None;
        dialog._join_subscription = None;
        dialog.validate_join_ticket(cx);
      }
    });

    let mut dialog = Self {
      workspace,
      panel_id,
      mode,
      join_input,
      joining_session: None,
      ticket_text: None,
      ticket_loading: false,
      ticket_error: None,
      copy_notice: None,
      join_error: None,
      _input_subscription: input_subscription,
      _join_subscription: None,
    };
    if mode == CollabDialogMode::Share {
      dialog.refresh_ticket(cx);
    }
    dialog
  }

  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    if self.mode == CollabDialogMode::Join {
      self.join_input.focus_handle(cx).focus(window);
    }
  }

  fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let _ = self
      .workspace
      .update(cx, |workspace, cx| workspace.close_collaboration_dialog(cx));
    window.close_dialog(cx);
  }

  fn set_mode(&mut self, mode: CollabDialogMode, cx: &mut Context<Self>) {
    if self.mode == mode {
      return;
    }
    self.mode = mode;
    self.join_error = None;
    self.copy_notice = None;
    if mode == CollabDialogMode::Share {
      self.refresh_ticket(cx);
    }
    cx.notify();
  }

  fn start_session(&mut self, cx: &mut Context<Self>) {
    let Some(panel_id) = self.panel_id else {
      self.ticket_error = Some("Open a document before starting a session.".into());
      cx.notify();
      return;
    };

    match self
      .workspace
      .update(cx, |workspace, cx| workspace.start_collaboration_on_document(panel_id, cx))
    {
      Ok(Some(_)) => self.refresh_ticket(cx),
      Ok(None) => {
        self.ticket_error = Some("The active document could not be shared.".into());
        cx.notify();
      },
      Err(error) => {
        self.ticket_error = Some(format!("The workspace is no longer available: {error}").into());
        cx.notify();
      },
    }
  }

  fn refresh_ticket(&mut self, cx: &mut Context<Self>) {
    let Some(panel_id) = self.panel_id else {
      self.ticket_text = None;
      self.ticket_loading = false;
      return;
    };
    let Some(ticket_rx) = crate::collab::request_ticket_for_panel(panel_id, cx) else {
      self.ticket_text = None;
      self.ticket_loading = false;
      self.ticket_error = None;
      return;
    };

    self.ticket_loading = true;
    self.ticket_error = None;
    self.copy_notice = None;
    cx.notify();

    cx.spawn(async move |dialog, cx| {
      let result = ticket_rx.recv().await;
      let encoded = match result {
        Ok(Ok(ticket)) => Ok(ticket.encode_text()),
        Ok(Err(error)) => Err(format!("Creating collaboration invite failed: {error:#}")),
        Err(error) => Err(format!("Creating collaboration invite failed: {error}")),
      };

      let _ = dialog.update(cx, |dialog, cx| {
        dialog.ticket_loading = false;
        match encoded {
          Ok(text) => {
            dialog.ticket_text = Some(text.into());
            dialog.ticket_error = None;
          },
          Err(error) => {
            dialog.ticket_error = Some(error.into());
          },
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn validate_join_ticket(&mut self, cx: &mut Context<Self>) {
    self.join_error = match self.parse_join_ticket(cx) {
      Ok(_) => None,
      Err(error) => Some(error.into()),
    };
    cx.notify();
  }

  fn parse_join_ticket(&self, cx: &App) -> Result<Option<SessionTicket>, String> {
    let text = self.join_input.read(cx).value().trim().to_string();
    if text.is_empty() {
      return Ok(None);
    }
    SessionTicket::decode_text(&text)
      .map(Some)
      .map_err(|error| format!("Invalid invite: {error}"))
  }

  fn join_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.joining_session.is_some() {
      return;
    }
    let ticket = match self.parse_join_ticket(cx) {
      Ok(Some(ticket)) => ticket,
      Ok(None) => {
        self.join_error = Some("Paste an invite before joining.".into());
        cx.notify();
        return;
      },
      Err(error) => {
        self.join_error = Some(error.into());
        cx.notify();
        return;
      },
    };

    match self
      .workspace
      .update(cx, |workspace, cx| workspace.join_collaboration_session(ticket, window, cx))
    {
      Ok(Some(session)) => {
        self.join_error = None;
        self.joining_session = Some(session);
        self.subscribe_join_session(session, cx);
        cx.notify();
      },
      Ok(None) => {
        self.join_error = Some("Joining collaboration session failed.".into());
        cx.notify();
      },
      Err(error) => {
        self.join_error = Some(format!("The workspace is no longer available: {error}").into());
        cx.notify();
      },
    }
  }

  fn subscribe_join_session(&mut self, session_id: SessionId, cx: &mut Context<Self>) {
    self._join_subscription = crate::collab::session_for_id(session_id, cx).map(|session| cx.observe(&session, |_, _, cx| cx.notify()));
  }

  fn confirm_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.panel_id else {
      return;
    };
    let _ = self
      .workspace
      .update(cx, |workspace, cx| workspace.confirm_leave_collaboration_on_panel(panel_id, window, cx));
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    match event.keystroke.key.as_str() {
      "escape" => {
        self.close(window, cx);
        cx.stop_propagation();
      },
      "enter" if self.mode == CollabDialogMode::Join => {
        self.join_session(window, cx);
        cx.stop_propagation();
      },
      _ => {},
    }
  }

  fn render_share_pane(&self, phase: Option<&SessionPhase>, cx: &mut Context<Self>) -> AnyElement {
    if self.panel_id.is_none() {
      return v_flex()
        .gap_3()
        .child(section_title("Share this document", cx))
        .child(helper_text("Open a rich-text document before starting a collaboration session.", cx))
        .into_any_element();
    }

    if phase.is_none_or(|phase| matches!(phase, SessionPhase::Detached(_))) {
      return v_flex()
        .gap_3()
        .child(section_title("Share this document", cx))
        .child(helper_text(
          "Start a session to create one invite ticket. Everyone who joins can edit, invite others, and leave independently.",
          cx,
        ))
        .child(
          Button::new("start-collaboration-session")
            .label("Start session")
            .primary()
            .on_click(cx.listener(|dialog, _, _, cx| dialog.start_session(cx))),
        )
        .when_some(self.ticket_error.clone(), |this, error| this.child(error_text(error, cx)))
        .into_any_element();
    }

    let Some(phase) = phase else {
      return v_flex().into_any_element();
    };
    let roster = self
      .panel_id
      .map_or_else(Vec::new, |panel_id| crate::collab::roster_for_panel(panel_id, cx));
    v_flex()
      .gap_3()
      .child(
        h_flex()
          .items_center()
          .justify_between()
          .child(section_title("Invite people", cx))
          .child(status::status_pill(phase, cx)),
      )
      .child(helper_text(connectivity_text(phase).as_str(), cx))
      .child(roster_list(roster, cx))
      .child(ticket_box(self.ticket_text.clone(), self.ticket_loading, cx))
      .when_some(self.ticket_error.clone(), |this, error| this.child(error_text(error, cx)))
      .when_some(self.copy_notice.clone(), |this, notice| this.child(success_text(notice, cx)))
      .child(
        h_flex().justify_end().child(
          Button::new("leave-collaboration-session")
            .label("Leave session")
            .danger()
            .on_click(cx.listener(|dialog, _, window, cx| dialog.confirm_leave(window, cx))),
        ),
      )
      .into_any_element()
  }

  fn render_join_pane(&self, cx: &mut Context<Self>) -> AnyElement {
    let parsed = self.parse_join_ticket(cx).ok().flatten();
    let progress = self.join_progress(cx);
    v_flex()
      .gap_3()
      .child(section_title("Join a session", cx))
      .child(helper_text(
        "Paste an invite ticket. Joining opens the shared document as a new tab and never merges into your existing files.",
        cx,
      ))
      .child(Input::new(&self.join_input).w_full().cleanable(true))
      .when_some(parsed.as_ref().map(|ticket| ticket.title.clone()), |this, title| {
        this.child(success_text(format!("Invite for: {title}").into(), cx))
      })
      .when_some(progress, |this, (is_error, text)| {
        if is_error {
          this.child(error_text(text, cx))
        } else {
          this.child(progress_text(text, cx))
        }
      })
      .when_some(self.join_error.clone(), |this, error| this.child(error_text(error, cx)))
      .child(
        h_flex().justify_end().child(
          Button::new("join-collaboration-session")
            .label(if self.joining_session.is_some() { "Joining..." } else { "Join" })
            .primary()
            .disabled(self.joining_session.is_some())
            .on_click(cx.listener(|dialog, _, window, cx| dialog.join_session(window, cx))),
        ),
      )
      .into_any_element()
  }

  fn join_progress(&self, cx: &App) -> Option<(bool, SharedString)> {
    let phase = crate::collab::phase_for_session(self.joining_session?, cx)?;
    match phase {
      SessionPhase::Creating => Some((false, "Starting collaboration...".into())),
      SessionPhase::Joining(stage) => Some((false, status::join_stage_label(&stage).into())),
      SessionPhase::Attached(_) => Some((false, "Joined. Opening shared document...".into())),
      SessionPhase::Detached(DetachReason::JoinFailed(error)) => Some((true, error.into())),
      SessionPhase::Detached(_) => None,
    }
  }
}

#[hotpath::measure_all]
impl Focusable for CollabShareDialog {
  fn focus_handle(&self, cx: &App) -> FocusHandle {
    self.join_input.focus_handle(cx)
  }
}

#[hotpath::measure_all]
impl Render for CollabShareDialog {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let phase = self
      .panel_id
      .and_then(|panel_id| crate::collab::phase_for_panel(panel_id, cx));

    v_flex()
      .max_h(px(560.0))
      .gap_4()
      .on_key_down(cx.listener(Self::on_key_down))
      .child(
        h_flex()
          .gap_2()
          .child(tab_button("collab-share-tab", "Share", self.mode == CollabDialogMode::Share).on_click(cx.listener(|dialog, _, _, cx| {
            dialog.set_mode(CollabDialogMode::Share, cx);
          })))
          .child(tab_button("collab-join-tab", "Join", self.mode == CollabDialogMode::Join).on_click(cx.listener(|dialog, _, window, cx| {
            dialog.set_mode(CollabDialogMode::Join, cx);
            dialog.focus(window, cx);
          }))),
      )
      .child(
        div()
          .flex_1()
          .overflow_y_scrollbar()
          .child(match self.mode {
            CollabDialogMode::Share => self.render_share_pane(phase.as_ref(), cx),
            CollabDialogMode::Join => self.render_join_pane(cx),
          }),
      )
  }
}
