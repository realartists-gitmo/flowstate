//! S2 gate: every grid intent class through the gated handle, undo
//! ping-pong, import classification (per-cell streams), the publish
//! contract, and the grid structural repair pass (D2 bump-down convergence).

use flowstate_document::{InputParagraph, InputRun, RunStyles};
use flowstate_flow::{CellId, CellSeed, ColumnId, FlowIntent, RowId, SheetId};
use uuid::Uuid;

use super::runtime::{FlowPublishEvent, FlowRuntime};
use super::{FlowDocHandle, FlowStreamItem};
use crate::local_write::GateHolder;

fn paragraphs(text: &str) -> Vec<InputParagraph> {
  vec![InputParagraph {
    style: flowstate_document::PARAGRAPH_TAG,
    runs: vec![InputRun {
      text: text.into(),
      styles: RunStyles::default(),
    }],
  }]
}

fn handle_with_sheet() -> (FlowDocHandle, SheetId) {
  let runtime = FlowRuntime::new_empty();
  let sheet_type = runtime.board().format.sheet_types[0].id;
  let (handle, _gate) = FlowDocHandle::new(runtime);
  let sheet = Uuid::new_v4();
  handle
    .apply(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "Case".into(),
      sheet_type_id: sheet_type,
    })
    .unwrap();
  (handle, sheet)
}

fn column(handle: &FlowDocHandle, sheet: SheetId, index: usize) -> ColumnId {
  handle.board_projection().unwrap().sheet(sheet).unwrap().columns[index].id
}

fn append_row(handle: &FlowDocHandle, sheet: SheetId) -> RowId {
  let row_id = Uuid::new_v4();
  handle
    .apply(&FlowIntent::InsertRows {
      sheet_id: sheet,
      before: None,
      row_ids: vec![row_id],
    })
    .unwrap();
  row_id
}

fn add_cell_at(handle: &FlowDocHandle, sheet: SheetId, row: RowId, column_id: ColumnId, text: &str) -> CellId {
  let cell_id = Uuid::new_v4();
  handle
    .apply(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id,
      row_id: row,
      column_id,
      seed: CellSeed::Paragraphs(paragraphs(text)),
    })
    .unwrap();
  cell_id
}

/// Fresh row at the bottom + a cell in the first column (the ghost-row
/// materialization shape most tests want).
fn add_cell(handle: &FlowDocHandle, sheet: SheetId, text: &str) -> CellId {
  let row = append_row(handle, sheet);
  add_cell_at(handle, sheet, row, column(handle, sheet, 0), text)
}

fn cell_count(handle: &FlowDocHandle, sheet: SheetId) -> usize {
  handle
    .board_projection()
    .unwrap()
    .sheet(sheet)
    .unwrap()
    .cells()
    .count()
}

fn drain_publish(handle: &FlowDocHandle) -> Vec<Vec<u8>> {
  let mut guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
  guard
    .take_pending_publish()
    .into_iter()
    .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
    .collect()
}

fn import_into(handle: &FlowDocHandle, blobs: &[Vec<u8>]) {
  handle
    .import_remote_updates(&blobs.iter().map(Vec::as_slice).collect::<Vec<_>>())
    .unwrap();
}

#[test]
fn every_intent_class_commits_through_the_gate() {
  let (handle, sheet) = handle_with_sheet();
  let row1 = append_row(&handle, sheet);
  let row2 = append_row(&handle, sheet);
  let col0 = column(&handle, sheet, 0);
  let col1 = column(&handle, sheet, 1);
  let a1 = add_cell_at(&handle, sheet, row1, col0, "A1");
  let b1 = add_cell_at(&handle, sheet, row1, col1, "B1");

  handle
    .apply(&FlowIntent::SetCellAddress {
      sheet_id: sheet,
      cell_id: b1,
      row_id: row2,
      column_id: col1,
    })
    .unwrap();
  handle
    .apply(&FlowIntent::MoveRows {
      sheet_id: sheet,
      row_ids: vec![row2],
      before: Some(row1),
    })
    .unwrap();
  handle
    .apply(&FlowIntent::SetRowHeight {
      sheet_id: sheet,
      row_id: row1,
      height: Some(96.0),
    })
    .unwrap();
  handle
    .apply(&FlowIntent::SetCellStruck {
      sheet_id: sheet,
      cell_id: a1,
      struck: true,
    })
    .unwrap();
  let overview = Uuid::new_v4();
  handle
    .apply(&FlowIntent::AddColumn {
      sheet_id: sheet,
      column_id: overview,
      label: "Overview".into(),
      side: flowstate_flow::ArgumentSide::One,
      before: Some(col0),
    })
    .unwrap();
  handle
    .apply(&FlowIntent::RenameColumn {
      sheet_id: sheet,
      column_id: overview,
      label: "OV".into(),
    })
    .unwrap();
  handle
    .apply(&FlowIntent::SetColumnWidth {
      sheet_id: sheet,
      column_id: overview,
      width: Some(320.0),
    })
    .unwrap();
  handle
    .apply(&FlowIntent::MoveColumn {
      sheet_id: sheet,
      column_id: overview,
      before: None,
    })
    .unwrap();
  handle
    .apply(&FlowIntent::RenameSheet {
      sheet_id: sheet,
      name: "Renamed".into(),
    })
    .unwrap();

  let board = handle.board_projection().unwrap();
  let sheet_ref = board.sheet(sheet).unwrap();
  assert_eq!(sheet_ref.name, "Renamed");
  assert_eq!(sheet_ref.rows[0].id, row2, "MoveRows landed row2 first");
  assert_eq!(sheet_ref.rows[1].height_override, Some(96.0));
  assert!(sheet_ref.find_cell(a1).unwrap().summary.struck);
  assert_eq!(sheet_ref.find_cell(b1).unwrap().row_id, row2);
  assert_eq!(sheet_ref.columns.last().unwrap().label, "OV");
  assert_eq!(sheet_ref.columns.last().unwrap().width, Some(320.0));

  handle
    .apply(&FlowIntent::DeleteColumn {
      sheet_id: sheet,
      column_id: overview,
    })
    .unwrap();
  handle
    .apply(&FlowIntent::DeleteCell {
      sheet_id: sheet,
      cell_id: b1,
    })
    .unwrap();
  handle
    .apply(&FlowIntent::DeleteRows {
      sheet_id: sheet,
      row_ids: vec![row2],
    })
    .unwrap();
  handle
    .apply(&FlowIntent::DeleteSheet { sheet_id: sheet })
    .unwrap();
  assert!(handle.board_projection().unwrap().sheets.is_empty());
}

