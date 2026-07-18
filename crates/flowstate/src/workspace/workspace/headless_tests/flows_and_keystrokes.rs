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

/// The grid keyboard grammar (excel flow spec D5): row/cell creation, the
/// answer-to-the-right verb, slot moves through the `SetCellAddress` law, and
/// refused moves SPEAK instead of silently failing.
#[gpui::test]
fn grid_keyboard_grammar_builds_navigates_and_moves(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h
    .read(cx, |ws| ws.active_flow.clone())
    .expect("active flow");

  // Seed a sheet with two cells in column 0 (two rows).
  flow.update(cx, |editor, cx| {
    editor.create_sheet(cx);
    editor.add_first_argument(cx);
    editor.add_new_family(cx);
  });
  cx.run_until_parked();
  let (sheet_id, cells) = flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    (sheet.id, sheet.cells().map(|cell| cell.id).collect::<Vec<_>>())
  });
  assert_eq!(cells.len(), 2, "first-argument + new-family = two cells in column 0");
  flow.read_with(cx, |editor, _cx| {
    assert_eq!(editor.board().sheets[0].rows.len(), 2, "each landed in its own row");
  });

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
            text: format!("arg {index}"),
            style_override: None,
          },
        ))
        .expect("cell text applies");
    }
    cx.notify();
  });
  cx.run_until_parked();

  // Answer the second cell: the response lands one column RIGHT, same row.
  flow.update(cx, |editor, cx| {
    editor.activate_cell(cells[1], cx);
    editor.add_response(cx);
  });
  cx.run_until_parked();
  let answer = flow.read_with(cx, |editor, _cx| editor.active_cell().expect("answer cell"));
  flow.read_with(cx, |editor, _cx| {
    let sheet = editor
      .board()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .expect("sheet");
    let (arg_row, arg_col) = sheet.cell_position(cells[1]).expect("argument placed");
    let (answer_row, answer_col) = sheet.cell_position(answer).expect("answer placed");
    assert_eq!(answer_row, arg_row, "the answer shares its argument's row");
    assert_eq!(answer_col, arg_col + 1, "the answer sits one column right");
  });

  // Moving the answer LEFT onto the argument's slot SWAPS them (lossless):
  // the two cells exchange addresses in one atomic intent.
  let arg_slot = flow.read_with(cx, |editor, _cx| {
    editor.board().sheets[0].cell_position(cells[1]).expect("argument placed")
  });
  flow.update(cx, |editor, cx| {
    editor.activate_cell(answer, cx);
    editor.move_active_cell(crate::flow::editor::GridDirection::Left, cx);
  });
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert_eq!(sheet.cell_position(answer), Some(arg_slot), "the answer took the argument's slot");
    assert_eq!(
      sheet.cell_position(cells[1]),
      Some((arg_slot.0, arg_slot.1 + 1)),
      "the argument took the answer's vacated slot — nothing was destroyed"
    );
  });

  // Multi-select strike: both column-0 cells strike as ONE undo group.
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
        sheet.find_cell(id).expect("cell").summary.struck,
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
        !sheet.find_cell(id).expect("cell").summary.struck,
        "ONE undo reverts the whole set-op (W2 law)"
      );
    }
  });
}

/// Focusing a cell must NOT reshape its height (excel flow spec D4): the editor
/// the grid spins up on focus is seeded with the cell's real content-box width,
/// so it wraps exactly as the idle display path did. If the seed regresses, a
/// fresh editor falls back to the ~900px+ unmeasured width, under-measures a
/// multi-line cell, and the autofit row collapses/shifts on click. This pins the
/// seeded width to the narrow column box, not the fallback.
#[gpui::test]
fn focusing_a_cell_seeds_the_column_width_not_the_fallback(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  flow.update(cx, |editor, cx| editor.create_sheet(cx));
  cx.run_until_parked();

  // A cell carrying enough text to wrap across several lines in a real column —
  // the case where a too-wide fallback wraps to fewer lines and mis-measures.
  let cell = flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor
      .type_into_cursor(
        "Extinction outweighs on magnitude and timeframe because the impact is irreversible.",
        cx,
      )
      .expect("cell created")
  });
  cx.run_until_parked();

  // Typing already built (and rendered) this cell's editor, settling its width.
  // Forget it so the next activation rebuilds it FRESH, then read the width the
  // INSTANT it is created — before any render pass settles it to the real cell
  // bounds. This is the first-focus frame the user sees: without the seed the
  // fresh editor has no layout width yet (`None`) and would fall back to ~900px+
  // on that frame; the seed must have already pinned it to the narrow column box.
  let width = flow.update(cx, |editor, cx| {
    editor.benchmark_forget_cell_editor(cell);
    editor.activate_cell(cell, cx);
    editor.active_cell_layout_width(cx)
  });
  let width = width.expect("the focused cell's editor must be seeded with a layout width on creation, before it renders");
  assert!(
    width > gpui::px(120.0) && width < gpui::px(400.0),
    "focus must seed the editor to the column's content box (~262px for a 280px column), \
     not the ~900px+ unmeasured fallback that collapses the autofit row; got {width:?}"
  );
}

