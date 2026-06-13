use flowstate_collab::ticket::SessionTicket;
use gpui::{
  AnyElement, AnyWindowHandle, App, ClipboardItem, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
  MouseButton, ParentElement, Render, SharedString, Subscription, WeakEntity, Window, div, prelude::*, px, relative, rgb,
};
use gpui_component::{
  ActiveTheme as _, Disableable, IconName, Sizable,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputEvent, InputState},
  scroll::ScrollableElement,
  v_flex,
};
use uuid::Uuid;

use crate::workspace::Workspace;

use super::{Connectivity, SessionPhase, status};

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
  ticket_text: Option<SharedString>,
  ticket_loading: bool,
  ticket_error: Option<SharedString>,
  copy_notice: Option<SharedString>,
  join_error: Option<SharedString>,
  _input_subscription: Subscription,
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
    let _input_subscription = cx.subscribe(&join_input, |dialog, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        dialog.validate_join_ticket(cx);
      }
    });

    let mut dialog = Self {
      workspace,
      panel_id,
      mode,
      join_input,
      ticket_text: None,
      ticket_loading: false,
      ticket_error: None,
      copy_notice: None,
      join_error: None,
      _input_subscription,
    };
    if mode == CollabDialogMode::Share {
      dialog.refresh_ticket(None, cx);
    }
    dialog
  }

  pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
    if self.mode == CollabDialogMode::Join {
      self.join_input.focus_handle(cx).focus(window);
    }
  }

  fn close(&mut self, cx: &mut Context<Self>) {
    let _ = self
      .workspace
      .update(cx, |workspace, cx| workspace.close_collaboration_dialog(cx));
  }

  fn set_mode(&mut self, mode: CollabDialogMode, cx: &mut Context<Self>) {
    if self.mode == mode {
      return;
    }
    self.mode = mode;
    self.join_error = None;
    self.copy_notice = None;
    if mode == CollabDialogMode::Share {
      self.refresh_ticket(None, cx);
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
      Ok(Some(_)) => self.refresh_ticket(None, cx),
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

  fn refresh_ticket(&mut self, copy_to_clipboard: Option<AnyWindowHandle>, cx: &mut Context<Self>) {
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
      let should_copy = copy_to_clipboard.is_some();

      if let Ok(text) = &encoded
        && let Some(window_handle) = copy_to_clipboard
      {
        let text = text.clone();
        let _ = window_handle.update(cx, |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
      }

      let _ = dialog.update(cx, |dialog, cx| {
        dialog.ticket_loading = false;
        match encoded {
          Ok(text) => {
            dialog.ticket_text = Some(text.into());
            dialog.ticket_error = None;
            if should_copy {
              dialog.copy_notice = Some("Fresh invite copied to clipboard.".into());
            }
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

  fn copy_invite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.refresh_ticket(Some(window.window_handle()), cx);
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
      Ok(Some(_)) => self.close(cx),
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
        self.close(cx);
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
        h_flex()
          .gap_2()
          .child(
            Button::new("copy-collaboration-invite")
              .label(if self.ticket_loading { "Minting..." } else { "Copy invite" })
              .primary()
              .disabled(self.ticket_loading)
              .on_click(cx.listener(|dialog, _, window, cx| dialog.copy_invite(window, cx))),
          )
          .child(
            Button::new("refresh-collaboration-invite")
              .label("Refresh invite")
              .outline()
              .disabled(self.ticket_loading)
              .on_click(cx.listener(|dialog, _, _, cx| dialog.refresh_ticket(None, cx))),
          )
          .child(div().flex_1())
          .child(
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
      .when_some(self.join_error.clone(), |this, error| this.child(error_text(error, cx)))
      .child(
        h_flex().justify_end().gap_2().child(
          Button::new("join-collaboration-session")
            .label("Join")
            .primary()
            .on_click(cx.listener(|dialog, _, window, cx| dialog.join_session(window, cx))),
        ),
      )
      .into_any_element()
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

    div()
      .absolute()
      .top_0()
      .right_0()
      .bottom_0()
      .left_0()
      .bg(cx.theme().background.opacity(0.72))
      .flex()
      .items_center()
      .justify_center()
      .occlude()
      .on_key_down(cx.listener(Self::on_key_down))
      .on_mouse_down(
        MouseButton::Left,
        cx.listener(|dialog, _, _, cx| {
          dialog.close(cx);
          cx.stop_propagation();
        }),
      )
      .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
      .child(
        v_flex()
          .w(px(620.0))
          .max_w_full()
          .max_h(px(620.0))
          .overflow_hidden()
          .rounded_lg()
          .border_1()
          .border_color(cx.theme().border)
          .bg(cx.theme().popover)
          .shadow_lg()
          .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
          .child(
            h_flex()
              .h(px(44.0))
              .items_center()
              .justify_between()
              .px_4()
              .border_b_1()
              .border_color(cx.theme().border)
              .child(
                div()
                  .font_weight(gpui::FontWeight::SEMIBOLD)
                  .child("Share / Collaborate"),
              )
              .child(
                Button::new("close-collaboration-dialog")
                  .icon(IconName::Close)
                  .xsmall()
                  .ghost()
                  .tooltip("Close")
                  .on_click(cx.listener(|dialog, _, _, cx| dialog.close(cx))),
              ),
          )
          .child(
            h_flex()
              .gap_2()
              .px_4()
              .pt_4()
              .child(
                tab_button("collab-share-tab", "Share", self.mode == CollabDialogMode::Share).on_click(cx.listener(|dialog, _, _, cx| {
                  dialog.set_mode(CollabDialogMode::Share, cx);
                })),
              )
              .child(
                tab_button("collab-join-tab", "Join", self.mode == CollabDialogMode::Join).on_click(cx.listener(|dialog, _, window, cx| {
                  dialog.set_mode(CollabDialogMode::Join, cx);
                  dialog.focus(window, cx);
                })),
              ),
          )
          .child(
            div()
              .flex_1()
              .overflow_y_scrollbar()
              .p_4()
              .child(match self.mode {
                CollabDialogMode::Share => self.render_share_pane(phase.as_ref(), cx),
                CollabDialogMode::Join => self.render_join_pane(cx),
              }),
          ),
      )
  }
}

fn tab_button(id: &'static str, label: &'static str, selected: bool) -> Button {
  let button = Button::new(id).label(label).small();
  if selected { button.primary() } else { button.outline() }
}

fn section_title(title: &'static str, cx: &App) -> impl IntoElement {
  div()
    .text_lg()
    .font_weight(gpui::FontWeight::SEMIBOLD)
    .text_color(cx.theme().foreground)
    .child(title)
}

fn helper_text(text: &str, cx: &App) -> impl IntoElement {
  div()
    .text_sm()
    .line_height(relative(1.45))
    .text_color(cx.theme().muted_foreground)
    .child(text.to_string())
}

fn ticket_box(ticket: Option<SharedString>, loading: bool, cx: &App) -> impl IntoElement {
  div()
    .w_full()
    .min_h(px(84.0))
    .max_h(px(132.0))
    .overflow_y_scrollbar()
    .rounded_md()
    .border_1()
    .border_color(cx.theme().border)
    .bg(cx.theme().background)
    .p_3()
    .text_xs()
    .line_height(relative(1.35))
    .text_color(cx.theme().foreground)
    .child(if loading {
      SharedString::from("Minting a fresh invite...")
    } else {
      ticket.unwrap_or_else(|| "Start sharing to mint an invite.".into())
    })
}

fn error_text(text: SharedString, cx: &App) -> impl IntoElement {
  div().text_sm().text_color(cx.theme().danger).child(text)
}

fn success_text(text: SharedString, cx: &App) -> impl IntoElement {
  div().text_sm().text_color(cx.theme().success).child(text)
}

fn roster_list(entries: Vec<crate::collab::SessionRosterEntry>, cx: &App) -> AnyElement {
  let mut list = v_flex()
    .gap_2()
    .rounded_md()
    .border_1()
    .border_color(cx.theme().border)
    .bg(cx.theme().background)
    .p_3()
    .child(
      div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Participants"),
    );

  if entries.is_empty() {
    return list
      .child(
        div()
          .text_sm()
          .text_color(cx.theme().muted_foreground)
          .child("Presence is starting..."),
      )
      .into_any_element();
  }

  for entry in entries {
    let label = if entry.is_self { format!("{} (you)", entry.name) } else { entry.name };
    list = list.child(
      h_flex()
        .items_center()
        .gap_2()
        .child(div().size(px(8.0)).rounded_full().bg(rgb(entry.color_rgb)))
        .child(
          div()
            .text_sm()
            .text_color(cx.theme().foreground)
            .child(label),
        ),
    );
  }
  list.into_any_element()
}

fn connectivity_text(phase: &SessionPhase) -> String {
  match phase {
    SessionPhase::Creating => "Starting collaboration...".to_string(),
    SessionPhase::Joining(_) => "Joining collaboration...".to_string(),
    SessionPhase::Attached(attachment) => match &attachment.connectivity {
      Connectivity::Online if attachment.peers_present == 0 => "Only you - share the invite.".to_string(),
      Connectivity::Online => format!("{} people in session.", attachment.peers_present + 1),
      Connectivity::Offline { .. } => "Offline - reconnecting. Changes will sync when connected.".to_string(),
    },
    SessionPhase::Detached(_) => "This document is local.".to_string(),
  }
}
