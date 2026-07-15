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

/// Flow architecture S9 gate: a FLOW tab hosts a live session through the
/// same manager path (phase registered, ticket kind = Flow), and leaving
/// keeps the tab editable through its untouched authority (invariant 5).
#[gpui::test]
fn start_and_leave_flow_collaboration_session_round_trip(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let session = h.update(cx, |ws, _, cx| ws.start_collaboration_on_active_document(cx));
  assert!(session.is_some(), "hosting a session on a fresh flow must succeed");
  cx.run_until_parked();

  let panel_id = h
    .read(cx, |ws| ws.active_document_id)
    .expect("flow panel id");
  let phase = h.update(cx, |_, _, cx| crate::collab::phase_for_panel(panel_id, cx));
  assert!(phase.is_some(), "flow session must register a phase");
  let kind = h.update(cx, |_, _, cx| crate::collab::session_kind_for_panel(panel_id, cx));
  assert_eq!(
    kind,
    Some(flowstate_collab::DocumentKind::Flow),
    "the session carries the FLOW kind for ticket minting"
  );

  // Editing works while attached...
  let flow = h
    .read(cx, |ws| ws.active_flow.clone())
    .expect("active flow");
  flow.update(cx, |editor, cx| {
    editor.create_sheet(cx);
    editor.add_first_argument(cx);
  });
  cx.run_until_parked();

  let left = h.update(cx, |ws, _, cx| ws.leave_collaboration_on_active_document(cx));
  assert!(left, "leaving the flow session must succeed");
  cx.run_until_parked();

  // ...and the tab stays editable after leaving (invariant 5).
  flow.update(cx, |editor, cx| editor.add_new_family(cx));
  flow.read_with(cx, |editor, _cx| {
    assert_eq!(
      editor.board().sheets[0].cells.len(),
      2,
      "the flow tab keeps editing through its untouched authority after leave"
    );
  });
}

/// Flow architecture S10 gate: the join handoff shape — a pathless flow panel
/// wired from a PRE-BUILT runtime (the parked join authority), autosave
/// skipped, recovery file written on edit and discarded on demand.
#[gpui::test]
fn joined_flow_panel_attachment_and_recovery_round_trip(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);

  // The "joined" runtime the session would park: a board with one sheet.
  let runtime = flowstate_collab::flow::FlowRuntime::new_empty();
  let (handle, gate) = flowstate_collab::flow::FlowDocHandle::new(runtime);
  let io = flowstate_collab::flow::FlowIoHandle::spawn(gate).expect("flow io");
  let handle = std::sync::Arc::new(handle);

  let panel = h.update(cx, |ws, window, cx| {
    ws.create_flow_panel_titled(
      super::super::FlowRuntimeSource::Attachment {
        handle: std::sync::Arc::clone(&handle),
        io,
      },
      None,
      Some("Shared board (shared)".into()),
      window,
      cx,
    )
  });
  cx.run_until_parked();
  let editor = h
    .read(cx, |ws| ws.active_flow.clone())
    .expect("joined flow active");
  assert_eq!(panel.read_with(cx, |panel, _| panel.title_text().to_string()), "Shared board (shared)");
  editor.read_with(cx, |editor, _cx| {
    assert!(editor.document_path().is_none(), "joined flow tabs are pathless (autosave skips them)");
  });

  // Recovery: install a target, edit, wait for the debounced write.
  let recovery = support::sandbox_config_dir().join("joined-flow-recovery.fl0");
  editor.update(cx, |editor, cx| {
    editor.set_recovery_path(Some(recovery.clone()), cx);
    editor.create_sheet(cx);
    editor.add_first_argument(cx);
  });
  cx.executor()
    .advance_clock(std::time::Duration::from_secs(3));
  cx.run_until_parked();
  // The encode crosses the real flow-IO OS thread; give its reply time to
  // wake the test executor.
  for _ in 0..100 {
    if recovery.exists() {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
    cx.run_until_parked();
  }
  assert!(recovery.exists(), "debounced .fl0 recovery file must be written for a pathless flow tab");
  let bytes = std::fs::read(&recovery).expect("recovery bytes");
  assert_eq!(&bytes[..8], b"FLOWFL0\0", "recovery file carries the framed .fl0 magic");

  editor.update(cx, |editor, _| editor.discard_recovery_file());
  assert!(!recovery.exists(), "discard_recovery_file removes the file");
}