// A cell text insert must return the POST-edit caret (spec §8): without it the
// editor strands the caret at the pre-edit offset and typing piles up reversed.
#[test]
fn cell_text_insert_advances_the_caret() {
  use gpui_flowtext::{DocumentOffset, InsertTextIntent, LocalIntent, LocalWriteAuthority as _, LocalWriteOutcome, TextAnchor};
  let (handle, sheet) = handle_with_sheet();
  let cell = add_cell(&handle, sheet, "hello");
  let projection = handle.cell_projection(cell).expect("cell projection");
  let paragraph = projection.ids.paragraph_ids[0];
  let authority = handle.cell_authority(cell);
  let outcome = authority
    .apply(LocalIntent::InsertText(InsertTextIntent {
      at: TextAnchor::new(paragraph, 5), // end of "hello"
      text: "X".into(),
      style_override: None,
    }))
    .expect("insert applies");
  let LocalWriteOutcome::CommittedWithRebuild { commit, .. } = outcome else {
    panic!("cell text commits with a full rebuild");
  };
  let selection = commit.selection_after.expect("the commit carries a post-edit caret");
  assert_eq!(
    selection.head.offset,
    DocumentOffset { paragraph: 0, byte: 6 },
    "the caret lands after the inserted character, not stranded at the start"
  );
}

#[test]
fn rejections_speak_and_leave_state_clean() {
  let (handle, sheet) = handle_with_sheet();
  let row = append_row(&handle, sheet);
  let col0 = column(&handle, sheet, 0);
  let col1 = column(&handle, sheet, 1);
  let _a1 = add_cell_at(&handle, sheet, row, col0, "A1");
  let b1 = add_cell_at(&handle, sheet, row, col1, "B1");
  let error = handle
    .apply(&FlowIntent::SetCellAddress {
      sheet_id: sheet,
      cell_id: b1,
      row_id: row,
      column_id: col0,
    })
    .unwrap_err();
  assert!(error.to_string().contains("occupied"), "{error}");
  let board = handle.board_projection().unwrap();
  assert_eq!(board.sheet(sheet).unwrap().cells().count(), 2, "rejected intent must not mutate");
  assert_eq!(board.sheet(sheet).unwrap().find_cell(b1).unwrap().column_id, col1);
}

#[test]
fn undo_redo_ping_pong_per_class() {
  let (handle, sheet) = handle_with_sheet();
  let a1 = add_cell(&handle, sheet, "A1");
  let struck_of = |handle: &FlowDocHandle| {
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .find_cell(a1)
      .map(|cell| cell.summary.struck)
  };
  handle
    .apply(&FlowIntent::SetCellStruck {
      sheet_id: sheet,
      cell_id: a1,
      struck: true,
    })
    .unwrap();
  assert_eq!(struck_of(&handle), Some(true));
  assert!(handle.undo().unwrap());
  assert_eq!(struck_of(&handle), Some(false));
  assert!(handle.redo().unwrap());
  assert_eq!(struck_of(&handle), Some(true));
  // Structural class: un-strike, un-add.
  assert!(handle.undo().unwrap());
  assert!(handle.undo().unwrap());
  assert_eq!(cell_count(&handle, sheet), 0);
  assert!(handle.redo().unwrap());
  assert_eq!(cell_count(&handle, sheet), 1);
}

#[test]
fn undo_grouping_is_one_member() {
  let (handle, sheet) = handle_with_sheet();
  handle.undo_group_start().unwrap();
  let _a1 = add_cell(&handle, sheet, "A1");
  let _b1 = add_cell(&handle, sheet, "B1");
  handle.undo_group_end().unwrap();
  assert_eq!(cell_count(&handle, sheet), 2);
  assert!(handle.undo().unwrap());
  assert_eq!(cell_count(&handle, sheet), 0, "grouped intents (rows + cells) undo as one member");
  assert!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .rows
      .is_empty(),
    "the grouped row inserts undo with their cells"
  );
}

