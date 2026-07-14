//! S2 gate (flow architecture spec Part 6): intent behavior ports, snapshot
//! round-trips, the normalization law's repair units, and cross-import
//! convergence/determinism of the total materializer.

use flowstate_document::{InputParagraph, InputRun, RunStyles};
use uuid::Uuid;

use crate::{
  AnnotationOriginator, AnnotationStroke, BoardPoint, BoardRect, CellId, CellPlacement, CellSeed, FlowDocument, FlowDropIntent, FlowIntent,
  RelativePosition, SheetId, StrokeStyle, loro_schema,
};

fn document_with_sheet() -> (FlowDocument, SheetId) {
  let mut document = FlowDocument::new();
  let sheet_type = document.projection().format.sheet_types[0].id;
  let sheet = document.create_sheet("Case", sheet_type).unwrap();
  (document, sheet)
}

fn paragraphs(text: &str) -> Vec<InputParagraph> {
  vec![InputParagraph {
    style: flowstate_document::PARAGRAPH_TAG,
    runs: vec![InputRun {
      text: text.into(),
      styles: RunStyles::default(),
    }],
  }]
}

fn add_cell_with_text(document: &mut FlowDocument, sheet: SheetId, placement: CellPlacement, text: &str) -> CellId {
  let cell_id = Uuid::new_v4();
  document
    .apply_intent(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id,
      placement,
      seed: CellSeed::Paragraphs(paragraphs(text)),
    })
    .unwrap();
  cell_id
}

fn cell_order(document: &FlowDocument, sheet: SheetId) -> Vec<CellId> {
  document
    .projection()
    .sheet(sheet)
    .unwrap()
    .cells
    .iter()
    .map(|cell| cell.id)
    .collect()
}

fn summary_of(document: &FlowDocument, sheet: SheetId, cell: CellId) -> String {
  document
    .projection()
    .sheet(sheet)
    .unwrap()
    .cells
    .iter()
    .find(|candidate| candidate.id == cell)
    .unwrap()
    .summary
    .summary_text
    .to_string()
}

/// Cross-import both ways and reload — the convergence handshake.
fn sync(a: &mut FlowDocument, b: &mut FlowDocument) {
  let for_b = a.export_updates_for(&b.version_vector()).unwrap();
  let for_a = b.export_updates_for(&a.version_vector()).unwrap();
  a.import_updates(&for_a).unwrap();
  b.import_updates(&for_b).unwrap();
}

fn assert_converged(a: &FlowDocument, b: &FlowDocument) {
  assert_eq!(a.projection(), b.projection(), "peers must materialize identical boards");
}

// ---------------------------------------------------------------- intents --

#[test]
fn sheet_crud_and_order() {
  let mut document = FlowDocument::new();
  let sheet_type = document.projection().format.sheet_types[0].id;
  let first = document.create_sheet("Aff", sheet_type).unwrap();
  let second = document.create_sheet("Neg", sheet_type).unwrap();
  assert_eq!(
    document
      .projection()
      .sheets
      .iter()
      .map(|sheet| sheet.id)
      .collect::<Vec<_>>(),
    vec![first, second]
  );
  document.rename_sheet(second, "Negative").unwrap();
  assert_eq!(document.projection().sheet(second).unwrap().name, "Negative");
  document.move_sheet(second, 0).unwrap();
  assert_eq!(
    document
      .projection()
      .sheets
      .iter()
      .map(|sheet| sheet.id)
      .collect::<Vec<_>>(),
    vec![second, first]
  );
  document.delete_sheet(first).unwrap();
  assert_eq!(document.projection().sheets.len(), 1);
}

#[test]
fn add_cells_and_summaries() {
  let (mut document, sheet) = document_with_sheet();
  let a1 = add_cell_with_text(
    &mut document,
    sheet,
    CellPlacement::SheetEnd { column_index: 0 },
    "Warming causes extinction",
  );
  assert_eq!(summary_of(&document, sheet, a1), "Warming causes extinction");
  let b1 = add_cell_with_text(&mut document, sheet, CellPlacement::LastChildOf(a1), "Alt causes");
  let sheet_ref = document.projection().sheet(sheet).unwrap();
  let cell = &sheet_ref.cells[1];
  assert_eq!(cell.id, b1);
  assert_eq!(cell.parent_id, Some(a1));
  // Answer lives one column right.
  let columns = document.projection().sheet_column_ids(sheet).unwrap();
  assert_eq!(cell.column_id, columns[1]);
}

