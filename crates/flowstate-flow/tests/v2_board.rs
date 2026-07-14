//! .fl0 v2 schema + materializer gates (build-order step 2):
//! import→materialize identity, normalization determinism under raw
//! concurrent mutation, cycle/orphan/run-split unit laws, and the headline
//! schema property — a reorder writes only the order list and can never
//! clobber a concurrent text edit.

use flowstate_flow::format::{CellId, FlowFormat, SheetId};
use flowstate_flow::loro_projection::{FlowDefect, materialize_board, materialize_cell_rows, summary_from_rows};
use flowstate_flow::loro_schema::{
  self, cell_flow_from_paragraphs, cell_map, init_flow_document, move_cell_order, read_document_id, read_schema_version, remove_cell,
  remove_sheet, seed_cell_flow, sheet_map, write_cell, write_sheet,
};
use flowstate_flow::{decode_fl0_snapshot, encode_fl0_snapshot};
use gpui_flowtext::{InputParagraph, InputRun, ParagraphStyle, RunStyles};
use loro::{ExportMode, LoroDoc};
use uuid::Uuid;

fn new_doc(format: &FlowFormat) -> LoroDoc {
  let doc = LoroDoc::new();
  init_flow_document(&doc, format).expect("init flow doc");
  doc.commit();
  doc
}

fn fork(doc: &LoroDoc) -> LoroDoc {
  let snapshot = doc.export(ExportMode::Snapshot).expect("export snapshot");
  let forked = LoroDoc::new();
  loro_schema::configure_flow_text_styles(&forked);
  forked.import(&snapshot).expect("import snapshot");
  forked
}

fn exchange(a: &LoroDoc, b: &LoroDoc) {
  let for_b = a.export(ExportMode::updates(&b.oplog_vv())).expect("export a→b");
  let for_a = b.export(ExportMode::updates(&a.oplog_vv())).expect("export b→a");
  a.import(&for_a).expect("import into a");
  b.import(&for_b).expect("import into b");
}

fn boards_equal(a: &LoroDoc, b: &LoroDoc) {
  let (board_a, _) = materialize_board(a).expect("materialize a");
  let (board_b, _) = materialize_board(b).expect("materialize b");
  assert_eq!(board_a, board_b, "peers sharing canonical state must materialize identical boards");
}

fn run(text: &str) -> InputRun {
  InputRun {
    text: text.into(),
    styles: RunStyles::default(),
  }
}

fn paragraph(style: ParagraphStyle, runs: Vec<InputRun>) -> InputParagraph {
  InputParagraph { style, runs }
}

struct Board {
  doc: LoroDoc,
  format: FlowFormat,
  sheet: SheetId,
}

fn board_with_sheet() -> Board {
  let format = FlowFormat::policy_debate();
  let doc = new_doc(&format);
  let sheet = Uuid::new_v4();
  write_sheet(&doc, sheet, "Case", format.sheet_types[0].id, 0).expect("write sheet");
  doc.commit();
  Board { doc, format, sheet }
}

impl Board {
  fn add_cell(&self, column_index: usize, parent: Option<CellId>, order_index: usize) -> CellId {
    let cell_id = Uuid::new_v4();
    let column = self.format.sheet_types[0].columns[column_index].id;
    let cell = write_cell(&self.doc, self.sheet, cell_id, column, parent, order_index).expect("write cell");
    seed_cell_flow(&cell).expect("seed cell flow");
    self.doc.commit();
    cell_id
  }
}

