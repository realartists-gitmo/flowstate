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

// ------------------------------------------------------- cell authority ----

mod cell_authority_tests {
  use super::*;
  use flowstate_flow::CellId;
  use gpui_flowtext::{
    DeleteRangeIntent, DocumentOffset, EditorSelection, FragmentBlock, InsertRichFragmentIntent, InsertTextIntent, JoinParagraphsIntent,
    LocalIntent, LocalWriteAuthority as _, LocalWriteOutcome, ReplaceMatch, ReplaceMatchesIntent, RunStyles, SetMarksIntent,
    SetParagraphStyleIntent, SplitParagraphIntent, TextAnchor, WriteRejected,
  };

  fn cell_fixture() -> (FlowDocHandle, SheetId, CellId) {
    let (handle, sheet) = handle_with_sheet();
    let cell = add_cell(&handle, sheet, CellPlacement::SheetEnd { column_index: 0 }, "seed text");
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
    {
      let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
      let _ = guard.take_pending_publish();
    }

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

    // Replace both occurrences of "line"... only one exists; replace "seed".
    authority
      .apply(LocalIntent::ReplaceMatches(ReplaceMatchesIntent {
        matches: vec![ReplaceMatch {
          start: TextAnchor::new(paragraph, 0),
          end: TextAnchor::new(paragraph, 4),
          styles: None,
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
    let updates: Vec<Vec<u8>> = {
      let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
      guard
        .take_pending_publish()
        .into_iter()
        .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
        .collect()
    };
    b.import_remote_updates(&updates.iter().map(Vec::as_slice).collect::<Vec<_>>())
      .unwrap();
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
  for index in 0..6_u128 {
    handle
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: Uuid::from_u128(0x100 + index),
        placement: CellPlacement::SheetEnd { column_index: 0 },
        seed: CellSeed::Empty,
      })
      .unwrap();
  }
  let live = handle.board_projection().unwrap();
  assert_eq!(live.sheets[0].cells.len(), 6);

  // Full replay equals the live board's structure.
  let (full, shown, total, full_frontier) = handle.history_board_at(1.0).unwrap();
  assert_eq!(shown, total);
  assert_eq!(full.sheets.len(), 1);
  assert_eq!(full.sheets[0].cells.len(), 6, "fraction 1.0 replays everything");

  // A mid-timeline replay shows a strict prefix of the cells — and the LIVE
  // board is untouched by the checkout (it ran on a fork).
  let (half, shown_half, _, half_frontier) = handle.history_board_at(0.5).unwrap();
  let half_cells = half.sheets.first().map_or(0, |sheet| sheet.cells.len());
  assert!(half_cells < 6, "fraction 0.5 must replay a strict prefix (saw {half_cells} cells)");
  assert!(shown_half < total);
  assert_eq!(
    handle.board_projection().unwrap().sheets[0].cells.len(),
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
  let cell = add_cell(&a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "alpha");
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    let _ = guard.take_pending_publish();
  }

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
  let _second_cell = add_cell(&a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "beta");

  // Rename PINS.
  let mut rename_guard = a.gate().lock(GateHolder::DocumentService).unwrap();
  rename_guard.rename_flow_checkpoint(checkpoint_id, "Before the block").unwrap();
  drop(rename_guard);

  // Restore to the checkpoint: the law demands a safety pin of the present
  // first, then a forward op.
  let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
  guard.restore_flow_frontier(&historical_frontier).unwrap();
    let board = guard.board();
    assert_eq!(board.sheets[0].cells.len(), 1, "the board reads as it did at the checkpoint");
    assert!(board.sheets[0].cells.iter().any(|c| c.id == cell));
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
  let from_a: Vec<Vec<u8>> = {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  };
  let mut guard = b.gate().lock(GateHolder::DocumentService).unwrap();
  let blobs: Vec<&[u8]> = from_a.iter().map(Vec::as_slice).collect();
    guard.import_remote_updates(&blobs).unwrap();
    assert_eq!(guard.board().sheets[0].cells.len(), 1, "the restore converges like a normal edit");
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
  assert_eq!(undo_guard.board().sheets[0].cells.len(), 2, "undo returns the pre-restore board");
  drop(undo_guard);
}

// ---- C-S2: flow comments converge like any other flow state ---------------

#[test]
fn flow_comments_converge_with_cell_anchors_tombstones_and_frontiers() {
  let (a, sheet) = handle_with_sheet();
  let cell = add_cell(&a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "warrant here");
  let snapshot = {
    let guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let (b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap());
  {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    let _ = guard.take_pending_publish();
  }

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
  let from_a: Vec<Vec<u8>> = {
    let mut guard = a.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  };
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
    assert!(historical.sheets.iter().any(|sheet| sheet.cells.iter().any(|c| c.id == cell)));
    let tombstoned = anchored.messages.iter().find(|message| message.message_id == reply_id).expect("reply");
    assert!(tombstoned.deleted);
    let general = threads.iter().find(|thread| thread.comment_id == general_id).expect("general");
    assert!(general.general);
    assert!(general.cell_id.is_none());
  }
}
