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
  let first = h
    .read(cx, |ws| ws.collaboration_dialog.clone())
    .expect("first dialog open");
  h.update(cx, |ws, window, cx| ws.open_join_collaboration_dialog(window, cx));
  cx.run_until_parked();
  let second = h
    .read(cx, |ws| ws.collaboration_dialog.clone())
    .expect("second dialog open");
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
fn comments_panel_opens_before_the_runtime_attaches(cx: &mut TestAppContext) {
  // C-S7: the attach-race net, migrated from the retired comment dialog. The
  // rail panel must open IMMEDIATELY on a fresh document — before its I/O
  // runtime attaches from the OS thread — and show the honest waiting state
  // instead of the old dialog's silent no-op.
  let h = support::open_workspace(cx);
  h.new_document(cx);
  // No runtime wait: open the panel in the race window.
  h.update(cx, |ws, window, cx| ws.open_comments_panel(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.comments_panel.is_some()), "panel opens without a runtime");
  // Once the runtime attaches, the next rail frame hands it to the panel.
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });
  cx.run_until_parked();
  // Reopen path stays idempotent (the old dialog test's reopen leg).
  h.update(cx, |ws, window, cx| ws.open_comments_panel(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.comments_panel.is_some()));
}