#[test]
fn add_sibling_before_and_after() {
  let (mut document, sheet) = document_with_sheet();
  let first = document.add_plain_cell(sheet, 0, None, None).unwrap();
  let after = document
    .add_sibling(sheet, first, RelativePosition::After)
    .unwrap();
  let before = document
    .add_sibling(sheet, first, RelativePosition::Before)
    .unwrap();
  assert_eq!(cell_order(&document, sheet), vec![before, first, after]);
}

#[test]
fn rightmost_cells_cannot_receive_responses() {
  let (mut document, sheet) = document_with_sheet();
  let columns = document.projection().sheet_column_ids(sheet).unwrap();
  let last_column = columns.len() - 1;
  let cell = document
    .add_plain_cell(sheet, last_column, None, None)
    .unwrap();
  let error = document.add_response(sheet, cell).unwrap_err();
  assert!(error.to_string().contains("rightmost"), "{error}");
}

#[test]
fn deleting_parent_orphans_children() {
  let (mut document, sheet) = document_with_sheet();
  let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
  let child = document.add_response(sheet, parent).unwrap();
  document.delete_cell(sheet, parent).unwrap();
  let sheet_ref = document.projection().sheet(sheet).unwrap();
  assert_eq!(sheet_ref.cells.len(), 1);
  assert_eq!(sheet_ref.cells[0].id, child);
  assert_eq!(sheet_ref.cells[0].parent_id, None);
}

#[test]
fn move_subtree_shifts_columns_and_reparents() {
  let (mut document, sheet) = document_with_sheet();
  let a1 = document.add_plain_cell(sheet, 0, None, None).unwrap();
  let b1 = document.add_response(sheet, a1).unwrap();
  let c1 = document.add_response(sheet, b1).unwrap();
  let a2 = document.add_plain_cell(sheet, 0, None, None).unwrap();
  document
    .move_cell_subtree(sheet, b1, FlowDropIntent::LastChildOf(a2))
    .unwrap();
  let sheet_ref = document.projection().sheet(sheet).unwrap();
  let b1_cell = sheet_ref.cells.iter().find(|cell| cell.id == b1).unwrap();
  let c1_cell = sheet_ref.cells.iter().find(|cell| cell.id == c1).unwrap();
  assert_eq!(b1_cell.parent_id, Some(a2));
  let columns = document.projection().sheet_column_ids(sheet).unwrap();
  assert_eq!(b1_cell.column_id, columns[1]);
  assert_eq!(c1_cell.column_id, columns[2]);
  assert_eq!(c1_cell.parent_id, Some(b1));
  let error = document
    .move_cell_subtree(sheet, a2, FlowDropIntent::LastChildOf(b1))
    .unwrap_err();
  assert!(error.to_string().contains("descendant"), "{error}");
}

#[test]
fn strike_marks_text_and_toggles() {
  let (mut document, sheet) = document_with_sheet();
  let cell = add_cell_with_text(&mut document, sheet, CellPlacement::SheetEnd { column_index: 0 }, "dead position");
  document.strike_cell(sheet, cell).unwrap();
  assert!(
    document.projection().sheet(sheet).unwrap().cells[0]
      .summary
      .struck
  );
  document.strike_cell(sheet, cell).unwrap();
  assert!(
    !document.projection().sheet(sheet).unwrap().cells[0]
      .summary
      .struck
  );
}

#[test]
fn annotations_add_clear_delete() {
  let (mut document, sheet) = document_with_sheet();
  let me = AnnotationOriginator("me".into());
  let stroke = AnnotationStroke {
    id: Uuid::new_v4(),
    sheet_id: sheet,
    originator: me.clone(),
    points: vec![BoardPoint { x: 1.0, y: 2.0 }],
    style: StrokeStyle {
      color_rgba: 0xff00_00ff,
      width: 2.0,
      opacity: 1.0,
    },
    bbox: BoardRect::default(),
  };
  document.add_annotation(sheet, stroke.clone()).unwrap();
  assert_eq!(
    document
      .projection()
      .sheet(sheet)
      .unwrap()
      .annotations
      .len(),
    1
  );
  assert!(document.delete_annotation(sheet, stroke.id, &me).unwrap());
  assert!(!document.delete_annotation(sheet, stroke.id, &me).unwrap());
  document.add_annotation(sheet, stroke.clone()).unwrap();
  document.clear_annotations(sheet, &me).unwrap();
  assert!(
    document
      .projection()
      .sheet(sheet)
      .unwrap()
      .annotations
      .is_empty()
  );
}