#[test]
fn fresh_board_materializes_and_round_trips_through_snapshot() {
  let board = board_with_sheet();
  let root = board.add_cell(0, None, usize::MAX);
  let child = board.add_cell(1, Some(root), usize::MAX);
  let cell = cell_map(&board.doc, root).expect("cell map");
  cell_flow_from_paragraphs(
    &cell,
    &[
      paragraph(flowstate_document::PARAGRAPH_TAG, vec![run("Tag line")]),
      paragraph(ParagraphStyle::Normal, vec![run("body text")]),
    ],
  )
  .expect("write cell content");
  board.doc.commit();

  let (projection, defects) = materialize_board(&board.doc).expect("materialize");
  assert_eq!(defects, Vec::new(), "well-formed board materializes without defects");
  assert_eq!(projection.format, board.format);
  assert_eq!(projection.sheets.len(), 1);
  let sheet = &projection.sheets[0];
  assert_eq!(sheet.name, "Case");
  assert_eq!(sheet.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(), vec![root, child]);
  assert_eq!(sheet.cells[1].parent_id, Some(root));
  let summary = &sheet.cells[0].summary;
  assert_eq!(summary.summary_text.as_ref(), "Tag line");
  assert!(summary.uses_summary_projection);
  assert!(!summary.is_empty);
  // The fresh child seeded with the canonical empty tag row.
  assert!(sheet.cells[1].summary.is_empty);
  assert!(sheet.cells[1].summary.uses_summary_projection);

  // Snapshot round trip (the join path): identical board on a fresh doc.
  let restored = fork(&board.doc);
  assert_eq!(read_schema_version(&restored), Some(2));
  assert_eq!(read_document_id(&restored), read_document_id(&board.doc));
  boards_equal(&board.doc, &restored);
}

/// THE headline schema property: a reorder writes only the order list, so it
/// can never clobber a concurrent text edit in a moved cell.
#[test]
fn reorder_never_clobbers_a_concurrent_cell_edit() {
  let board = board_with_sheet();
  let first = board.add_cell(0, None, usize::MAX);
  let second = board.add_cell(0, None, usize::MAX);

  let peer_a = fork(&board.doc);
  let peer_b = fork(&board.doc);

  // Peer A moves `first` after `second` (order-list-only write).
  let sheet_a = sheet_map(&peer_a, board.sheet).expect("sheet map");
  move_cell_order(&sheet_a, 0, 1).expect("move");
  peer_a.commit();

  // Peer B concurrently replaces `first`'s text.
  let cell_b = cell_map(&peer_b, first).expect("cell map");
  cell_flow_from_paragraphs(&cell_b, &[paragraph(flowstate_document::PARAGRAPH_TAG, vec![run("surviving edit")])]).expect("edit");
  peer_b.commit();

  exchange(&peer_a, &peer_b);
  boards_equal(&peer_a, &peer_b);

  let (merged, defects) = materialize_board(&peer_a).expect("materialize merged");
  assert_eq!(defects, Vec::new());
  let sheet = &merged.sheets[0];
  assert_eq!(
    sheet.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
    vec![second, first],
    "the reorder survived"
  );
  assert_eq!(
    sheet.cell(first).expect("moved cell").summary.summary_text.as_ref(),
    "surviving edit",
    "the concurrent text edit survived the reorder"
  );
}

#[test]
fn concurrent_char_edits_in_one_cell_merge() {
  let board = board_with_sheet();
  let cell_id = board.add_cell(0, None, usize::MAX);
  let cell = cell_map(&board.doc, cell_id).expect("cell map");
  cell_flow_from_paragraphs(&cell, &[paragraph(flowstate_document::PARAGRAPH_TAG, vec![run("shared")])]).expect("seed text");
  board.doc.commit();

  let peer_a = fork(&board.doc);
  let peer_b = fork(&board.doc);
  let text_a = flowstate_flow::loro_schema::cell_flow_map(&cell_map(&peer_a, cell_id).unwrap())
    .unwrap()
    .ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)
    .unwrap();
  let text_b = flowstate_flow::loro_schema::cell_flow_map(&cell_map(&peer_b, cell_id).unwrap())
    .unwrap()
    .ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)
    .unwrap();
  // "\nshared": A prepends at the paragraph head, B appends at the tail.
  text_a.insert(1, "A-").expect("insert a");
  peer_a.commit();
  text_b.insert(7, "-B").expect("insert b");
  peer_b.commit();

  exchange(&peer_a, &peer_b);
  boards_equal(&peer_a, &peer_b);
  let rows = materialize_cell_rows(&peer_a, cell_id).expect("cell rows");
  let summary = summary_from_rows(&rows.blocks);
  assert_eq!(summary.summary_text.as_ref(), "A-shared-B", "same-cell edits interleave char-level");
}

