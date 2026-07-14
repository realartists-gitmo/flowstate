use gpui::{AnyElement, App, Context, IntoElement, ParentElement, SharedString, div, prelude::*, px, relative, rgb};
use gpui_component::{
  ActiveTheme as _, Disableable, Sizable,
  avatar::Avatar,
  button::{Button, ButtonVariants as _},
  clipboard::Clipboard,
  h_flex,
  scroll::ScrollableElement,
  v_flex,
};

use super::CollabShareDialog;
use crate::collab::{Connectivity, SessionPhase};

pub(super) fn tab_button(id: &'static str, label: &'static str, selected: bool) -> Button {
  let button = Button::new(id).label(label).small();
  if selected { button.primary() } else { button.outline() }
}

pub(super) fn section_title(title: &'static str, cx: &App) -> impl IntoElement {
  div()
    .text_lg()
    .font_weight(gpui::FontWeight::SEMIBOLD)
    .text_color(cx.theme().foreground)
    .child(title)
}

pub(super) fn helper_text(text: &str, cx: &App) -> impl IntoElement {
  div()
    .text_sm()
    .line_height(relative(1.45))
    .text_color(cx.theme().muted_foreground)
    .child(text.to_string())
}

pub(super) fn ticket_box(ticket: Option<SharedString>, loading: bool, cx: &mut Context<CollabShareDialog>) -> AnyElement {
  h_flex()
    .items_start()
    .gap_2()
    .rounded_md()
    .border_1()
    .border_color(cx.theme().border)
    .bg(cx.theme().background)
    .p_3()
    .child(
      div()
        .flex_1()
        .min_h(px(84.0))
        .max_h(px(132.0))
        .overflow_y_scrollbar()
        .text_xs()
        .line_height(relative(1.35))
        .text_color(cx.theme().foreground)
        .child(if loading {
          SharedString::from("Minting a fresh invite...")
        } else {
          ticket
            .clone()
            .unwrap_or_else(|| "Start sharing to mint an invite.".into())
        }),
    )
    .child(copy_invite_button(ticket, loading, cx))
    .into_any_element()
}

pub(super) fn error_text(text: SharedString, cx: &App) -> impl IntoElement {
  div().text_sm().text_color(cx.theme().danger).child(text)
}

pub(super) fn progress_text(text: SharedString, cx: &App) -> impl IntoElement {
  div().text_sm().text_color(cx.theme().info).child(text)
}

pub(super) fn success_text(text: SharedString, cx: &App) -> impl IntoElement {
  div().text_sm().text_color(cx.theme().success).child(text)
}

pub(super) fn roster_list(entries: Vec<crate::collab::SessionRosterEntry>, cx: &App) -> AnyElement {
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
    let name = entry.name;
    let label = if entry.is_self { format!("{name} (you)") } else { name.clone() };
    let color = gpui::Hsla::from(rgb(entry.color_rgb));
    list = list.child(
      h_flex()
        .items_center()
        .gap_2()
        .child(
          Avatar::new()
            .name(name)
            .small()
            .bg(color.opacity(0.14))
            .text_color(color)
            .border_color(color.opacity(0.32)),
        )
        .child(div().size(px(8.0)).rounded_full().bg(color))
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

pub(super) fn connectivity_text(phase: &SessionPhase) -> String {
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

fn copy_invite_button(ticket: Option<SharedString>, loading: bool, cx: &mut Context<CollabShareDialog>) -> AnyElement {
  let Some(ticket) = ticket.filter(|_| !loading) else {
    return Button::new("copy-collaboration-invite-disabled")
      .label("Copy")
      .small()
      .disabled(true)
      .into_any_element();
  };
  let dialog = cx.entity().downgrade();
  h_flex()
    .items_center()
    .gap_1()
    .child(
      Clipboard::new("copy-collaboration-invite")
        .value(ticket)
        .on_copied(move |_, _, cx| {
          let _ = dialog.update(cx, |dialog, cx| {
            dialog.copy_notice = Some("Invite copied to clipboard.".into());
            cx.notify();
          });
        }),
    )
    .child(
      div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Copy"),
    )
    .into_any_element()
}
