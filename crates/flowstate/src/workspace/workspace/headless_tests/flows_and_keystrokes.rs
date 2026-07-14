//! Flow panel lifecycle plus true keystroke-level dispatch through the
//! window interceptor installed in `Workspace::new` (one level above the
//! `handle_window_keybinding` unit the other suites exercise).

use gpui::TestAppContext;

use super::support;

#[gpui::test]
fn new_flow_creates_and_activates_flow_panel(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  h.read(cx, |ws| {
    assert_eq!(ws.flow_panels.len(), 1);
    assert!(ws.active_flow.is_some());
    assert!(ws.active_document_id.is_some(), "flow panel takes a panel id");
  });
}

#[gpui::test]
fn flow_and_document_panels_coexist(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  h.read(cx, |ws| {
    assert_eq!(ws.document_panels.len(), 1);
    assert_eq!(ws.flow_panels.len(), 1);
    assert!(ws.active_flow.is_some(), "flow was opened last and must be active");
  });
}

#[gpui::test]
fn ctrl_tab_keystroke_cycles_tabs_through_the_interceptor(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.new_document(cx);
  let before = h.read(cx, |ws| ws.active_document_id);
  cx.simulate_keystrokes(h.window, "ctrl-tab");
  cx.run_until_parked();
  let after = h.read(cx, |ws| ws.active_document_id);
  assert_ne!(before, after, "ctrl-tab must reach the workspace keybinding interceptor");
}

#[gpui::test]
fn find_keystroke_is_absorbed_without_panic(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  cx.simulate_keystrokes(h.window, "ctrl-f");
  cx.run_until_parked();
}