#[test]
fn cycle_break_orphans_the_greatest_uuid_member_deterministically() {
  let board = board_with_sheet();
  let a = board.add_cell(0, None, usize::MAX);
  let b = board.add_cell(1, Some(a), usize::MAX);
  // Raw-write the cycle: a.parent = b (b is already a's child).
  let cell_a = cell_map(&board.doc, a).expect("cell a");
  loro_schema::set_cell_parent(&cell_a, Some(b)).expect("cycle write");
  board.doc.commit();

  let (projection, defects) = materialize_board(&board.doc).expect("materialize");
  let broken = a.max(b);
  assert!(
    defects
      .iter()
      .any(|defect| matches!(defect, FlowDefect::ParentCycle { cell } if *cell == broken)),
    "greatest-uuid member reported: {defects:?}"
  );
  let sheet = &projection.sheets[0];
  assert_eq!(sheet.cell(broken).expect("broken cell").parent_id, None);
  // Determinism: a fresh fork materializes the identical board.
  boards_equal(&board.doc, &fork(&board.doc));
}

#[test]
fn dangling_parent_is_orphaned() {
  let board = board_with_sheet();
  let cell_id = board.add_cell(1, None, usize::MAX);
  let cell = cell_map(&board.doc, cell_id).expect("cell map");
  loro_schema::set_cell_parent(&cell, Some(Uuid::new_v4())).expect("dangling write");
  board.doc.commit();

  let (projection, defects) = materialize_board(&board.doc).expect("materialize");
  assert!(
    defects
      .iter()
      .any(|defect| matches!(defect, FlowDefect::DanglingParent { cell } if *cell == cell_id))
  );
  assert_eq!(projection.sheets[0].cell(cell_id).unwrap().parent_id, None);
}

#[test]
fn column_adjacency_violation_orphans_the_child() {
  let board = board_with_sheet();
  let parent = board.add_cell(0, None, usize::MAX);
  // Child two columns right of its parent (adjacency requires exactly one).
  let child = board.add_cell(2, Some(parent), usize::MAX);

  let (projection, defects) = materialize_board(&board.doc).expect("materialize");
  assert!(
    defects
      .iter()
      .any(|defect| matches!(defect, FlowDefect::ColumnAdjacency { cell } if *cell == child))
  );
  assert_eq!(projection.sheets[0].cell(child).unwrap().parent_id, None);
}

#[test]
fn split_sibling_run_relinearizes_to_canonical_dfs() {
  let board = board_with_sheet();
  let parent = board.add_cell(0, None, usize::MAX);
  let first_child = board.add_cell(1, Some(parent), usize::MAX);
  let second_child = board.add_cell(1, Some(parent), usize::MAX);
  let intruder = board.add_cell(1, None, usize::MAX);
  // Raw order surgery: shove the parentless intruder between the two children.
  let sheet = sheet_map(&board.doc, board.sheet).expect("sheet map");
  move_cell_order(&sheet, 3, 2).expect("split the run");
  board.doc.commit();

  let (projection, defects) = materialize_board(&board.doc).expect("materialize");
  assert!(
    defects
      .iter()
      .any(|defect| matches!(defect, FlowDefect::SiblingRunSplit { sheet } if *sheet == board.sheet))
  );
  let cells: Vec<CellId> = projection.sheets[0].cells.iter().map(|cell| cell.id).collect();
  assert_eq!(
    cells,
    vec![parent, first_child, second_child, intruder],
    "canonical DFS keeps the subtree contiguous"
  );
  boards_equal(&board.doc, &fork(&board.doc));
}

