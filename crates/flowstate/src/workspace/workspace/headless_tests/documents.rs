//! Document panel lifecycle: creation, activation, close, and the sandboxed
//! open-tabs session file.

use gpui::TestAppContext;

use super::support;

#[gpui::test]
fn new_document_activates_panel(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  assert!(h.read(cx, |ws| ws.document_panels.is_empty() && ws.active_document_id.is_none()));
  h.new_document(cx);
  h.read(cx, |ws| {
    assert_eq!(ws.document_panels.len(), 1);
    assert!(ws.active_document_id.is_some());
    assert!(ws.active_editor.is_some());
  });
}

#[gpui::test]
fn second_document_takes_focus_and_activation_switches_back(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let first = h
    .read(cx, |ws| ws.active_document_id)
    .expect("first panel active");
  h.new_document(cx);
  let second = h
    .read(cx, |ws| ws.active_document_id)
    .expect("second panel active");
  assert_ne!(first, second, "new document must become active");

  h.update(cx, |ws, _, cx| {
    let (id, editor) = {
      let panel = ws
        .document_panels
        .iter()
        .find(|panel| panel.read(cx).id() == first)
        .expect("first panel still open");
      (panel.read(cx).id(), panel.read(cx).editor())
    };
    ws.set_active_document(id, editor, cx);
  });
  cx.run_until_parked();
  assert_eq!(h.read(cx, |ws| ws.active_document_id), Some(first));
}

#[gpui::test]
fn close_active_document_removes_panel(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.close_active_document(window, cx));
  cx.run_until_parked();
  // A blank untitled document may count as dirty; if the save prompt fired,
  // decline saving and let the close proceed.
  if cx.has_pending_prompt() {
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
  }
  h.wait_until(cx, "panel closed", |ws| ws.document_panels.is_empty());
  assert_eq!(h.read(cx, |ws| ws.active_document_id), None);
  assert!(h.read(cx, |ws| ws.active_editor.is_none()));
}

#[gpui::test]
fn temporary_session_file_lands_in_sandbox(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  // Persist through the workspace path (debounced spawn), then verify the
  // direct write, so both prove the FLOWSTATE_CONFIG_DIR reroute: a leak to
  // the real std::env::temp_dir() would clobber the user's real tab session.
  h.update(cx, |ws, _, cx| ws.persist_temporary_workspace_session(cx));
  cx.run_until_parked();

  super::super::persist_temporary_workspace_session_to_disk(super::super::TemporaryWorkspaceSession {
    entries: vec![],
    active_index: None,
    ribbon_collapsed: false,
    outline_collapsed: false,
    toolkit_collapsed: false,
    pinned_entry_indices: vec![],
    speech_entry_index: None,
  });
  let sandbox_session = support::sandbox_config_dir().join("flowstate-open-tabs-session.json");
  let path = super::super::temporary_workspace_session_path();
  assert_eq!(path, sandbox_session, "session file must resolve into the sandbox");
}
