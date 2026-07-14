//! Dialog lifecycle under the real workspace wiring. The share-dialog test is
//! the regression net for the field panic where the dialog constructor called
//! `workspace.update` while the Workspace lease was held (GPUI double lease).

use gpui::TestAppContext;

use super::support;

#[gpui::test]
fn share_dialog_opens_over_active_document(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.open_collaboration_dialog(window, cx));
  // Flush the deferred trusted-peer scan — the old double-lease panic site.
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.collaboration_dialog.is_some()));
}

#[gpui::test]
fn share_dialog_opens_with_no_document(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.open_collaboration_dialog(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.collaboration_dialog.is_some()));
}

#[gpui::test]
fn join_dialog_opens(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.open_join_collaboration_dialog(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.collaboration_dialog.is_some()));
}

#[gpui::test]
fn share_dialog_reopen_replaces_previous(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.open_collaboration_dialog(window, cx));
  cx.run_until_parked();
  let first = h.read(cx, |ws| ws.collaboration_dialog.clone()).expect("first dialog open");
  h.update(cx, |ws, window, cx| ws.open_join_collaboration_dialog(window, cx));
  cx.run_until_parked();
  let second = h.read(cx, |ws| ws.collaboration_dialog.clone()).expect("second dialog open");
  assert_ne!(first.entity_id(), second.entity_id(), "reopen must build a fresh dialog");
}

#[gpui::test]
fn close_collaboration_dialog_clears_state(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.open_collaboration_dialog(window, cx));
  cx.run_until_parked();
  h.update(cx, |ws, _, cx| ws.close_collaboration_dialog(cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.collaboration_dialog.is_none()));
}

#[gpui::test]
fn comment_dialog_opens_once_runtime_attaches(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  // The comment dialog needs the panel's I/O runtime, which attaches from an
  // OS thread outside the test dispatcher.
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });
  h.update(cx, |ws, window, cx| ws.open_comment_dialog(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.comment_dialog.is_some()));
  // Reopen path (closes the previous dialog first).
  h.update(cx, |ws, window, cx| ws.open_comment_dialog(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.comment_dialog.is_some()));
}