#[test]
fn concurrent_sheet_delete_and_cell_add_converge_on_sheet_delete() {
  let board = board_with_sheet();
  board.add_cell(0, None, usize::MAX);

  let peer_a = fork(&board.doc);
  let peer_b = fork(&board.doc);
  remove_sheet(&peer_a, board.sheet).expect("remove sheet");
  peer_a.commit();
  let orphan = Uuid::new_v4();
  let column = board.format.sheet_types[0].columns[0].id;
  let cell = write_cell(&peer_b, board.sheet, orphan, column, None, usize::MAX).expect("concurrent add");
  seed_cell_flow(&cell).expect("seed");
  peer_b.commit();

  exchange(&peer_a, &peer_b);
  boards_equal(&peer_a, &peer_b);
  let (projection, defects) = materialize_board(&peer_a).expect("materialize");
  // Loro map LWW: whichever way the sheet-record race resolves, both peers
  // agree; with the delete winning the orphan cell is skipped + reported.
  if projection.sheets.is_empty() {
    assert!(
      defects
        .iter()
        .any(|defect| matches!(defect, FlowDefect::CellRecordUnknownSheet { cell } if *cell == orphan))
    );
  }
}

#[test]
fn delete_cell_removes_record_and_order() {
  let board = board_with_sheet();
  let stays = board.add_cell(0, None, usize::MAX);
  let goes = board.add_cell(0, None, usize::MAX);
  remove_cell(&board.doc, board.sheet, goes).expect("remove");
  board.doc.commit();

  let (projection, defects) = materialize_board(&board.doc).expect("materialize");
  assert_eq!(defects, Vec::new(), "a clean delete leaves no liveness defect");
  assert_eq!(
    projection.sheets[0]
      .cells
      .iter()
      .map(|cell| cell.id)
      .collect::<Vec<_>>(),
    vec![stays]
  );
}

/// Normalization determinism property: forked docs take random RAW mutations
/// (moves, parent rewrites, deletes racing adds — no invariant checked at
/// write time), cross-import, and must materialize byte-equal, defect-equal
/// boards; and materializing twice is a fixpoint.
#[test]
fn normalization_determinism_under_random_concurrent_raw_mutation() {
  let mut rng = 0x1234_5678_9abc_def0_u64;
  let mut next = move || {
    rng ^= rng << 13;
    rng ^= rng >> 7;
    rng ^= rng << 17;
    rng
  };

  for round in 0..60 {
    let board = board_with_sheet();
    let mut cells: Vec<CellId> = Vec::new();
    for _ in 0..6 {
      let column = (next() % 3) as usize;
      let parent = if column > 0 && next() % 2 == 0 && !cells.is_empty() {
        Some(cells[(next() % cells.len() as u64) as usize])
      } else {
        None
      };
      cells.push(board.add_cell(column, parent, usize::MAX));
    }

    let peers = [fork(&board.doc), fork(&board.doc), fork(&board.doc)];
    for peer in &peers {
      for _ in 0..4 {
        let choice = next() % 5;
        let victim = cells[(next() % cells.len() as u64) as usize];
        match choice {
          0 => {
            if let Some(sheet) = sheet_map(peer, board.sheet) {
              let len = loro_schema::cell_order_ids(&sheet).len();
              if len > 1 {
                let from = (next() % len as u64) as usize;
                let to = (next() % len as u64) as usize;
                let _ = move_cell_order(&sheet, from, to);
              }
            }
          },
          1 => {
            if let Some(cell) = cell_map(peer, victim) {
              let parent = cells[(next() % cells.len() as u64) as usize];
              let _ = loro_schema::set_cell_parent(&cell, Some(parent));
            }
          },
          2 => {
            if let Some(cell) = cell_map(peer, victim) {
              let column = board.format.sheet_types[0].columns[(next() % 4) as usize].id;
              let _ = loro_schema::set_cell_column(&cell, column);
            }
          },
          3 => {
            let _ = remove_cell(peer, board.sheet, victim);
          },
          _ => {
            if let Some(cell) = cell_map(peer, victim) {
              let _ = cell_flow_from_paragraphs(
                &cell,
                &[paragraph(flowstate_document::PARAGRAPH_TAG, vec![run(&format!("edit {round}"))])],
              );
            }
          },
        }
        peer.commit();
      }
    }

    // Full mesh exchange until quiescent (two rounds suffice for 3 peers).
    for _ in 0..2 {
      exchange(&peers[0], &peers[1]);
      exchange(&peers[1], &peers[2]);
      exchange(&peers[0], &peers[2]);
    }

    let materialized: Vec<_> = peers
      .iter()
      .map(|peer| materialize_board(peer).expect("materialize"))
      .collect();
    assert_eq!(materialized[0], materialized[1], "round {round}: peers 0/1 diverged");
    assert_eq!(materialized[1], materialized[2], "round {round}: peers 1/2 diverged");
    // Fixpoint: materializing again yields the identical board.
    let again = materialize_board(&peers[0]).expect("materialize again");
    assert_eq!(materialized[0].0, again.0, "round {round}: materializer is not a pure function");
  }
}