/// Two empty cells stacked in column 0, at rows 0 and 1. Returns (top, bottom).
fn two_stacked_cells(cx: &mut TestAppContext, h: &support::WorkspaceHarness) -> (gpui::Entity<crate::flow::FlowEditor>, uuid::Uuid, uuid::Uuid) {
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  flow.update(cx, |editor, cx| {
    editor.create_sheet(cx);
    editor.add_first_argument(cx);
    editor.add_new_family(cx);
  });
  cx.run_until_parked();
  let (top, bottom) = flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert_eq!(sheet.rows.len(), 2, "two stacked cells occupy two rows");
    let at = |row, col| sheet.cells().find(|cell| sheet.cell_position(cell.id) == Some((row, col))).expect("cell at slot").id;
    (at(0, 0), at(1, 0))
  });
  (flow, top, bottom)
}

/// Excel muscle memory (fix #1): Enter moves the cursor DOWN to the existing
/// row below — it must NOT insert a new row. Before this fix the window
/// interceptor routed Enter to `add_sibling(After)`, growing the sheet on every
/// press.
#[gpui::test]
fn enter_moves_the_cursor_down_a_row_without_inserting_one(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let (flow, top, bottom) = two_stacked_cells(cx, &h);

  // Sit on the top cell, then press Enter.
  flow.update(cx, |editor, cx| editor.activate_cell(top, cx));
  cx.run_until_parked();
  cx.simulate_keystrokes(h.window, "enter");
  cx.run_until_parked();

  flow.read_with(cx, |editor, _cx| {
    assert_eq!(editor.cursor(), Some((1, 0)), "Enter stepped the cursor down one row");
    assert_eq!(editor.active_cell(), Some(bottom), "Enter landed on the EXISTING cell below, not a new one");
    assert_eq!(editor.board().sheets[0].rows.len(), 2, "Enter must not insert a row (Excel behavior)");
  });
}

/// Fix #2: Backspace in an EMPTY cell removes it and steps the cursor UP to the
/// previous row, so repeated backspaces walk back up through blanks. (A
/// non-empty cell is untouched here — the editor deletes a character.)
#[gpui::test]
fn backspace_in_empty_cell_deletes_it_and_moves_up(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let (flow, top, bottom) = two_stacked_cells(cx, &h);

  // Sit on the bottom (empty) cell and confirm the guard sees it as empty.
  flow.update(cx, |editor, cx| editor.activate_cell(bottom, cx));
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| assert!(editor.active_cell_is_empty(), "the bottom cell must be empty for this path"));

  cx.simulate_keystrokes(h.window, "backspace");
  cx.run_until_parked();

  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert!(sheet.find_cell(bottom).is_none(), "backspace deleted the empty cell");
    assert_eq!(editor.cursor(), Some((0, 0)), "the cursor stepped UP to the previous row");
    assert_eq!(editor.active_cell(), Some(top), "it landed on the cell above");
  });
}

// W2 range grammar: shift-extend selects the rectangle between the anchor
// (here the cursor) and the clicked slot; empty slots contribute no cell.
#[gpui::test]
fn shift_range_selects_the_rectangle(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  flow.update(cx, |editor, cx| editor.create_sheet(cx));
  cx.run_until_parked();

  // A 2×2 block of cells at (0,0),(0,1),(1,0),(1,1).
  let ids = flow.update(cx, |editor, cx| {
    let mut ids = Vec::new();
    for (row, column) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
      editor.set_cursor(row, column, cx);
      ids.push(editor.type_into_cursor("x", cx).expect("cell created"));
    }
    ids
  });
  cx.run_until_parked();

  // Anchor at (0,0), shift-extend to (1,1) — every cell in the rectangle.
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(1, 1, cx);
  });
  flow.read_with(cx, |editor, _cx| {
    let selected = editor.selected_cells();
    assert_eq!(selected.len(), 4, "the 2×2 rectangle selected all four cells");
    for id in &ids {
      assert!(selected.contains(id), "every cell in the rect is selected");
    }
  });

  // A narrower extend to (0,1) keeps only the top row.
  flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    editor.select_cell_range(0, 1, cx);
  });
  flow.read_with(cx, |editor, _cx| {
    assert_eq!(editor.selected_cells().len(), 2, "the top-row range holds exactly two cells");
  });
}

