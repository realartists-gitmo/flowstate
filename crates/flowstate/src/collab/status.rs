use gpui::{AnyElement, App, IntoElement, div, prelude::*, px, rgb};
use gpui_component::{
  ActiveTheme as _, Sizable as _,
  avatar::{Avatar, AvatarGroup},
  h_flex,
};

use super::{Attachment, Connectivity, JoinStage, SessionPhase};

pub fn participant_group(phase: &SessionPhase, entries: Vec<super::SessionRosterEntry>, cx: &App) -> AnyElement {
  let mut avatars = AvatarGroup::new().xsmall().limit(4).ellipsis();
  for entry in entries {
    let color = gpui::Hsla::from(rgb(entry.color_rgb));
    avatars = avatars.child(
      Avatar::new()
        .name(entry.name)
        .bg(color.opacity(0.14))
        .text_color(color)
        .border_color(color.opacity(0.38)),
    );
  }
  // Law 2: never signal by dot color alone — the phase label rides along
  // ("2 in session", "Offline - will sync", "Fetching 3/7 bytes").
  let (status_label, status_color) = status_label_and_color(phase, cx);
  h_flex()
    .items_center()
    .gap_1()
    .child(avatars)
    .child(div().size(px(6.0)).rounded_full().bg(status_color))
    .child(div().text_size(px(10.0)).text_color(status_color).child(status_label))
    .into_any_element()
}

/// The phase's human-readable label, for tooltips and status surfaces.
#[must_use]
pub fn phase_label(phase: &SessionPhase, cx: &App) -> String {
  status_label_and_color(phase, cx).0
}

pub fn status_pill(phase: &SessionPhase, cx: &App) -> AnyElement {
  let (label, color) = status_label_and_color(phase, cx);
  h_flex()
    .flex_none()
    .items_center()
    .gap_1()
    .px_2()
    .py_0p5()
    .rounded_full()
    .border_1()
    .border_color(color.opacity(0.38))
    .bg(color.opacity(0.10))
    .child(div().size(px(6.0)).rounded_full().bg(color))
    .child(div().text_size(px(10.0)).text_color(color).child(label))
    .into_any_element()
}

pub fn tab_badge(phase: &SessionPhase, cx: &App) -> Option<AnyElement> {
  let (count, color) = match phase {
    SessionPhase::Attached(attachment) => {
      let color = match &attachment.connectivity {
        Connectivity::Online => cx.theme().success,
        Connectivity::Offline { .. } => cx.theme().muted_foreground,
      };
      (Some(attachment.peers_present + 1), color)
    },
    SessionPhase::Creating | SessionPhase::Joining(_) => (None, cx.theme().info),
    SessionPhase::Detached(_) => return None,
  };

  Some(
    h_flex()
      .items_center()
      .gap_0p5()
      .child(div().size(px(6.0)).rounded_full().bg(color))
      .when_some(count, |this, count| {
        this.child(
          div()
            .text_size(px(9.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(color)
            .child(count.to_string()),
        )
      })
      .into_any_element(),
  )
}

fn status_label_and_color(phase: &SessionPhase, cx: &App) -> (String, gpui::Hsla) {
  match phase {
    SessionPhase::Creating => ("Starting share".to_string(), cx.theme().info),
    SessionPhase::Joining(stage) => (join_stage_label(stage), cx.theme().info),
    SessionPhase::Attached(attachment) => attached_label_and_color(attachment, cx),
    SessionPhase::Detached(_) => ("Not shared".to_string(), cx.theme().muted_foreground),
  }
}

pub fn join_stage_label(stage: &JoinStage) -> String {
  match stage {
    JoinStage::Resolving => "Resolving invite".to_string(),
    JoinStage::Subscribing => "Joining session".to_string(),
    JoinStage::FetchingSnapshot { got, total } => match total {
      Some(total) => format!("Fetching {got}/{total} bytes"),
      None => "Fetching document".to_string(),
    },
    JoinStage::Building => "Opening shared doc".to_string(),
  }
}

fn attached_label_and_color(attachment: &Attachment, cx: &App) -> (String, gpui::Hsla) {
  match &attachment.connectivity {
    Connectivity::Online => {
      let participants = attachment.peers_present + 1;
      if attachment.peers_present == 0 {
        ("Only you".to_string(), cx.theme().success)
      } else {
        (format!("{participants} in session"), cx.theme().success)
      }
    },
    Connectivity::Offline { .. } => ("Offline - will sync".to_string(), cx.theme().warning),
  }
}
