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

/// Build-order step 10 gate: the join handoff — a snapshot-built flow
/// authority + I/O service become a PATHLESS tab (autosave skips it) that
/// paints the host's board and stays editable through its own gate, with a
/// debounced `.fl0` recovery file that discard removes.
#[gpui::test]
fn flow_join_handoff_pathless_tab_recovery_written_and_discarded(cx: &mut TestAppContext) {
  use flowstate_collab::flow::{FlowDocHandle, FlowIoHandle, FlowRuntime};
  use flowstate_flow::FlowIntent;

  // "Remote host" side: a flow with one sheet, snapshotted for the joiner.
  let format = flowstate_flow::FlowFormat::policy_debate();
  let (host, _host_gate) = FlowDocHandle::new(FlowRuntime::new(&format).expect("host runtime"));
  let sheet_id = uuid::Uuid::new_v4();
  host
    .apply(FlowIntent::CreateSheet {
      sheet_id,
      name: "Shared sheet".into(),
      sheet_type_id: format.sheet_types[0].id,
    })
    .expect("host sheet");
  let snapshot = host.snapshot().expect("host snapshot");
  let expected_board = host.board_projection().expect("host board");

  // Joiner session side (the `finish_join_flow_snapshot` shape): runtime from
  // snapshot → authority + I/O service, handed to the workspace.
  let h = support::open_workspace(cx);
  let runtime = FlowRuntime::from_snapshot(&snapshot).expect("joined runtime");
  let (authority, gate) = FlowDocHandle::new(runtime);
  let io = FlowIoHandle::spawn(gate).expect("joined flow io");
  h.update(cx, |ws, window, cx| {
    ws.add_joined_collaboration_flow_panel(authority, io, "Untitled (shared)".to_string(), window, cx);
  });
  cx.run_until_parked();

  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("joined flow tab active");
  h.update(cx, |_, _, cx| {
    let editor = flow.read(cx);
    assert!(editor.document_path().is_none(), "joined tab must be pathless (autosave skips it)");
    assert_eq!(editor.board(), &expected_board, "joined tab paints the host's board");
  });

  // Recovery: the session sets a recovery path on pathless joined tabs; edits
  // schedule a debounced snapshot write.
  let dir = std::env::temp_dir().join(format!("flowstate-headless-join-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("temp dir");
  let recovery = dir.join("recovery.fl0");
  h.update(cx, |_, _, cx| {
    flow.update(cx, |editor, cx| editor.set_recovery_path(Some(recovery.clone()), cx));
  });
  // The joined tab stays editable through its own authority (invariant 5).
  h.update(cx, |_, _, cx| flow.update(cx, |editor, cx| editor.create_sheet(cx)));
  cx.run_until_parked();
  h.update(cx, |_, _, cx| {
    assert_eq!(flow.read(cx).board().sheets.len(), 2, "joined tab edits commit through the gate");
  });
  cx.executor().advance_clock(std::time::Duration::from_millis(800));
  let recovery_for_wait = recovery.clone();
  h.wait_until(cx, "flow recovery file write", move |_| recovery_for_wait.exists());
  let recovered = flowstate_flow::read_fl0(&recovery).expect("recovery file is a valid .fl0 v2");
  FlowRuntime::from_snapshot(&recovered).expect("recovery snapshot reloads");

  // Autosave skipped: the pathless tab stays dirty (nothing saved it).
  h.update(cx, |_, _, cx| {
    assert!(flow.read(cx).has_unsaved_changes(), "pathless joined tab is never autosaved");
  });

  // Leave/close discards the recovery file.
  h.update(cx, |_, _, cx| flow.update(cx, |editor, _| editor.discard_recovery_file()));
  assert!(!recovery.exists(), "discard removes the recovery file");
  std::fs::remove_dir_all(&dir).ok();
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
