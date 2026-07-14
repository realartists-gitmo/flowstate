//! Command dispatch through the window-keybinding surface — the same switch
//! real keystrokes reach via the interceptor installed in `Workspace::new`.

use gpui::TestAppContext;

use crate::commands::CommandId;

use super::support;

#[gpui::test]
fn new_document_command_creates_panel(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let handled = h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::NewDocument, window, cx));
  cx.run_until_parked();
  assert!(handled);
  assert_eq!(h.read(cx, |ws| ws.document_panels.len()), 1);
}

#[gpui::test]
fn share_command_opens_dialog_over_document(cx: &mut TestAppContext) {
  // The exact shape of the field crash: a keybinding-dispatched ShareDocument
  // command entering open_collaboration_dialog inside the workspace update.
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let handled = h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::ShareDocument, window, cx));
  cx.run_until_parked();
  assert!(handled);
  assert!(h.read(cx, |ws| ws.collaboration_dialog.is_some()));
}

#[gpui::test]
fn join_command_opens_dialog_without_document(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let handled = h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::JoinSession, window, cx));
  cx.run_until_parked();
  assert!(handled);
  assert!(h.read(cx, |ws| ws.collaboration_dialog.is_some()));
}

#[gpui::test]
fn zoom_commands_require_an_editor(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  assert!(!h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::ZoomIn, window, cx)));
  assert!(!h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::ZoomOut, window, cx)));
  h.new_document(cx);
  assert!(h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::ZoomIn, window, cx)));
  assert!(h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::ZoomOut, window, cx)));
}

#[gpui::test]
fn start_collaboration_command_without_document_is_unhandled(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let handled = h.update(cx, |ws, window, cx| {
    ws.handle_window_keybinding(CommandId::StartCollaboration, window, cx)
  });
  assert!(!handled, "no active document — command must fall through");
}

#[gpui::test]
fn close_document_command_with_no_document_is_safe(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::CloseDocument, window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.document_panels.is_empty()));
}

#[gpui::test]
fn find_in_document_command_survives_with_and_without_document(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::FindInDocument, window, cx));
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.handle_window_keybinding(CommandId::FindInDocument, window, cx));
  cx.run_until_parked();
}