#[test]
fn publish_queue_carries_local_commits() {
  let (handle, sheet) = handle_with_sheet();
  let _a1 = add_cell(&handle, sheet, "A1");
  let published = {
    let mut guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
    guard.take_pending_publish()
  };
  assert!(!published.is_empty(), "local commits must queue publish events");
  for event in &published {
    let FlowPublishEvent::LocalUpdate { bytes, .. } = event;
    assert!(!bytes.is_empty());
  }
}

#[test]
fn two_handles_converge_via_publish_and_import() {
  let (a, sheet) = handle_with_sheet();
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  let _ = drain_publish(&a);

  let a1 = add_cell(&a, sheet, "from A");
  let b1 = add_cell(&b, sheet, "from B");

  let from_a = drain_publish(&a);
  let from_b = drain_publish(&b);
  import_into(&a, &from_b);
  import_into(&b, &from_a);
  // Drain the (possibly empty) repair deltas both ways so canonical state
  // fully converges too.
  let from_a = drain_publish(&a);
  let from_b = drain_publish(&b);
  import_into(&a, &from_b);
  import_into(&b, &from_a);

  let board_a = a.board_projection().unwrap();
  let board_b = b.board_projection().unwrap();
  assert_eq!(board_a, board_b, "publish/import round converges");
  assert!(board_a.sheet(sheet).unwrap().find_cell(a1).is_some());
  assert!(board_a.sheet(sheet).unwrap().find_cell(b1).is_some());
}

/// The D2 law end-to-end through the RUNTIME: concurrent cells on one
/// address collide after import; the normalizer bumps in projection space;
/// the repair pass converges CANONICAL state (bump row into `row_order`,
/// loser's register rewritten) so the defect clears — on both peers,
/// identically.
#[test]
fn slot_collision_repairs_canonically_on_both_peers() {
  let (a, sheet) = handle_with_sheet();
  let row = append_row(&a, sheet);
  let col0 = column(&a, sheet, 0);
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  let _ = drain_publish(&a);

  let cell_a = add_cell_at(&a, sheet, row, col0, "from A");
  let cell_b = add_cell_at(&b, sheet, row, col0, "from B");
  let (winner, loser) = if cell_a < cell_b { (cell_a, cell_b) } else { (cell_b, cell_a) };

  let from_a = drain_publish(&a);
  let from_b = drain_publish(&b);
  import_into(&a, &from_b);
  import_into(&b, &from_a);
  // Cross-import the repair deltas.
  let from_a = drain_publish(&a);
  let from_b = drain_publish(&b);
  import_into(&a, &from_b);
  import_into(&b, &from_a);

  let board_a = a.board_projection().unwrap();
  let board_b = b.board_projection().unwrap();
  assert_eq!(board_a, board_b, "collision resolution converges");
  let sheet_ref = board_a.sheet(sheet).unwrap();
  let bump_row = flowstate_flow::bump_row_id(loser, 1);
  assert_eq!(sheet_ref.slot_by_ids(row, col0).unwrap().id, winner);
  assert_eq!(sheet_ref.slot_by_ids(bump_row, col0).unwrap().id, loser);
  for handle in [&a, &b] {
    let defects = {
      let guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
      guard.defects().to_vec()
    };
    assert!(defects.is_empty(), "the repair pass converged canonical state; defects must clear: {defects:?}");
  }
}

#[test]
fn import_classification_feeds_per_cell_streams() {
  let (a, sheet) = handle_with_sheet();
  let cell = add_cell(&a, sheet, "shared");
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  let _ = a.drain_board_stream().unwrap();
  let _ = a.drain_cell_stream(cell).unwrap();

  b.apply(&FlowIntent::ReplaceCellContent {
    sheet_id: sheet,
    cell_id: cell,
    paragraphs: paragraphs("shared — edited remotely"),
  })
  .unwrap();
  let updates = drain_publish(&b);
  import_into(&a, &updates);

  let board_items = a.drain_board_stream().unwrap();
  assert!(
    board_items.iter().any(|FlowStreamItem::Board(board)| {
      board
        .sheet(sheet)
        .and_then(|s| s.find_cell(cell))
        .is_some_and(|c| c.summary.summary_text.contains("edited remotely"))
    }),
    "board stream carries the remote change"
  );
  let cell_items = a.drain_cell_stream(cell).unwrap();
  assert!(!cell_items.is_empty(), "the edited cell's stream must receive a Replace");
}

