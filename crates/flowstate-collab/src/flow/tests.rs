//! S3 gate: every intent class through the gated handle, undo ping-pong,
//! import classification (per-cell streams), and the publish contract.

use flowstate_document::{InputParagraph, InputRun, RunStyles};
use flowstate_flow::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent, SheetId};
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

fn add_cell(handle: &FlowDocHandle, sheet: SheetId, placement: CellPlacement, text: &str) -> flowstate_flow::CellId {
  let cell_id = Uuid::new_v4();
  handle
    .apply(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id,
      placement,
      seed: CellSeed::Paragraphs(paragraphs(text)),
    })
    .unwrap();
  cell_id
}

#[test]
fn every_intent_class_commits_through_the_gate() {
  let (handle, sheet) = handle_with_sheet();
  let a1 = add_cell(&handle, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A1");
  let b1 = add_cell(&handle, sheet, CellPlacement::LastChildOf(a1), "B1");
  handle
    .apply(&FlowIntent::MoveCellSubtree {
      sheet_id: sheet,
      cell_id: b1,
      drop: FlowDropIntent::RootInColumn {
        column_index: 1,
        insertion_index: 0,
      },
    })
    .unwrap();
  handle
    .apply(&FlowIntent::SetCellStruck {
      sheet_id: sheet,
      cell_id: a1,
      struck: true,
    })
    .unwrap();
  handle
    .apply(&FlowIntent::RenameSheet {
      sheet_id: sheet,
      name: "Renamed".into(),
    })
    .unwrap();
  let board = handle.board_projection().unwrap();
  assert_eq!(board.sheet(sheet).unwrap().name, "Renamed");
  assert!(
    board
      .sheet(sheet)
      .unwrap()
      .cells
      .iter()
      .any(|cell| cell.id == a1 && cell.summary.struck)
  );
  handle
    .apply(&FlowIntent::DeleteCell {
      sheet_id: sheet,
      cell_id: b1,
    })
    .unwrap();
  handle
    .apply(&FlowIntent::DeleteSheet { sheet_id: sheet })
    .unwrap();
  assert!(handle.board_projection().unwrap().sheets.is_empty());
}

#[test]
fn rejections_speak_and_leave_state_clean() {
  let (handle, sheet) = handle_with_sheet();
  let a1 = add_cell(&handle, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A1");
  let b1 = add_cell(&handle, sheet, CellPlacement::LastChildOf(a1), "B1");
  let error = handle
    .apply(&FlowIntent::MoveCellSubtree {
      sheet_id: sheet,
      cell_id: a1,
      drop: FlowDropIntent::LastChildOf(b1),
    })
    .unwrap_err();
  assert!(error.to_string().contains("descendant"), "{error}");
  let board = handle.board_projection().unwrap();
  assert_eq!(board.sheet(sheet).unwrap().cells.len(), 2, "rejected intent must not mutate");
}

#[test]
fn undo_redo_ping_pong_per_class() {
  let (handle, sheet) = handle_with_sheet();
  let a1 = add_cell(&handle, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A1");
  handle
    .apply(&FlowIntent::SetCellStruck {
      sheet_id: sheet,
      cell_id: a1,
      struck: true,
    })
    .unwrap();
  assert!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells[0]
      .summary
      .struck
  );
  assert!(handle.undo().unwrap());
  assert!(
    !handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells[0]
      .summary
      .struck
  );
  assert!(handle.redo().unwrap());
  assert!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells[0]
      .summary
      .struck
  );
  // Structural class.
  assert!(handle.undo().unwrap()); // un-strike
  assert!(handle.undo().unwrap()); // un-add A1
  assert!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells
      .is_empty()
  );
  assert!(handle.redo().unwrap());
  assert_eq!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells
      .len(),
    1
  );
}

#[test]
fn undo_grouping_is_one_member() {
  let (handle, sheet) = handle_with_sheet();
  handle.undo_group_start().unwrap();
  let a1 = add_cell(&handle, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A1");
  let _b1 = add_cell(&handle, sheet, CellPlacement::LastChildOf(a1), "B1");
  handle.undo_group_end().unwrap();
  assert_eq!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells
      .len(),
    2
  );
  assert!(handle.undo().unwrap());
  assert!(
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .cells
      .is_empty(),
    "grouped intents undo as one member"
  );
}

#[test]
fn publish_queue_carries_local_commits() {
  let (handle, sheet) = handle_with_sheet();
  let _a1 = add_cell(&handle, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A1");
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
  // Drain A's backlog up to the fork point.
  {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    let _ = guard.take_pending_publish();
  }

  let a1 = add_cell(&a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "from A");
  let b1 = add_cell(&b, sheet, CellPlacement::SheetEnd { column_index: 0 }, "from B");

  let from_a: Vec<Vec<u8>> = {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  };
  let from_b: Vec<Vec<u8>> = {
    let mut guard = b.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  };
  a.import_remote_updates(&from_b.iter().map(Vec::as_slice).collect::<Vec<_>>())
    .unwrap();
  b.import_remote_updates(&from_a.iter().map(Vec::as_slice).collect::<Vec<_>>())
    .unwrap();

  let board_a = a.board_projection().unwrap();
  let board_b = b.board_projection().unwrap();
  assert_eq!(board_a, board_b, "publish/import round converges");
  assert!(
    board_a
      .sheet(sheet)
      .unwrap()
      .cells
      .iter()
      .any(|cell| cell.id == a1)
  );
  assert!(
    board_a
      .sheet(sheet)
      .unwrap()
      .cells
      .iter()
      .any(|cell| cell.id == b1)
  );
}

#[test]
fn import_classification_feeds_per_cell_streams() {
  let (a, sheet) = handle_with_sheet();
  let cell = add_cell(&a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "shared");
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  // Clear stream backlogs on A.
  let _ = a.drain_board_stream().unwrap();
  let _ = a.drain_cell_stream(cell).unwrap();

  b.apply(&FlowIntent::ReplaceCellContent {
    sheet_id: sheet,
    cell_id: cell,
    paragraphs: paragraphs("shared — edited remotely"),
  })
  .unwrap();
  let updates: Vec<Vec<u8>> = {
    let mut guard = b.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  };
  a.import_remote_updates(&updates.iter().map(Vec::as_slice).collect::<Vec<_>>())
    .unwrap();

  let board_items = a.drain_board_stream().unwrap();
  assert!(
    board_items.iter().any(|FlowStreamItem::Board(board)| {
      board
        .sheet(sheet)
        .is_some_and(|s| s.cells[0].summary.summary_text.contains("edited remotely"))
    }),
    "board stream carries the remote change"
  );
  let cell_items = a.drain_cell_stream(cell).unwrap();
  assert!(!cell_items.is_empty(), "the edited cell's stream must receive a Replace");
}
