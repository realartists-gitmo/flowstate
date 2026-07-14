//! Collaboration glue above the session layer: clipboard join failures land
//! as prompts (not panics), leave is a no-op without a session, and the full
//! host-session start/leave round trip works headlessly.

use gpui::{ClipboardItem, TestAppContext};

use super::support;

#[gpui::test]
fn join_from_empty_clipboard_prompts(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let handled = h.update(cx, |ws, window, cx| ws.join_collaboration_from_clipboard(window, cx));
  assert!(handled);
  cx.run_until_parked();
  assert!(cx.has_pending_prompt(), "empty clipboard must surface a Join failed prompt");
  cx.simulate_prompt_answer("Ok");
  cx.run_until_parked();
}

#[gpui::test]
fn join_from_garbage_clipboard_prompts(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  cx.write_to_clipboard(ClipboardItem::new_string("definitely not a flowstate invite".into()));
  let handled = h.update(cx, |ws, window, cx| ws.join_collaboration_from_clipboard(window, cx));
  assert!(handled);
  cx.run_until_parked();
  assert!(cx.has_pending_prompt(), "invalid ticket must surface a Join failed prompt");
  cx.simulate_prompt_answer("Ok");
  cx.run_until_parked();
}

#[gpui::test]
fn leave_collaboration_without_session_is_a_noop(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  assert!(!h.update(cx, |ws, window, cx| ws.confirm_leave_collaboration_on_active_document(window, cx)));
  assert!(!h.update(cx, |ws, _, cx| ws.leave_collaboration_on_active_document(cx)));
}

#[gpui::test]
fn start_and_leave_collaboration_session_round_trip(cx: &mut TestAppContext) {
  // End-to-end through the real CollabManager: starts the local network
  // runtime (local socket bind only; discovery is paused by the sandbox).
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let session = h.update(cx, |ws, _, cx| ws.start_collaboration_on_active_document(cx));
  assert!(session.is_some(), "hosting a session on a fresh document must succeed");
  cx.run_until_parked();
  let left = h.update(cx, |ws, _, cx| ws.leave_collaboration_on_active_document(cx));
  assert!(left, "leaving the session we just started must succeed");
  cx.run_until_parked();
}

/// Build-order step 9 gate: hosting a session on an open FLOW tab registers a
/// live phase + a roster entry for the local user, and leaving keeps the tab
/// fully editable (invariant 5 — the authority never detaches from the tab).
#[gpui::test]
fn start_and_leave_flow_collaboration_session_round_trip(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let panel_id = h.read(cx, |ws| ws.active_document_id).expect("flow panel id");

  let session = h.update(cx, |ws, _, cx| ws.start_collaboration_on_active_document(cx));
  assert!(session.is_some(), "hosting a session on a fresh flow must succeed");
  cx.run_until_parked();

  h.update(cx, |_, _, cx| {
    let phase = crate::collab::phase_for_panel(panel_id, cx);
    assert!(
      phase.is_some_and(|phase| !matches!(phase, crate::collab::SessionPhase::Detached(_))),
      "hosted flow session must register a live phase"
    );
  });
  // The roster fills when the network endpoint comes online (a real OS
  // thread, outside the test dispatcher) — wait for it like `wait_until`.
  let mut rostered = false;
  for _ in 0..500 {
    cx.run_until_parked();
    rostered = h.update(cx, |_, _, cx| {
      crate::collab::roster_for_panel(panel_id, cx)
        .iter()
        .any(|entry| entry.is_self)
    });
    if rostered {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  assert!(rostered, "hosted flow session must roster the local user");

  let left = h.update(cx, |ws, _, cx| ws.leave_collaboration_on_active_document(cx));
  assert!(left, "leaving the flow session we just started must succeed");
  cx.run_until_parked();

  // The tab stays editable after leave: a structural intent still commits
  // through the same gated authority.
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  let sheets_before = h.update(cx, |_, _, cx| flow.read(cx).board().sheets.len());
  h.update(cx, |_, _, cx| flow.update(cx, |editor, cx| editor.create_sheet(cx)));
  cx.run_until_parked();
  h.update(cx, |_, _, cx| {
    assert_eq!(
      flow.read(cx).board().sheets.len(),
      sheets_before + 1,
      "flow tab must stay editable after leaving the session"
    );
  });
}

/// Build-order step 9 gate: the window-close prompt cascade covers live FLOW
/// sessions (`collaboration_close_panels` scans `flow_panels` too).
#[gpui::test]
fn flow_collaboration_close_prompt_cascade(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let session = h.update(cx, |ws, _, cx| ws.start_collaboration_on_active_document(cx));
  assert!(session.is_some(), "hosting a session on a fresh flow must succeed");
  cx.run_until_parked();

  let intercepted = h.update(cx, |ws, window, cx| ws.request_close_window_with_collaboration(window, cx));
  assert!(intercepted, "closing with a live flow session must be intercepted by the leave prompt");
  cx.run_until_parked();
  assert!(cx.has_pending_prompt(), "the leave-and-quit prompt must be pending");
  cx.simulate_prompt_answer("Cancel");
  cx.run_until_parked();

  let left = h.update(cx, |ws, _, cx| ws.leave_collaboration_on_active_document(cx));
  assert!(left, "cancelled close must leave the session intact and leavable");
  cx.run_until_parked();
}