/// The incremental structural paths (local AddCell / DeleteCell patch the
/// retained board instead of rematerializing it) must produce a board and
/// routing index IDENTICAL to a fresh full materialization at every step —
/// `verify_board_equivalence` asserts exactly that. Covers the deterministic
/// cases the soak's randomized fuzz may not hit (explicit delete, move
/// fallback, re-add into a vacated slot).
#[test]
fn incremental_structural_matches_full_materialization() {
  let (handle, sheet) = handle_with_sheet();
  let verify = |handle: &FlowDocHandle| {
    handle.gate().lock(GateHolder::DocumentService).unwrap().verify_board_equivalence();
  };

  let row1 = append_row(&handle, sheet);
  verify(&handle);
  let row2 = append_row(&handle, sheet);
  verify(&handle);
  let col0 = column(&handle, sheet, 0);
  let col1 = column(&handle, sheet, 1);

  // AddCell — incremental patch on a clean board.
  let a1 = add_cell_at(&handle, sheet, row1, col0, "A1");
  verify(&handle);
  let b1 = add_cell_at(&handle, sheet, row1, col1, "B1");
  verify(&handle);
  let a2 = add_cell_at(&handle, sheet, row2, col0, "A2");
  verify(&handle);

  // Strike — content-only patch (a text mark).
  handle
    .apply(&FlowIntent::SetCellStruck {
      sheet_id: sheet,
      cell_id: a1,
      struck: true,
    })
    .unwrap();
  verify(&handle);

  // Move — placement change, takes the full-refresh fallback.
  handle
    .apply(&FlowIntent::SetCellAddress {
      sheet_id: sheet,
      cell_id: b1,
      row_id: row2,
      column_id: col1,
    })
    .unwrap();
  verify(&handle);

  // DeleteCell — incremental patch, then re-add a fresh cell into the vacated
  // slot (also incremental), then delete another.
  handle.apply(&FlowIntent::DeleteCell { sheet_id: sheet, cell_id: a1 }).unwrap();
  verify(&handle);
  let a1b = add_cell_at(&handle, sheet, row1, col0, "A1 again");
  verify(&handle);

  // Metadata patches — O(1), no cell touched.
  handle.apply(&FlowIntent::SetRowHeight { sheet_id: sheet, row_id: row1, height: Some(88.0) }).unwrap();
  verify(&handle);
  handle.apply(&FlowIntent::SetColumnWidth { sheet_id: sheet, column_id: col0, width: Some(240.0) }).unwrap();
  verify(&handle);
  handle.apply(&FlowIntent::RenameColumn { sheet_id: sheet, column_id: col1, label: "Renamed".into() }).unwrap();
  verify(&handle);
  handle.apply(&FlowIntent::RenameSheet { sheet_id: sheet, name: "Sheet Renamed".into() }).unwrap();
  verify(&handle);
  handle
    .apply(&FlowIntent::AddColumn {
      sheet_id: sheet,
      column_id: Uuid::new_v4(),
      label: "New Speech".into(),
      side: flowstate_flow::ArgumentSide::Two,
      before: None,
    })
    .unwrap();
  verify(&handle);

  // Swap two live cells (a1b @ row1/col0, b1 @ row2/col1) — they exchange slots.
  handle.apply(&FlowIntent::SwapCells { sheet_id: sheet, a: a1b, b: b1 }).unwrap();
  verify(&handle);

  handle.apply(&FlowIntent::DeleteCell { sheet_id: sheet, cell_id: a2 }).unwrap();
  verify(&handle);
}

/// `FlowRuntime::from_flow_document` REUSES the document's already-materialized
/// board (the cold-open dedup) instead of rematerializing. The seeded board must
/// still equal a fresh materialization of the runtime's own doc.
#[test]
fn from_flow_document_seeds_board_equal_to_materialization() {
  let mut document = flowstate_flow::FlowDocument::new();
  let sheet_type = document.projection().format.sheet_types[0].id;
  let sheet = Uuid::new_v4();
  document
    .apply_intent(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "s".into(),
      sheet_type_id: sheet_type,
    })
    .unwrap();
  let rows: Vec<RowId> = (0..3).map(|_| Uuid::new_v4()).collect();
  document
    .apply_intent(&FlowIntent::InsertRows {
      sheet_id: sheet,
      before: None,
      row_ids: rows.clone(),
    })
    .unwrap();
  let col = document.projection().sheet(sheet).unwrap().columns[0].id;
  document
    .apply_intent(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id: Uuid::new_v4(),
      row_id: rows[1],
      column_id: col,
      seed: CellSeed::Paragraphs(paragraphs("seeded")),
    })
    .unwrap();

  let runtime = FlowRuntime::from_flow_document(&document).unwrap();
  // Panics if the seeded board or index diverges from a fresh materialization.
  runtime.verify_board_equivalence();
  assert_eq!(runtime.board(), document.projection());
}

/// The forked doc `from_flow_document` builds must be fully WRITABLE — the fork
/// has to carry the text-style config so cell keystrokes and structural edits
/// commit, and the board must stay equal to a fresh materialization after.
#[test]
fn from_flow_document_runtime_is_writable() {
  use gpui_flowtext::{InsertTextIntent, LocalIntent, LocalWriteAuthority as _, TextAnchor};

  let mut document = flowstate_flow::FlowDocument::new();
  let sheet_type = document.projection().format.sheet_types[0].id;
  let sheet = Uuid::new_v4();
  document
    .apply_intent(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "s".into(),
      sheet_type_id: sheet_type,
    })
    .unwrap();
  let rows: Vec<RowId> = (0..2).map(|_| Uuid::new_v4()).collect();
  document
    .apply_intent(&FlowIntent::InsertRows {
      sheet_id: sheet,
      before: None,
      row_ids: rows.clone(),
    })
    .unwrap();
  let col = document.projection().sheet(sheet).unwrap().columns[0].id;
  let c0 = Uuid::new_v4();
  document
    .apply_intent(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id: c0,
      row_id: rows[0],
      column_id: col,
      seed: CellSeed::Paragraphs(paragraphs("start ")),
    })
    .unwrap();

  let (handle, _gate) = FlowDocHandle::new(FlowRuntime::from_flow_document(&document).unwrap());

  // Text write on the forked doc.
  let paragraph = handle.cell_projection(c0).unwrap().ids.paragraph_ids[0];
  handle
    .cell_authority(c0)
    .apply(LocalIntent::InsertText(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "X".into(),
      style_override: None,
    }))
    .unwrap();
  // Structural write.
  let c1 = add_cell_at(&handle, sheet, rows[1], col, "second");

  handle.gate().lock(GateHolder::DocumentService).unwrap().verify_board_equivalence();
  let board = handle.board_projection().unwrap();
  assert!(board.sheet(sheet).unwrap().find_cell(c1).is_some());
  assert!(board.sheet(sheet).unwrap().find_cell(c0).unwrap().summary.summary_text.starts_with('X'));
}