// Q3: dragging a member of a multi-selection translates the WHOLE set by the
// dragged cell's delta, as one undo group.
#[gpui::test]
fn multiselect_drag_translates_the_whole_set(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  flow.update(cx, |editor, cx| editor.create_sheet(cx));
  cx.run_until_parked();

  let (sheet_id, a, b) = flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    let a = editor.type_into_cursor("a", cx).expect("cell a");
    editor.set_cursor(1, 0, cx);
    let b = editor.type_into_cursor("b", cx).expect("cell b");
    let sheet_id = editor.board().sheets[0].id;
    (sheet_id, a, b)
  });
  cx.run_until_parked();

  // Select both column-0 cells; drag the block one column right.
  flow.update(cx, |editor, cx| {
    editor.activate_cell(a, cx);
    editor.toggle_select_cell(b, cx);
    editor.move_selection_block(sheet_id, a, 0, 1, cx);
  });
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert_eq!(sheet.cell_position(a), Some((0, 1)), "a shifted one column right");
    assert_eq!(sheet.cell_position(b), Some((1, 1)), "b shifted with the set");
  });

  // ONE undo reverts the whole block move.
  flow.update(cx, |editor, cx| editor.undo(cx));
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert_eq!(sheet.cell_position(a), Some((0, 0)), "undo returned a to column 0");
    assert_eq!(sheet.cell_position(b), Some((1, 0)), "undo returned b to column 0");
  });
}

// F: dragging a block onto occupied cells BLOCK-SWAPS — the displaced cards
// (not in the selection) slide into the slots the block vacated. Lossless.
#[gpui::test]
fn multiselect_drag_block_swaps_displaced_cards(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow = h.read(cx, |ws| ws.active_flow.clone()).expect("active flow");
  flow.update(cx, |editor, cx| editor.create_sheet(cx));
  cx.run_until_parked();

  // Column 0 holds the selection {p, q}; column 1 holds outsiders {x, y}.
  let (sheet_id, p, q, x, y) = flow.update(cx, |editor, cx| {
    editor.set_cursor(0, 0, cx);
    let p = editor.type_into_cursor("p", cx).expect("p");
    editor.set_cursor(1, 0, cx);
    let q = editor.type_into_cursor("q", cx).expect("q");
    editor.set_cursor(0, 1, cx);
    let x = editor.type_into_cursor("x", cx).expect("x");
    editor.set_cursor(1, 1, cx);
    let y = editor.type_into_cursor("y", cx).expect("y");
    let sheet_id = editor.board().sheets[0].id;
    (sheet_id, p, q, x, y)
  });
  cx.run_until_parked();

  // Select column 0; drag the block one column right, onto x and y.
  flow.update(cx, |editor, cx| {
    editor.activate_cell(p, cx);
    editor.toggle_select_cell(q, cx);
    editor.move_selection_block(sheet_id, p, 0, 1, cx);
  });
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert_eq!(sheet.cell_position(p), Some((0, 1)), "p took its destination");
    assert_eq!(sheet.cell_position(q), Some((1, 1)), "q took its destination");
    assert_eq!(sheet.cell_position(x), Some((0, 0)), "x slid into the vacated slot");
    assert_eq!(sheet.cell_position(y), Some((1, 0)), "y slid into the vacated slot");
  });

  // ONE undo restores every card — block-swap is a single group.
  flow.update(cx, |editor, cx| editor.undo(cx));
  cx.run_until_parked();
  flow.read_with(cx, |editor, _cx| {
    let sheet = &editor.board().sheets[0];
    assert_eq!(sheet.cell_position(p), Some((0, 0)), "undo restored p");
    assert_eq!(sheet.cell_position(q), Some((1, 0)), "undo restored q");
    assert_eq!(sheet.cell_position(x), Some((0, 1)), "undo restored x");
    assert_eq!(sheet.cell_position(y), Some((1, 1)), "undo restored y");
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