/// The old `Cell::summary_text` fixture, verbatim: mixed card + analytic
/// content projects tag/cite/undertag/analytic rows in document order.
#[test]
fn summary_projects_mixed_card_and_analytic_content_in_document_order() {
  let cite = |text: &str| {
    let mut run = run(text);
    run.styles.semantic = flowstate_document::SEMANTIC_CITE;
    run
  };
  let blocks: Vec<gpui_flowtext::InputBlock> = vec![
    gpui_flowtext::InputBlock::Paragraph(paragraph(flowstate_document::PARAGRAPH_TAG, vec![run("Tag")])),
    gpui_flowtext::InputBlock::Paragraph(paragraph(
      ParagraphStyle::Normal,
      vec![run("hidden before "), cite("Cite"), run(" hidden after")],
    )),
    gpui_flowtext::InputBlock::Paragraph(paragraph(flowstate_document::PARAGRAPH_UNDERTAG, vec![run("Undertag")])),
    gpui_flowtext::InputBlock::Paragraph(paragraph(flowstate_document::PARAGRAPH_ANALYTIC, vec![run("Analytic")])),
  ];
  let summary = summary_from_rows(&blocks);
  assert_eq!(summary.summary_text.as_ref(), "Tag\nCite\nUndertag\nAnalytic");
  assert!(summary.uses_summary_projection);
  assert!(!summary.struck);
  assert!(!summary.is_empty);
}

#[test]
fn fl0_v2_round_trips_and_v1_is_rejected() {
  let board = board_with_sheet();
  board.add_cell(0, None, usize::MAX);
  let snapshot = board.doc.export(ExportMode::Snapshot).expect("export");
  let encoded = encode_fl0_snapshot(&snapshot);
  let decoded = decode_fl0_snapshot(&encoded).expect("decode");
  let restored = LoroDoc::new();
  loro_schema::configure_flow_text_styles(&restored);
  restored.import(&decoded).expect("import");
  assert_eq!(read_schema_version(&restored), Some(2));
  boards_equal(&board.doc, &restored);

  // v1 rejection: same magic, version 1.
  let mut v1 = Vec::new();
  v1.extend_from_slice(b"FLOWFL0\0");
  v1.extend_from_slice(&1u32.to_le_bytes());
  v1.extend_from_slice(&0u64.to_le_bytes());
  let error = decode_fl0_snapshot(&v1).expect_err("v1 must be rejected");
  assert!(error.to_string().contains("never shipped"), "{error}");
}
