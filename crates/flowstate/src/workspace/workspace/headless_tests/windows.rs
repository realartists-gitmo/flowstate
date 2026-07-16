//! W-S1 — the windows floor: double-open guard, primary-only session
//! ownership, coordinated quit, honest untitled tear-off.

use gpui::TestAppContext;

use super::support;
use crate::workspace::file_management::new_blank_document;
use crate::workspace::open_workspace_window;

#[gpui::test]
fn double_open_same_path_activates_existing_tab(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let path = std::env::temp_dir().join(format!("flowstate-w-s1-{}.db8", uuid::Uuid::new_v4()));
  let panel_id = h.update(cx, |ws, window, cx| {
    ws.create_pending_document_panel(new_blank_document(), Some(path.clone()), None, window, cx)
  });
  cx.run_until_parked();
  assert_eq!(h.read(cx, |ws| ws.document_panels.len()), 1);

  // Opening the SAME path again must activate the existing tab — never mint a
  // second panel/runtime on one file (the docx-clobber defect class).
  h.update(cx, |ws, window, cx| ws.open_document_path(path.clone(), window, cx));
  cx.run_until_parked();
  assert_eq!(
    h.read(cx, |ws| ws.document_panels.len()),
    1,
    "double-open must not create a second panel"
  );
  assert_eq!(
    h.read(cx, |ws| ws.active_document_id),
    Some(panel_id),
    "double-open must activate the existing tab"
  );
}

#[gpui::test]
fn double_open_across_windows_activates_the_other_window(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let path = std::env::temp_dir().join(format!("flowstate-w-s1-{}.db8", uuid::Uuid::new_v4()));
  let panel_id = h.update(cx, |ws, window, cx| {
    ws.create_pending_document_panel(new_blank_document(), Some(path.clone()), None, window, cx)
  });
  cx.run_until_parked();

  let second = cx.update(|cx| open_workspace_window(None, cx));
  cx.run_until_parked();
  let second = second.upgrade().expect("second workspace alive");

  // Open the first window's path FROM the second window: the guard must land
  // in window one, leaving window two panel-free.
  let handles = cx.windows();
  let second_handle = *handles.last().expect("second window handle");
  second_handle
    .update(cx, |_, window, cx| {
      second.update(cx, |ws, cx| ws.open_document_path(path.clone(), window, cx));
    })
    .expect("second window open");
  cx.run_until_parked();

  assert_eq!(cx.update(|cx| second.read(cx).document_panels.len()), 0, "no duplicate panel in window two");
  assert_eq!(
    h.read(cx, |ws| (ws.document_panels.len(), ws.active_document_id)),
    (1, Some(panel_id)),
    "window one keeps the single activated panel"
  );
}

#[gpui::test]
fn second_window_is_ephemeral_not_session_owner(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  assert!(h.read(cx, |ws| ws.session_owner), "first window owns the session");

  let second = cx.update(|cx| open_workspace_window(None, cx));
  cx.run_until_parked();
  let second = second.upgrade().expect("second workspace alive");
  assert!(
    !cx.update(|cx| second.read(cx).session_owner),
    "second window must not own the session"
  );
  // W1-A pre-delivery: a new window opens EMPTY — it must not restore (and so
  // duplicate) the owner's session.
  assert_eq!(cx.update(|cx| second.read(cx).document_panels.len()), 0);
  assert_eq!(cx.update(|cx| second.read(cx).flow_panels.len()), 0);
}

#[gpui::test]
fn untitled_tear_off_refuses_loudly(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let panel_id = h.read(cx, |ws| ws.active_document_id).expect("active panel");
  let windows_before = cx.windows().len();

  h.update(cx, |ws, window, cx| ws.tear_off_document_tab(panel_id, window, cx));
  cx.run_until_parked();

  assert_eq!(h.read(cx, |ws| ws.document_panels.len()), 1, "the untitled panel must stay");
  assert_eq!(cx.windows().len(), windows_before, "no window may open for an untitled tab");
  assert!(
    h.read(cx, |ws| ws.activity_event.is_some()),
    "the refusal must land in the activity zone, not vanish"
  );
}

#[gpui::test]
fn quit_all_windows_closes_every_window(cx: &mut TestAppContext) {
  let _h = support::open_workspace(cx);
  cx.update(|cx| {
    let _ = open_workspace_window(None, cx);
  });
  cx.run_until_parked();
  assert_eq!(cx.windows().len(), 2);

  cx.update(crate::workspace::request_quit_all_windows);
  cx.run_until_parked();
  assert!(cx.windows().is_empty(), "quit must walk every window, not just the focused one");
}
