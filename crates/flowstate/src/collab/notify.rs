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
    // CO-S1 (Law 2): visible in RELEASE builds too — a rebuilt view is a
    // real event the user may have felt as a flicker or a caret jump.
    SessionNotice::ViewRebuilt => push_info("Collaboration view rebuilt from remote state.", window, cx),
    SessionNotice::IncompatibleVersion(peer) => push_warning(format!("{peer} is using an incompatible collaboration version."), window, cx),
    SessionNotice::HistoryRebaseRequired => {
      std::mem::drop(window.prompt(
        PromptLevel::Warning,
        "Collaborator changes need a reopen",
        Some("A collaborator's offline changes were merged into this document on disk, but the open session can't display them. Close and reopen the document to see their edits — nothing is lost."),
        &[PromptButton::ok("Ok")],
        cx,
      ));
    },
    SessionNotice::AdmissionRefused(identity) => push_warning(
      format!("Someone tried to join via discovery but isn't trusted for this document (identity {identity}). Add them under Settings > Collaboration > Trusted people to admit them."),
      window,
      cx,
    ),
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
