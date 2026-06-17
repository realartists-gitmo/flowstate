use gpui::{App, PromptButton, PromptLevel, Window};
use gpui_component::{WindowExt as _, notification::Notification};

use super::SessionNotice;

pub fn show_session_notice(notice: &SessionNotice, window: &mut Window, cx: &mut App) {
  match notice {
    SessionNotice::PeerJoined(name) => push_info(collaborator_message(name, "joined"), window, cx),
    SessionNotice::PeerLeft(name) => push_info(collaborator_message(name, "left"), window, cx),
    SessionNotice::LeftSession => push_info("Left session - this copy is now local.", window, cx),
    SessionNotice::Disconnected(detail) => {
      std::mem::drop(window.prompt(
        PromptLevel::Critical,
        "Collaboration disconnected",
        Some(detail.as_str()),
        &[PromptButton::ok("Ok")],
        cx,
      ));
    },
    SessionNotice::ViewRebuilt => {
      #[cfg(debug_assertions)]
      push_info("Collaboration view rebuilt from remote state.", window, cx);
    },
    SessionNotice::IncompatibleVersion(peer) => push_warning(format!("{peer} is using an incompatible collaboration version."), window, cx),
  }
}

fn push_info(message: impl Into<gpui::SharedString>, window: &mut Window, cx: &mut App) {
  window.push_notification(Notification::info(message).title("Collaboration"), cx);
}

fn push_warning(message: impl Into<gpui::SharedString>, window: &mut Window, cx: &mut App) {
  window.push_notification(Notification::warning(message).title("Collaboration"), cx);
}

fn collaborator_message(name: &str, action: &str) -> String {
  if name.trim().is_empty() {
    format!("A collaborator {action}")
  } else {
    format!("{name} {action}")
  }
}