// ------------------------------------------------------- cell authority ----

mod cell_authority_tests {
  use super::*;
  use gpui_flowtext::{
    DeleteRangeIntent, DocumentOffset, EditorSelection, FragmentBlock, InsertRichFragmentIntent, InsertTextIntent, JoinParagraphsIntent,
    LocalIntent, LocalWriteAuthority as _, LocalWriteOutcome, ReplaceMatch, ReplaceMatchesIntent, RunStyles, SetMarksIntent,
    SetParagraphStyleIntent, SplitParagraphIntent, TextAnchor, WriteRejected,
  };

  fn cell_fixture() -> (FlowDocHandle, SheetId, CellId) {
    let (handle, sheet) = handle_with_sheet();
    let cell = add_cell(&handle, sheet, "seed text");
    (handle, sheet, cell)
  }

  fn projection_of(handle: &FlowDocHandle, cell: CellId) -> flowstate_document::DocumentProjection {
    handle.cell_projection(cell).unwrap()
  }

  fn first_paragraph_id(projection: &flowstate_document::DocumentProjection) -> gpui_flowtext::ParagraphId {
    projection.ids.paragraph_ids[0]
  }

  #[test]
  fn typing_is_minimal_ops_and_merges_with_remote() {
    let (a, _sheet, cell) = cell_fixture();
    let snapshot = {
      let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
      guard.snapshot_bytes().unwrap()
    };
    let (b, _gb) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
    let _ = drain_publish(&a);

    // A types at the start; B types at the end — same cell, concurrently.
    let authority_a = a.cell_authority(cell);
    let authority_b = b.cell_authority(cell);
    let paragraph_a = first_paragraph_id(&projection_of(&a, cell));
    let paragraph_b = first_paragraph_id(&projection_of(&b, cell));
    assert_eq!(paragraph_a, paragraph_b, "durable ids agree across peers");
    authority_a
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph_a, 0),
        text: "aff ".into(),
        style_override: None,
      }))
      .unwrap();
    authority_b
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph_b, "seed text".len()),
        text: " neg".into(),
        style_override: None,
      }))
      .unwrap();

    let from_a = drain_publish(&a);
    let from_b = drain_publish(&b);
    import_into(&a, &from_b);
    import_into(&b, &from_a);

    let text_a = projection_of(&a, cell).text.to_string();
    let text_b = projection_of(&b, cell).text.to_string();
    assert_eq!(text_a, text_b, "same-cell typing converges");
    assert_eq!(text_a, "aff seed text neg", "char-level merge, no clobber");
  }

  #[test]
  fn split_join_round_trip_with_registry_identity() {
    let (handle, _sheet, cell) = cell_fixture();
    let authority = handle.cell_authority(cell);
    let projection = projection_of(&handle, cell);
    let paragraph = first_paragraph_id(&projection);
    authority
      .apply(LocalIntent::SplitParagraph(SplitParagraphIntent {
        at: TextAnchor::new(paragraph, "seed".len()),
        inherited_style: gpui_flowtext::ParagraphStyle::Normal,
      }))
      .unwrap();
    let split = projection_of(&handle, cell);
    assert_eq!(split.paragraphs.len(), 2);
    assert_eq!(split.ids.paragraph_ids.len(), 2);
    assert_eq!(split.ids.paragraph_ids[0], paragraph, "first identity survives the split");
    let second = split.ids.paragraph_ids[1];
    assert_ne!(second, paragraph);

    authority
      .apply(LocalIntent::JoinParagraphs(JoinParagraphsIntent { first: paragraph, second }))
      .unwrap();
    let joined = projection_of(&handle, cell);
    assert_eq!(joined.paragraphs.len(), 1);
    assert_eq!(joined.text.to_string(), "seed text");
  }

  #[test]
  fn marks_styles_fragment_and_replace_matches() {
    let (handle, _sheet, cell) = cell_fixture();
    let authority = handle.cell_authority(cell);
    let paragraph = first_paragraph_id(&projection_of(&handle, cell));

    // Strike "seed".
    let struck = RunStyles {
      strikethrough: true,
      ..Default::default()
    };
    authority
      .apply(LocalIntent::SetMarks(SetMarksIntent {
        start: TextAnchor::new(paragraph, 0),
        end: TextAnchor::new(paragraph, 4),
        styles: struck,
      }))
      .unwrap();
    let projection = projection_of(&handle, cell);
    assert!(projection.paragraphs[0].runs[0].styles.strikethrough);
    assert!(
      !projection.paragraphs[0]
        .runs
        .last()
        .unwrap()
        .styles
        .strikethrough
    );

    // Paragraph style.
    authority
      .apply(LocalIntent::SetParagraphStyle(SetParagraphStyleIntent {
        paragraph,
        style: flowstate_document::PARAGRAPH_UNDERTAG,
      }))
      .unwrap();
    assert_eq!(projection_of(&handle, cell).paragraphs[0].style, flowstate_document::PARAGRAPH_UNDERTAG);

    // Paste a two-paragraph fragment at the end.
    authority
      .apply(LocalIntent::InsertRichFragment(InsertRichFragmentIntent {
        at: TextAnchor::new(paragraph, "seed text".len()),
        blocks: vec![
          FragmentBlock::Paragraph(gpui_flowtext::InputParagraph {
            style: gpui_flowtext::ParagraphStyle::Normal,
            runs: vec![gpui_flowtext::InputRun {
              text: " tail".into(),
              styles: RunStyles::default(),
            }],
          }),
          FragmentBlock::Paragraph(gpui_flowtext::InputParagraph {
            style: flowstate_document::PARAGRAPH_ANALYTIC,
            runs: vec![gpui_flowtext::InputRun {
              text: "analytic line".into(),
              styles: RunStyles::default(),
            }],
          }),
        ],
      }))
      .unwrap();
    let pasted = projection_of(&handle, cell);
    assert_eq!(pasted.paragraphs.len(), 2);
    assert_eq!(pasted.paragraphs[1].style, flowstate_document::PARAGRAPH_ANALYTIC);
    assert!(pasted.text.to_string().contains("seed text tail"));

    authority
      .apply(LocalIntent::ReplaceMatches(ReplaceMatchesIntent {
        matches: vec![ReplaceMatch {
          start: TextAnchor::new(paragraph, 0),
          end: TextAnchor::new(paragraph, 4),
          styles: None,
          replacement_override: None,
        }],
        replacement: "SEED".into(),
      }))
      .unwrap();
    assert!(
      projection_of(&handle, cell)
        .text
        .to_string()
        .starts_with("SEED")
    );
  }

  #[test]
  fn object_intents_are_rejected_and_leave_state_clean() {
    let (handle, _sheet, cell) = cell_fixture();
    let authority = handle.cell_authority(cell);
    let before = projection_of(&handle, cell).text.to_string();
    let error = authority
      .apply(LocalIntent::DeleteBlocks(gpui_flowtext::DeleteBlocksIntent { blocks: vec![] }))
      .unwrap_err();
    assert!(matches!(error, WriteRejected::StructureViolation(_)));
    assert_eq!(projection_of(&handle, cell).text.to_string(), before);
  }

  #[test]
  fn outcome_is_rebuild_with_exact_projection() {
    let (handle, _sheet, cell) = cell_fixture();
    let authority = handle.cell_authority(cell);
    let paragraph = first_paragraph_id(&projection_of(&handle, cell));
    let outcome = authority
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "x".into(),
        style_override: None,
      }))
      .unwrap();
    match outcome {
      LocalWriteOutcome::CommittedWithRebuild { replace, .. } => {
        assert_eq!(replace.document.text.to_string(), "xseed text");
      },
      LocalWriteOutcome::Committed(_) => panic!("cell authority always returns rebuild outcomes"),
    }
  }

  #[test]
  fn selection_anchors_survive_concurrent_remote_edits() {
    let (a, _sheet, cell) = cell_fixture();
    let snapshot = {
      let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
      guard.snapshot_bytes().unwrap()
    };
    let (b, _gb) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());

    let authority_a = a.cell_authority(cell);
    // Caret between "seed" and " text" (paragraph 0, byte 4).
    let selection = EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: 4 });
    let frontier = {
      let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
      guard.frontier()
    };
    let (head, anchor) = authority_a
      .encode_selection_anchor(&selection, &frontier)
      .expect("anchors encode");

    // B prepends text; A imports.
    let authority_b = b.cell_authority(cell);
    let paragraph_b = first_paragraph_id(&projection_of(&b, cell));
    authority_b
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph_b, 0),
        text: "ABC ".into(),
        style_override: None,
      }))
      .unwrap();
    let updates = drain_publish(&b);
    import_into(&a, &updates);

    let (resolved_head, _resolved_anchor) = authority_a
      .resolve_selection_anchor(&head, &anchor)
      .expect("anchors resolve");
    assert_eq!(
      resolved_head,
      DocumentOffset { paragraph: 0, byte: 8 },
      "caret shifts past the concurrent 4-char prepend"
    );

    // A stale-frontier encode must refuse (the editor hasn't drained yet).
    assert!(
      authority_a
        .encode_selection_anchor(&selection, &frontier)
        .is_none(),
      "encoding against a stale frontier must return None"
    );
  }

  #[test]
  fn delete_range_across_boundary_prunes_registry() {
    let (handle, _sheet, cell) = cell_fixture();
    let authority = handle.cell_authority(cell);
    let paragraph = first_paragraph_id(&projection_of(&handle, cell));
    authority
      .apply(LocalIntent::SplitParagraph(SplitParagraphIntent {
        at: TextAnchor::new(paragraph, 4),
        inherited_style: gpui_flowtext::ParagraphStyle::Normal,
      }))
      .unwrap();
    let split = projection_of(&handle, cell);
    let second = split.ids.paragraph_ids[1];
    // Delete across the boundary (from mid-first into mid-second).
    authority
      .apply(LocalIntent::DeleteRange(DeleteRangeIntent {
        start: TextAnchor::new(paragraph, 2),
        end: TextAnchor::new(second, 2),
      }))
      .unwrap();
    let after = projection_of(&handle, cell);
    assert_eq!(after.paragraphs.len(), 1, "boundary removed");
    assert_eq!(after.text.to_string(), "seext");
    assert_eq!(after.ids.paragraph_ids[0], paragraph, "surviving identity is the first paragraph");
  }

  /// I-10 compensation + publish law. A rich fragment whose SECOND block is
  /// an object fails mid-apply (the first paragraph's runs are already in the
  /// text). The compensation must (a) restore the local projection exactly
  /// and (b) PUBLISH the partial+revert ops — later exports start after them,
  /// so an unpublished compensation is a permanent causal gap: every
  /// subsequent update from this peer would stall as a pending import on
  /// every other peer.
  #[test]
  fn mid_apply_failure_compensates_and_publishes_no_causal_gap() {
    let (a, _sheet, cell) = cell_fixture();
    let snapshot = {
      let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
      guard.snapshot_bytes().unwrap()
    };
    let (b, _gb) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
    let authority = a.cell_authority(cell);
    let paragraph = first_paragraph_id(&projection_of(&a, cell));
    let before = projection_of(&a, cell).text.to_string();

    let error = authority
      .apply(LocalIntent::InsertRichFragment(InsertRichFragmentIntent {
        at: TextAnchor::new(paragraph, 0),
        blocks: vec![
          FragmentBlock::Paragraph(gpui_flowtext::InputParagraph {
            style: gpui_flowtext::ParagraphStyle::Normal,
            runs: vec![gpui_flowtext::InputRun {
              text: "leaked".into(),
              styles: RunStyles::default(),
            }],
          }),
          FragmentBlock::Object(gpui_flowtext::InputBlock::Equation(gpui_flowtext::InputEquationBlock {
            source: "x".into(),
            syntax: gpui_flowtext::InputEquationSyntax::Latex,
            display: gpui_flowtext::InputEquationDisplay::Display,
          })),
        ],
      }))
      .unwrap_err();
    assert!(matches!(error, WriteRejected::StructureViolation(_)));
    assert_eq!(projection_of(&a, cell).text.to_string(), before, "compensation restored the cell");

    // A later successful edit must reach a peer cleanly: if the compensation
    // ops were never published, this import would dead-letter on missing deps.
    authority
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "after ".into(),
        style_override: None,
      }))
      .unwrap();
    let updates = drain_publish(&a);
    import_into(&b, &updates);
    assert_eq!(
      projection_of(&b, cell).text.to_string(),
      "after seed text",
      "peer received the post-failure edit (no causal gap)"
    );
  }
}

