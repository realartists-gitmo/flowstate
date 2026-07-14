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

/// Build-order step 8 gate: solo flow edit/save/undo THROUGH the gated
/// authority path — sheet + cell structural intents, cell typing via the
/// per-cell `FlowCellAuthority`, `.fl0` v2 save round trip, drag commit via
/// `MoveCellSubtree`, and whole-doc undo.
#[gpui::test]
fn flow_edit_save_undo_through_the_authority(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");

  // Structural edits: sheet + two root cells + typing into the second.
  h.update(cx, |_, _, cx| {
    flow.update(cx, |editor, cx| {
      editor.create_sheet(cx);
      editor.add_first_argument(cx);
      editor.add_orphan_at_column_top(0, cx);
    });
  });
  cx.run_until_parked();
  let (board, first, second) = h.update(cx, |_, _, cx| {
    let board = flow.read(cx).board().clone();
    let cells: Vec<_> = board.sheets[0].cells.iter().map(|cell| cell.id).collect();
    (board.clone(), cells[1], cells[0])
  });
  assert_eq!(board.sheets.len(), 1);
  assert_eq!(board.sheets[0].cells.len(), 2);

  // Cell typing through the cell authority (the write path an attached
  // RichTextEditor drives).
  h.update(cx, |_, _, cx| {
    flow.update(cx, |editor, cx| {
      let authority = editor.handle().cell_authority(second);
      let projection = editor.handle().open_cell(second).expect("open cell");
      use crate::rich_text_element::{InsertTextIntent, LocalIntent, LocalWriteAuthority, TextAnchor};
      let authority: std::sync::Arc<dyn LocalWriteAuthority> = authority;
      authority
        .apply(LocalIntent::InsertText(InsertTextIntent {
          at: TextAnchor::new(projection.ids.paragraph_ids[0], 0),
          text: "typed through the gate".into(),
          style_override: None,
        }))
        .expect("cell typing commits");
      editor.sync_board_from_handle(cx);
    });
  });
  cx.run_until_parked();
  h.update(cx, |_, _, cx| {
    let board = flow.read(cx).board().clone();
    let summary = &board.cell(second).expect("cell").1.summary;
    assert_eq!(summary.summary_text.as_ref(), "typed through the gate");
  });

  // Drag commit via MoveCellSubtree: `second` after `first`'s subtree.
  h.update(cx, |_, _, cx| {
    flow.update(cx, |editor, cx| {
      let sheet = editor.board().sheets[0].id;
      editor.activate_sheet(sheet, cx);
      let moved = editor.handle().apply(flowstate_flow::FlowIntent::MoveCellSubtree {
        sheet_id: sheet,
        cell_id: second,
        drop: flowstate_flow::FlowDropIntent::AfterSibling(first),
      });
      assert!(moved.is_ok_and(|outcome| outcome.changed), "drag commit through the gate");
      editor.sync_board_from_handle(cx);
    });
  });
  cx.run_until_parked();
  h.update(cx, |_, _, cx| {
    let order: Vec<_> = flow.read(cx).board().sheets[0].cells.iter().map(|cell| cell.id).collect();
    assert_eq!(order, vec![first, second]);
  });

  // Undo restores the pre-move order (whole-doc undo stack).
  h.update(cx, |_, _, cx| {
    flow.update(cx, |editor, cx| editor.undo(cx));
  });
  cx.run_until_parked();
  h.update(cx, |_, _, cx| {
    let order: Vec<_> = flow.read(cx).board().sheets[0].cells.iter().map(|cell| cell.id).collect();
    assert_eq!(order, vec![second, first], "undo reversed the move");
  });

  // Save writes a v2 .fl0 that round-trips to the identical board.
  let dir = std::env::temp_dir().join(format!("flowstate-headless-flow-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("temp dir");
  let path = dir.join("gate.fl0");
  h.update(cx, |_, _, cx| {
    flow.update(cx, |editor, cx| editor.save_as(path.clone(), cx)).detach();
  });
  let flow_for_wait = flow.clone();
  h.wait_until(cx, "flow save to complete", move |_| path.exists());
  cx.run_until_parked();
  let snapshot = flowstate_flow::read_fl0(&dir.join("gate.fl0")).expect("read .fl0 v2");
  let reloaded = flowstate_collab::flow::FlowRuntime::from_snapshot(&snapshot).expect("reload");
  h.update(cx, |_, _, cx| {
    assert_eq!(reloaded.board_ref(), flow_for_wait.read(cx).board(), "saved board round-trips");
  });
  std::fs::remove_dir_all(&dir).ok();
}
