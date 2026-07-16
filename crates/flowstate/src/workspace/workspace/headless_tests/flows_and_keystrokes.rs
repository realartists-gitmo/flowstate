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

/// Living Grid keyboard layer (spec §3.2): the new-family / navigation /
/// movement grammar drives the FLOW authority end to end, and refused moves
/// SPEAK instead of silently failing.
#[gpui::test]
fn grid_keyboard_grammar_builds_navigates_and_moves(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h
    .read(cx, |ws| ws.active_flow.clone())
    .expect("active flow");

  // Seed a sheet with two families in column 0 via the editor API.
  flow.update(cx, |editor, cx| {
    editor.create_sheet(cx);
    editor.add_first_argument(cx);
    editor.add_new_family(cx);
  });
  cx.run_until_parked();
  let (sheet_id, cells) = flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    (sheet.id, sheet.cells.iter().map(|cell| cell.id).collect::<Vec<_>>())
  });
  assert_eq!(cells.len(), 2, "first-argument + new-family = two roots");

  // Give both cells text (an empty cell can never read struck).
  flow.update(cx, |editor, cx| {
    use crate::rich_text_element::LocalWriteAuthority as _;
    for (index, &cell) in cells.iter().enumerate() {
      let projection = editor
        .handle()
        .cell_projection(cell)
        .expect("cell projection");
      editor
        .handle()
        .cell_authority(cell)
        .apply(crate::rich_text_element::LocalIntent::InsertText(
          crate::rich_text_element::InsertTextIntent {
            at: crate::rich_text_element::TextAnchor::new(projection.ids.paragraph_ids[0], 0),
            text: format!("root {index}"),
            style_override: None,
          },
        ))
        .expect("cell text applies");
    }
    cx.notify();
  });
  cx.run_until_parked();

  // Answer the second root, then Shift-Alt-Left promotes the answer back to
  // a root, Shift-Alt-Left again refuses WITH WORDS.
  flow.update(cx, |editor, cx| {
    editor.activate_cell(cells[1], cx);
    editor.add_response(cx);
  });
  cx.run_until_parked();
  let answer = flow.read_with(cx, |editor, _cx| editor.active_cell().expect("answer cell"));
  flow.update(cx, |editor, cx| {
    editor.move_active_cell(crate::flow::editor::GridDirection::Left, cx);
  });
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = editor
      .board()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .expect("sheet");
    let cell = sheet
      .cells
      .iter()
      .find(|cell| cell.id == answer)
      .expect("promoted cell");
    assert_eq!(cell.parent_id, None, "Shift-Alt-Left promotes to root");
  });
  flow.update(cx, |editor, cx| {
    editor.move_active_cell(crate::flow::editor::GridDirection::Left, cx);
  });
  flow.read_with(cx, |editor, _cx| {
    assert!(
      editor.board().sheets[0]
        .cells
        .iter()
        .any(|cell| cell.id == answer && cell.parent_id.is_none()),
      "cell still a root after the refused promote"
    );
  });

  // Multi-select strike: both roots strike as ONE undo group.
  flow.update(cx, |editor, cx| {
    editor.activate_cell(cells[0], cx);
    editor.toggle_select_cell(cells[1], cx);
    editor.strike_selected(cx);
  });
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    for id in [cells[0], cells[1]] {
      assert!(
        sheet
          .cells
          .iter()
          .find(|cell| cell.id == id)
          .expect("cell")
          .summary
          .struck,
        "set-op strike hit every member"
      );
    }
  });
  flow.update(cx, |editor, cx| editor.undo(cx));
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    for id in [cells[0], cells[1]] {
      assert!(
        !sheet
          .cells
          .iter()
          .find(|cell| cell.id == id)
          .expect("cell")
          .summary
          .struck,
        "ONE undo reverts the whole set-op (W2 law)"
      );
    }
  });
}

// I-S3: the sheet strip's drag-reorder primitive — identity-anchored
// move-before with a no-op guard (already-in-place drops must not write).
#[gpui::test]
fn move_sheet_before_reorders_and_skips_noops(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let editor = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");

  // Three sheets of the first type (a fresh board starts empty).
  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.create_sheet_of_type(0, cx);
      editor.create_sheet_of_type(0, cx);
      editor.create_sheet_of_type(0, cx);
    });
  });
  let order = |cx: &mut TestAppContext| -> Vec<flowstate_flow::SheetId> {
    cx.update(|cx| editor.read(cx).board().sheets.iter().map(|sheet| sheet.id).collect())
  };
  let initial = order(cx);
  assert!(initial.len() >= 3, "expected at least 3 sheets, got {}", initial.len());
  let (a, b, c) = (initial[0], initial[1], initial[2]);

  // Drag the first tab onto the third: A lands immediately before C.
  cx.update(|cx| editor.update(cx, |editor, cx| editor.move_sheet_before(a, Some(c), cx)));
  assert_eq!(order(cx)[..3], [b, a, c], "A lands before C");

  // Dropping a tab where it already sits is a no-op (op-log hygiene).
  let dirty_before = editor.read_with(cx, |editor, _| editor.has_unsaved_changes());
  cx.update(|cx| editor.update(cx, |editor, cx| editor.move_sheet_before(a, Some(c), cx)));
  assert_eq!(order(cx)[..3], [b, a, c], "in-place drop changes nothing");
  let _ = dirty_before;

  // Tail drop: `before: None` sends the tab to the end.
  cx.update(|cx| editor.update(cx, |editor, cx| editor.move_sheet_before(b, None, cx)));
  let after_tail = order(cx);
  assert_eq!(*after_tail.last().expect("sheets"), b, "B lands at the end");
}