#[test]
fn history_scrubber_replays_the_lamport_prefix() {
  let runtime = FlowRuntime::new_empty();
  let sheet_type = runtime.board().format.sheet_types[0].id;
  let (handle, _gate) = FlowDocHandle::new(runtime);
  let sheet = Uuid::from_u128(0x51);
  handle
    .apply(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "History".into(),
      sheet_type_id: sheet_type,
    })
    .unwrap();
  let col0 = column(&handle, sheet, 0);
  for index in 0..6_u128 {
    let row = Uuid::from_u128(0x1000 + index);
    handle
      .apply(&FlowIntent::InsertRows {
        sheet_id: sheet,
        before: None,
        row_ids: vec![row],
      })
      .unwrap();
    handle
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: Uuid::from_u128(0x100 + index),
        row_id: row,
        column_id: col0,
        seed: CellSeed::Empty,
      })
      .unwrap();
  }
  let live = handle.board_projection().unwrap();
  assert_eq!(live.sheets[0].cells().count(), 6);

  // Full replay equals the live board's structure.
  let (full, shown, total, full_frontier) = handle.history_board_at(1.0).unwrap();
  assert_eq!(shown, total);
  assert_eq!(full.sheets.len(), 1);
  assert_eq!(full.sheets[0].cells().count(), 6, "fraction 1.0 replays everything");

  // A mid-timeline replay shows a strict prefix of the cells — and the LIVE
  // board is untouched by the checkout (it ran on a fork).
  let (half, shown_half, _, half_frontier) = handle.history_board_at(0.5).unwrap();
  let half_cells = half.sheets.first().map_or(0, |sheet| sheet.cells().count());
  assert!(half_cells < 6, "fraction 0.5 must replay a strict prefix (saw {half_cells} cells)");
  assert!(shown_half < total);
  assert_eq!(
    handle.board_projection().unwrap().sheets[0].cells().count(),
    6,
    "the live board is untouched by history checkouts"
  );
  // H-S6 tape: the scrub reports the checked-out position's frontier, and the
  // timeline positions of those frontiers land in order.
  assert_ne!(half_frontier, full_frontier, "different scrub positions report different frontiers");
  let positions = handle
    .history_timeline_positions(&[half_frontier, full_frontier])
    .unwrap();
  let half_pos = positions[0].expect("half frontier positions");
  let full_pos = positions[1].expect("full frontier positions");
  assert!(half_pos < full_pos, "timeline positions respect the replay order ({half_pos} < {full_pos})");
  assert!((full_pos - 1.0).abs() < 1e-4, "the full replay sits at the tape's end");
}