#[test]
fn undo_redo_round_trip() {
  let (mut document, sheet) = document_with_sheet();
  let cell = document.add_plain_cell(sheet, 0, None, None).unwrap();
  assert_eq!(cell_order(&document, sheet), vec![cell]);
  assert!(document.undo().unwrap());
  assert!(document.projection().sheet(sheet).unwrap().cells.is_empty());
  assert!(document.redo().unwrap());
  assert_eq!(cell_order(&document, sheet), vec![cell]);
}

// ------------------------------------------------------------ round trips --

#[test]
fn snapshot_round_trips_identically() {
  let (mut document, sheet) = document_with_sheet();
  let a1 = add_cell_with_text(&mut document, sheet, CellPlacement::SheetEnd { column_index: 0 }, "Tag line");
  let _b1 = document.add_response(sheet, a1).unwrap();
  let restored = FlowDocument::from_snapshot(&document.snapshot().unwrap()).unwrap();
  assert_eq!(document.projection(), restored.projection());
  assert_eq!(document.document_id(), restored.document_id());
  assert!(restored.defects().is_empty(), "{:?}", restored.defects());
}

#[test]
fn cell_document_exposes_durable_ids() {
  let (mut document, sheet) = document_with_sheet();
  let cell = add_cell_with_text(&mut document, sheet, CellPlacement::SheetEnd { column_index: 0 }, "Tag line");
  let projection = document.cell_document(cell).unwrap();
  assert_eq!(projection.paragraphs.len(), 1);
  assert_eq!(projection.ids.paragraph_ids.len(), 1);
  let again = document.cell_document(cell).unwrap();
  assert_eq!(
    projection.ids.paragraph_ids, again.ids.paragraph_ids,
    "durable ids must be stable across rematerializations"
  );
}

// ------------------------------------------------------------ convergence --

#[test]
fn concurrent_reorder_never_clobbers_a_text_edit() {
  let (mut a, sheet) = document_with_sheet();
  let r1 = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "one");
  let r2 = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "two");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.move_cell_subtree(sheet, r2, FlowDropIntent::BeforeSibling(r1))
    .unwrap();
  b.apply_intent(&FlowIntent::ReplaceCellContent {
    sheet_id: sheet,
    cell_id: r1,
    paragraphs: paragraphs("one - edited"),
  })
  .unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  assert_eq!(cell_order(&a, sheet), vec![r2, r1], "the reorder survives");
  assert_eq!(summary_of(&a, sheet, r1), "one - edited", "the text edit survives");
}

#[test]
fn concurrent_same_cell_edits_merge_char_level() {
  let (mut a, sheet) = document_with_sheet();
  let cell = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "shared");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  for (document, insert_at_start, text) in [(&mut a, true, "aff "), (&mut b, false, " neg")] {
    let record = loro_schema::cell_record(document.doc(), cell).unwrap();
    let flow = loro_schema::cell_flow(&record).unwrap();
    let container = flow
      .ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)
      .unwrap();
    let len = container.len_unicode();
    let pos = if insert_at_start { 1 } else { len };
    container.insert(pos, text).unwrap();
    document.doc().commit();
    document.reload().unwrap();
  }

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  let merged = summary_of(&a, sheet, cell);
  assert!(
    merged.contains("aff") && merged.contains("neg") && merged.contains("shared"),
    "char-level merge, not clobber: {merged}"
  );
}

#[test]
fn concurrent_moves_of_the_same_cell_converge() {
  let (mut a, sheet) = document_with_sheet();
  let a1 = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A1");
  let a2 = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "A2");
  let target = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "mover");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.move_cell_subtree(sheet, target, FlowDropIntent::LastChildOf(a1))
    .unwrap();
  b.move_cell_subtree(sheet, target, FlowDropIntent::LastChildOf(a2))
    .unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  let winner = a
    .projection()
    .sheet(sheet)
    .unwrap()
    .cells
    .iter()
    .find(|cell| cell.id == target)
    .unwrap()
    .parent_id;
  assert!(winner == Some(a1) || winner == Some(a2));
  a.projection().validate().unwrap();
}