// ---- H-S6/H-S7: flow checkpoints converge; restore obeys the .db8 law -----

#[test]
fn flow_checkpoints_converge_and_restore_pins_then_reverts_undoably() {
  use flowstate_document::RevisionKind;
  let (a, sheet) = handle_with_sheet();
  let cell = add_cell(&a, sheet, "alpha");
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  let _ = drain_publish(&a);

  // Checkpoint at "alpha", then grow the board.
  let (checkpoint_id, historical_frontier) = {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    let id = guard.create_flow_checkpoint(None, RevisionKind::Session).unwrap();
    let frontier = guard
      .flow_checkpoints()
      .iter()
      .find(|checkpoint| checkpoint.checkpoint_id == id)
      .expect("checkpoint recorded")
      .frontier
      .clone();
    drop(guard);
    (id, frontier)
  };
  let _second_cell = add_cell(&a, sheet, "beta");

  // Rename PINS.
  let mut rename_guard = a.gate().lock(GateHolder::DocumentService).unwrap();
  rename_guard.rename_flow_checkpoint(checkpoint_id, "Before the block").unwrap();
  drop(rename_guard);

  // Restore to the checkpoint: the law demands a safety pin of the present
  // first, then a forward op.
  let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
  guard.restore_flow_frontier(&historical_frontier).unwrap();
  let board = guard.board();
  assert_eq!(board.sheets[0].cells().count(), 1, "the board reads as it did at the checkpoint");
  assert!(board.sheets[0].find_cell(cell).is_some());
  let checkpoints = guard.flow_checkpoints();
  assert!(
    checkpoints
      .iter()
      .any(|checkpoint| checkpoint.kind == RevisionKind::Named && checkpoint.title == "Before restore"),
    "flow restore is always preceded by a safety checkpoint"
  );
  assert!(
    checkpoints
      .iter()
      .any(|checkpoint| checkpoint.checkpoint_id == checkpoint_id
        && checkpoint.kind == RevisionKind::Named
        && checkpoint.title == "Before the block"),
    "rename pins the record"
  );
  drop(guard);

  // Convergence: B imports everything and agrees on board + checkpoints.
  let from_a = drain_publish(&a);
  let mut guard = b.gate().lock(GateHolder::DocumentService).unwrap();
  let blobs: Vec<&[u8]> = from_a.iter().map(Vec::as_slice).collect();
  guard.import_remote_updates(&blobs).unwrap();
  assert_eq!(guard.board().sheets[0].cells().count(), 1, "the restore converges like a normal edit");
  assert_eq!(
    guard.flow_checkpoints(),
    {
      let guard_a = a.gate().lock(GateHolder::DocumentService).unwrap();
      guard_a.flow_checkpoints()
    },
    "checkpoint records converge"
  );
  drop(guard);

  // Forward op: one undo on A returns the pre-restore board.
  let mut undo_guard = a.gate().lock(GateHolder::DocumentService).unwrap();
  assert!(undo_guard.undo().unwrap(), "restore is undoable");
  assert_eq!(undo_guard.board().sheets[0].cells().count(), 2, "undo returns the pre-restore board");
  drop(undo_guard);
}