// -------------------------------------------------------------- repairs ----

#[test]
fn parent_cycle_breaks_deterministically() {
  let (mut a, sheet) = document_with_sheet();
  let x = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 1 }, "X");
  let y = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 1 }, "Y");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  let poke = |document: &FlowDocument, child: CellId, parent: CellId| {
    let record = loro_schema::cell_record(document.doc(), child).unwrap();
    loro_schema::set_cell_parent(&record, Some(parent)).unwrap();
    document.doc().commit();
  };
  poke(&a, x, y);
  poke(&b, y, x);
  a.reload().unwrap();
  b.reload().unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  a.projection().validate().unwrap();
  assert!(!a.defects().is_empty(), "repairs must be reported");
}

#[test]
fn dangling_parent_is_orphaned() {
  let (mut a, sheet) = document_with_sheet();
  let parent = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "parent");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.delete_cell(sheet, parent).unwrap();
  let child = add_cell_with_text(&mut b, sheet, CellPlacement::LastChildOf(parent), "child");

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  let sheet_ref = a.projection().sheet(sheet).unwrap();
  let child_cell = sheet_ref
    .cells
    .iter()
    .find(|cell| cell.id == child)
    .unwrap();
  assert_eq!(child_cell.parent_id, None, "dangling parent must orphan");
  a.projection().validate().unwrap();
}

#[test]
fn split_sibling_run_regroups() {
  let (mut a, sheet) = document_with_sheet();
  let p1 = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "P1");
  let k1 = a.add_response(sheet, p1).unwrap();
  let _k2 = a.add_response(sheet, p1).unwrap();
  let p2 = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "P2");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  let k3 = a.add_response(sheet, p1).unwrap();
  b.move_cell_subtree(sheet, k1, FlowDropIntent::LastChildOf(p2))
    .unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  a.projection().validate().unwrap();
  let _ = k3;
}

// ----------------------------------------------------- seeded determinism --

#[test]
fn seeded_two_peer_determinism() {
  let mut rng_state: u64 = 0x5eed_f10c;
  let mut rng = move || {
    rng_state ^= rng_state << 13;
    rng_state ^= rng_state >> 7;
    rng_state ^= rng_state << 17;
    rng_state
  };

  let (mut a, sheet) = document_with_sheet();
  let seedling = add_cell_with_text(&mut a, sheet, CellPlacement::SheetEnd { column_index: 0 }, "seed");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();
  let mut known: Vec<CellId> = vec![seedling];

  for round in 0..24 {
    for (peer_ix, peer) in [&mut a, &mut b].into_iter().enumerate() {
      let roll = rng() % 5;
      let anchor = known[(rng() as usize) % known.len()];
      let anchor_live = peer
        .projection()
        .sheet(sheet)
        .unwrap()
        .cells
        .iter()
        .any(|cell| cell.id == anchor);
      let result = match roll {
        0 => {
          let id = Uuid::new_v4();
          let placement = if anchor_live {
            CellPlacement::After(anchor)
          } else {
            CellPlacement::SheetEnd { column_index: 0 }
          };
          peer
            .apply_intent(&FlowIntent::AddCell {
              sheet_id: sheet,
              cell_id: id,
              placement,
              seed: CellSeed::Paragraphs(paragraphs(&format!("cell r{round} p{peer_ix}"))),
            })
            .map(|_| known.push(id))
        },
        1 if anchor_live => peer.add_response(sheet, anchor).map(|id| known.push(id)),
        2 if anchor_live && known.len() > 3 => peer.delete_cell(sheet, anchor),
        3 if anchor_live => {
          let other = known[(rng() as usize) % known.len()];
          peer
            .move_cell_subtree(sheet, anchor, FlowDropIntent::AfterSibling(other))
            .or(Ok(()))
        },
        4 if anchor_live => peer.strike_cell(sheet, anchor),
        _ => Ok(()),
      };
      let _ = result; // individual ops may legitimately refuse
    }
    sync(&mut a, &mut b);
    assert_converged(&a, &b);
    a.projection()
      .validate()
      .unwrap_or_else(|error| panic!("round {round}: {error}"));
  }
}