// ---- C-S2: flow comments converge like any other flow state ---------------

#[test]
fn flow_comments_converge_with_cell_anchors_tombstones_and_frontiers() {
  let (a, sheet) = handle_with_sheet();
  let cell = add_cell(&a, sheet, "warrant here");
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  let _ = drain_publish(&a);

  // A: one anchored + one general comment; tombstone a reply (author-gated).
  let (anchored_id, general_id, reply_id) = {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    let anchored_id = guard.create_flow_comment(Some(cell), "This warrant is circular", 7, "Ada").unwrap();
    let general_id = guard.create_flow_comment(None, "Overview: wrong layer", 7, "Ada").unwrap();
    let reply_id = guard.reply_to_flow_comment(anchored_id, "fixing", 9, "Sol").unwrap();
    assert!(guard.delete_flow_comment_message(anchored_id, reply_id, 7).is_err());
    guard.delete_flow_comment_message(anchored_id, reply_id, 9).unwrap();
    drop(guard);
    (anchored_id, general_id, reply_id)
  };

  // Publish A -> import into B.
  let from_a = drain_publish(&a);
  let mut import_guard = b.gate().lock(GateHolder::DocumentService).unwrap();
  let blobs: Vec<&[u8]> = from_a.iter().map(Vec::as_slice).collect();
  import_guard.import_remote_updates(&blobs).unwrap();
  drop(import_guard);

  for handle in [&a, &b] {
    let guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
    let threads = guard.flow_comments();
    let anchored = threads.iter().find(|thread| thread.comment_id == anchored_id).expect("anchored");
    assert_eq!(anchored.cell_id, Some(cell));
    assert!(anchored.cell_alive, "durable cell id resolves on both peers");
    assert_eq!(anchored.author_user_id, Some(7));
    assert_eq!(anchored.quoted_text, "warrant here");
    let frontier = anchored.created_frontier.clone().expect("birth frontier");
    // H-K0 flow mirror: the birth frontier is checkout-able on both peers.
    let historical = guard.board_at_frontier(&frontier).unwrap();
    drop(guard);
    assert!(historical.sheets.iter().any(|sheet| sheet.find_cell(cell).is_some()));
    let tombstoned = anchored.messages.iter().find(|message| message.message_id == reply_id).expect("reply");
    assert!(tombstoned.deleted);
    let general = threads.iter().find(|thread| thread.comment_id == general_id).expect("general");
    assert!(general.general);
    assert!(general.cell_id.is_none());
  }
}
