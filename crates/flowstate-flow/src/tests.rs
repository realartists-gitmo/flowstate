//! S1 gate (excel flow spec §7): grid intent behavior, snapshot round-trips,
//! the normalization law's repair units (phantom rows, D2 bump-down), the
//! rigid-body annotation law, and cross-import convergence/determinism of
//! the total materializer — including a seeded multi-peer fuzz over the full
//! intent vocabulary.

use flowstate_document::{InputParagraph, InputRun, RunStyles};
use uuid::Uuid;

use crate::{
  AnnotationOriginator, AnnotationStroke, ArgumentSide, CellId, CellSeed, ColumnId, FlowDocument, FlowDefect, FlowIntent, GridAnchor, RowId,
  SheetId, StrokePoint, StrokeRect, StrokeStyle, loro_projection, loro_schema,
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

fn column_id(document: &FlowDocument, sheet: SheetId, index: usize) -> ColumnId {
  document.projection().sheet(sheet).unwrap().columns[index].id
}

fn add_cell_with_text(document: &mut FlowDocument, sheet: SheetId, row: RowId, column: ColumnId, text: &str) -> CellId {
  let cell_id = Uuid::new_v4();
  document
    .apply_intent(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id,
      row_id: row,
      column_id: column,
      seed: CellSeed::Paragraphs(paragraphs(text)),
    })
    .unwrap();
  cell_id
}

fn row_order(document: &FlowDocument, sheet: SheetId) -> Vec<RowId> {
  document
    .projection()
    .sheet(sheet)
    .unwrap()
    .rows
    .iter()
    .map(|row| row.id)
    .collect()
}

fn cell_at(document: &FlowDocument, sheet: SheetId, row: RowId, column: ColumnId) -> Option<CellId> {
  document
    .projection()
    .sheet(sheet)
    .unwrap()
    .slot_by_ids(row, column)
    .map(|cell| cell.id)
}

fn summary_of(document: &FlowDocument, sheet: SheetId, cell: CellId) -> String {
  document
    .projection()
    .sheet(sheet)
    .unwrap()
    .find_cell(cell)
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

/// Convergence = identical projection AND identical defect report (the
/// normalizer is a pure function of canonical state).
fn assert_converged(a: &FlowDocument, b: &FlowDocument) {
  assert_eq!(a.projection(), b.projection(), "peers must materialize identical boards");
  assert_eq!(a.defects(), b.defects(), "peers must report identical defects");
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
  document.move_sheet(second, Some(first)).unwrap();
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
  document.projection().validate().unwrap();
}

#[test]
fn create_sheet_seeds_columns_from_its_type() {
  let (document, sheet) = document_with_sheet();
  let projected: Vec<String> = document
    .projection()
    .sheet(sheet)
    .unwrap()
    .columns
    .iter()
    .map(|column| column.label.clone())
    .collect();
  let expected: Vec<String> = document.projection().format.sheet_types[0]
    .columns
    .iter()
    .map(|column| column.label.clone())
    .collect();
  assert_eq!(projected, expected);
  assert!(document.projection().sheet(sheet).unwrap().rows.is_empty(), "sheets are born with zero rows");
}

#[test]
fn insert_rows_add_cells_and_summaries() {
  let (mut document, sheet) = document_with_sheet();
  let rows = document.insert_rows(sheet, None, 3).unwrap();
  assert_eq!(row_order(&document, sheet), rows);
  let col = column_id(&document, sheet, 0);
  let cell = add_cell_with_text(&mut document, sheet, rows[1], col, "Warming causes extinction");
  assert_eq!(summary_of(&document, sheet, cell), "Warming causes extinction");
  assert_eq!(cell_at(&document, sheet, rows[1], col), Some(cell));
  assert!(document.projection().sheet(sheet).unwrap().rows[0].is_empty());
  document.projection().validate().unwrap();

  // Insert before an anchor lands between existing rows.
  let inserted = document.insert_row(sheet, Some(rows[1])).unwrap();
  assert_eq!(row_order(&document, sheet), vec![rows[0], inserted, rows[1], rows[2]]);
}

#[test]
fn add_cell_rejects_occupied_and_unknown_addresses() {
  let (mut document, sheet) = document_with_sheet();
  let row = document.append_row(sheet).unwrap();
  let col = column_id(&document, sheet, 0);
  let first = document.add_cell_at(sheet, row, col);
  assert!(first.is_ok());
  assert!(document.add_cell_at(sheet, row, col).is_err(), "occupied slot must reject");
  assert!(document.add_cell_at(sheet, Uuid::new_v4(), col).is_err(), "unknown row must reject");
  assert!(document.add_cell_at(sheet, row, Uuid::new_v4()).is_err(), "unknown column must reject");
}

#[test]
fn set_cell_address_moves_without_touching_content() {
  let (mut document, sheet) = document_with_sheet();
  let rows = document.insert_rows(sheet, None, 2).unwrap();
  let col0 = column_id(&document, sheet, 0);
  let col1 = column_id(&document, sheet, 1);
  let mover = add_cell_with_text(&mut document, sheet, rows[0], col0, "mover");
  let blocker = add_cell_with_text(&mut document, sheet, rows[1], col1, "blocker");

  document.set_cell_address(sheet, mover, rows[1], col0).unwrap();
  assert_eq!(cell_at(&document, sheet, rows[1], col0), Some(mover));
  assert_eq!(summary_of(&document, sheet, mover), "mover");
  assert!(
    document.set_cell_address(sheet, mover, rows[1], col1).is_err(),
    "occupied target must reject (held by {blocker})"
  );
  // Setting a cell onto its own slot is a no-op, not a rejection.
  document.set_cell_address(sheet, mover, rows[1], col0).unwrap();
  document.projection().validate().unwrap();
}

#[test]
fn move_rows_reorders_and_delete_rows_reaps() {
  let (mut document, sheet) = document_with_sheet();
  let rows = document.insert_rows(sheet, None, 4).unwrap();
  let col = column_id(&document, sheet, 0);
  let cell = add_cell_with_text(&mut document, sheet, rows[3], col, "resident");
  document.set_row_height(sheet, rows[3], Some(120.0)).unwrap();

  document
    .move_rows(sheet, vec![rows[2], rows[3]], Some(rows[0]))
    .unwrap();
  assert_eq!(row_order(&document, sheet), vec![rows[2], rows[3], rows[0], rows[1]]);
  // To the end.
  document.move_rows(sheet, vec![rows[2]], None).unwrap();
  assert_eq!(row_order(&document, sheet), vec![rows[3], rows[0], rows[1], rows[2]]);

  document.delete_rows(sheet, vec![rows[3]]).unwrap();
  assert_eq!(row_order(&document, sheet), vec![rows[0], rows[1], rows[2]]);
  assert!(
    document.projection().sheet(sheet).unwrap().find_cell(cell).is_none(),
    "resident cells die with their row"
  );
  document.projection().validate().unwrap();
}

#[test]
fn row_height_override_set_and_clear() {
  let (mut document, sheet) = document_with_sheet();
  let row = document.append_row(sheet).unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().rows[0].height_override, None);
  document.set_row_height(sheet, row, Some(88.5)).unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().rows[0].height_override, Some(88.5));
  document.set_row_height(sheet, row, None).unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().rows[0].height_override, None);
}

#[test]
fn column_lifecycle_add_rename_move_resize_delete() {
  let (mut document, sheet) = document_with_sheet();
  let base_len = document.projection().sheet(sheet).unwrap().columns.len();
  let first = column_id(&document, sheet, 0);
  let added = document
    .add_column(sheet, "Overview", ArgumentSide::One, Some(first))
    .unwrap();
  {
    let sheet_ref = document.projection().sheet(sheet).unwrap();
    assert_eq!(sheet_ref.columns.len(), base_len + 1);
    assert_eq!(sheet_ref.columns[0].id, added);
    assert_eq!(sheet_ref.columns[0].label, "Overview");
  };
  document
    .apply_intent(&FlowIntent::RenameColumn {
      sheet_id: sheet,
      column_id: added,
      label: "OV".into(),
    })
    .unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().columns[0].label, "OV");
  document
    .apply_intent(&FlowIntent::SetColumnWidth {
      sheet_id: sheet,
      column_id: added,
      width: Some(340.0),
    })
    .unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().columns[0].width, Some(340.0));
  document
    .apply_intent(&FlowIntent::MoveColumn {
      sheet_id: sheet,
      column_id: added,
      before: None,
    })
    .unwrap();
  assert_eq!(
    document.projection().sheet(sheet).unwrap().columns.last().unwrap().id,
    added
  );

  // Resident cells die with the column.
  let row = document.append_row(sheet).unwrap();
  let resident = add_cell_with_text(&mut document, sheet, row, added, "dies");
  document
    .apply_intent(&FlowIntent::DeleteColumn {
      sheet_id: sheet,
      column_id: added,
    })
    .unwrap();
  let sheet_ref = document.projection().sheet(sheet).unwrap();
  assert_eq!(sheet_ref.columns.len(), base_len);
  assert!(sheet_ref.find_cell(resident).is_none());
  document.projection().validate().unwrap();
}

#[test]
fn the_last_column_cannot_be_deleted() {
  let mut document = FlowDocument::new();
  let sheet_type = document.projection().format.sheet_types[0].id;
  let sheet = document.create_sheet("Case", sheet_type).unwrap();
  let columns: Vec<ColumnId> = document
    .projection()
    .sheet(sheet)
    .unwrap()
    .columns
    .iter()
    .map(|column| column.id)
    .collect();
  for column in &columns[..columns.len() - 1] {
    document
      .apply_intent(&FlowIntent::DeleteColumn {
        sheet_id: sheet,
        column_id: *column,
      })
      .unwrap();
  }
  assert!(
    document
      .apply_intent(&FlowIntent::DeleteColumn {
        sheet_id: sheet,
        column_id: columns[columns.len() - 1],
      })
      .is_err()
  );
}

#[test]
fn strike_marks_text_and_toggles() {
  let (mut document, sheet) = document_with_sheet();
  let row = document.append_row(sheet).unwrap();
  let col = column_id(&document, sheet, 0);
  let cell = add_cell_with_text(&mut document, sheet, row, col, "struck arg");
  assert!(!document.projection().sheet(sheet).unwrap().find_cell(cell).unwrap().summary.struck);
  document.strike_cell(sheet, cell).unwrap();
  assert!(document.projection().sheet(sheet).unwrap().find_cell(cell).unwrap().summary.struck);
  document.strike_cell(sheet, cell).unwrap();
  assert!(!document.projection().sheet(sheet).unwrap().find_cell(cell).unwrap().summary.struck);
}

#[test]
fn undo_redo_round_trip() {
  let (mut document, sheet) = document_with_sheet();
  let row = document.append_row(sheet).unwrap();
  let col = column_id(&document, sheet, 0);
  let _cell = add_cell_with_text(&mut document, sheet, row, col, "undo me");
  assert!(document.can_undo());
  document.undo().unwrap();
  assert!(document.projection().sheet(sheet).unwrap().cells().next().is_none());
  assert!(document.can_redo());
  document.redo().unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().cells().count(), 1);
}

#[test]
fn snapshot_round_trips_identically() {
  let (mut document, sheet) = document_with_sheet();
  let rows = document.insert_rows(sheet, None, 2).unwrap();
  let col = column_id(&document, sheet, 1);
  add_cell_with_text(&mut document, sheet, rows[0], col, "persisted");
  document.set_row_height(sheet, rows[1], Some(64.0)).unwrap();
  let restored = FlowDocument::from_snapshot(&document.snapshot().unwrap()).unwrap();
  assert_eq!(document.projection(), restored.projection());
  assert_eq!(document.defects(), restored.defects());
}

#[test]
fn cell_document_exposes_durable_ids() {
  let (mut document, sheet) = document_with_sheet();
  let row = document.append_row(sheet).unwrap();
  let col = column_id(&document, sheet, 0);
  let cell = add_cell_with_text(&mut document, sheet, row, col, "ids");
  let first = document.cell_document(cell).unwrap();
  let second = document.cell_document(cell).unwrap();
  assert!(!first.ids.paragraph_ids.is_empty());
  assert_eq!(first.ids.paragraph_ids, second.ids.paragraph_ids, "registry ids are durable");
}

// ---------------------------------------------------------- convergence ----

#[test]
fn concurrent_row_reorder_never_clobbers_a_text_edit() {
  let (mut a, sheet) = document_with_sheet();
  let rows = a.insert_rows(sheet, None, 2).unwrap();
  let col = column_id(&a, sheet, 0);
  let r1 = add_cell_with_text(&mut a, sheet, rows[0], col, "one");
  let _r2 = add_cell_with_text(&mut a, sheet, rows[1], col, "two");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.move_rows(sheet, vec![rows[1]], Some(rows[0])).unwrap();
  b.apply_intent(&FlowIntent::ReplaceCellContent {
    sheet_id: sheet,
    cell_id: r1,
    paragraphs: paragraphs("one - edited"),
  })
  .unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  assert_eq!(row_order(&a, sheet), vec![rows[1], rows[0]], "the reorder survives");
  assert_eq!(summary_of(&a, sheet, r1), "one - edited", "the text edit survives");
}

#[test]
fn concurrent_same_cell_edits_merge_char_level() {
  let (mut a, sheet) = document_with_sheet();
  let row = a.append_row(sheet).unwrap();
  let col = column_id(&a, sheet, 0);
  let cell = add_cell_with_text(&mut a, sheet, row, col, "shared");
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
fn concurrent_typing_survives_a_concurrent_move() {
  // The D1 law in one test: the move is LWW registers only, so text typed
  // concurrently into the moving cell merges into the moved cell.
  let (mut a, sheet) = document_with_sheet();
  let rows = a.insert_rows(sheet, None, 2).unwrap();
  let col = column_id(&a, sheet, 0);
  let cell = add_cell_with_text(&mut a, sheet, rows[0], col, "body");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.set_cell_address(sheet, cell, rows[1], col).unwrap();
  {
    let record = loro_schema::cell_record(b.doc(), cell).unwrap();
    let flow = loro_schema::cell_flow(&record).unwrap();
    let container = flow
      .ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)
      .unwrap();
    let len = container.len_unicode();
    container.insert(len, " typed-through-move").unwrap();
    b.doc().commit();
    b.reload().unwrap();
  };

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  assert_eq!(cell_at(&a, sheet, rows[1], col), Some(cell), "the move survives");
  assert!(
    summary_of(&a, sheet, cell).contains("typed-through-move"),
    "the concurrent typing survives"
  );
}

#[test]
fn concurrent_moves_of_the_same_cell_converge_lww() {
  let (mut a, sheet) = document_with_sheet();
  let rows = a.insert_rows(sheet, None, 3).unwrap();
  let col = column_id(&a, sheet, 0);
  let cell = add_cell_with_text(&mut a, sheet, rows[0], col, "mover");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.set_cell_address(sheet, cell, rows[1], col).unwrap();
  b.set_cell_address(sheet, cell, rows[2], col).unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  let landed = a.projection().sheet(sheet).unwrap().find_cell(cell).unwrap().row_id;
  assert!(landed == rows[1] || landed == rows[2]);
  a.projection().validate().unwrap();
}

// -------------------------------------------------------------- repairs ----

#[test]
fn slot_collision_from_concurrent_adds_bumps_down_deterministically() {
  let (mut a, sheet) = document_with_sheet();
  let row = a.append_row(sheet).unwrap();
  let col = column_id(&a, sheet, 0);
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  let cell_a = a.add_cell_at(sheet, row, col).unwrap();
  let cell_b = b.add_cell_at(sheet, row, col).unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);

  let (winner, loser) = if cell_a < cell_b { (cell_a, cell_b) } else { (cell_b, cell_a) };
  assert_eq!(cell_at(&a, sheet, row, col), Some(winner), "least uuid keeps the slot");
  let bump_row = loro_projection::bump_row_id(loser, 1);
  assert_eq!(cell_at(&a, sheet, bump_row, col), Some(loser), "loser lands in its bump row");
  let order = row_order(&a, sheet);
  let contested_ix = order.iter().position(|r| *r == row).unwrap();
  assert_eq!(order.get(contested_ix + 1), Some(&bump_row), "bump row sits immediately below");
  assert!(
    a.defects()
      .iter()
      .any(|defect| matches!(defect, FlowDefect::SlotCollisionBumped { cell, .. } if *cell == loser)),
    "the bump is reported: {:?}",
    a.defects()
  );
  a.projection().validate().unwrap();
}

#[test]
fn slot_collision_from_concurrent_moves_bumps_down() {
  let (mut a, sheet) = document_with_sheet();
  let rows = a.insert_rows(sheet, None, 3).unwrap();
  let col = column_id(&a, sheet, 0);
  let first = add_cell_with_text(&mut a, sheet, rows[0], col, "first");
  let second = add_cell_with_text(&mut a, sheet, rows[1], col, "second");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.set_cell_address(sheet, first, rows[2], col).unwrap();
  b.set_cell_address(sheet, second, rows[2], col).unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  // Both cells live, in distinct rows, exactly one of them in the contested
  // slot, and the loser's row is the shared bump law's row.
  let sheet_ref = a.projection().sheet(sheet).unwrap();
  let (row_first, row_second) = (
    sheet_ref.find_cell(first).unwrap().row_id,
    sheet_ref.find_cell(second).unwrap().row_id,
  );
  assert_ne!(row_first, row_second);
  let loser = if row_first == rows[2] { second } else { first };
  assert_eq!(
    if loser == first { row_first } else { row_second },
    loro_projection::bump_row_id(loser, 1)
  );
  a.projection().validate().unwrap();
}

#[test]
fn move_into_a_concurrently_deleted_row_materializes_a_phantom() {
  let (mut a, sheet) = document_with_sheet();
  let rows = a.insert_rows(sheet, None, 2).unwrap();
  let col = column_id(&a, sheet, 0);
  let cell = add_cell_with_text(&mut a, sheet, rows[0], col, "survivor");
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  // A deletes the (empty) target row while B moves the cell into it.
  a.delete_rows(sheet, vec![rows[1]]).unwrap();
  b.set_cell_address(sheet, cell, rows[1], col).unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  let sheet_ref = a.projection().sheet(sheet).unwrap();
  let landed = sheet_ref.find_cell(cell);
  // Whichever LWW order won, the cell must be alive somewhere and any dead
  // row reference must have produced a phantom row + defect.
  let landed = landed.expect("cell survives (delete only reaps resident cells at execute time)");
  if landed.row_id == rows[1] {
    assert!(sheet_ref.row_index(rows[1]).is_some(), "phantom row materialized");
    assert!(
      a.defects()
        .iter()
        .any(|defect| matches!(defect, FlowDefect::RowMissingFromOrder { row, .. } if *row == rows[1])),
      "phantom is reported: {:?}",
      a.defects()
    );
  }
  a.projection().validate().unwrap();
}

// ---------------------------------------------------------- annotations ----

fn test_stroke(sheet: SheetId, anchor_row: RowId, anchor_col: ColumnId, originator: &str, x: f32) -> AnnotationStroke {
  AnnotationStroke {
    id: Uuid::new_v4(),
    sheet_id: sheet,
    originator: AnnotationOriginator(originator.into()),
    anchor: GridAnchor {
      row_id: anchor_row,
      column_id: anchor_col,
      offset: StrokePoint { x, y: 4.0 },
    },
    points: vec![StrokePoint { x: 0.0, y: 0.0 }, StrokePoint { x: 10.0, y: 12.0 }],
    style: StrokeStyle {
      color_rgba: 0xff00_00ff,
      width: 2.0,
      opacity: 1.0,
    },
    bbox: StrokeRect {
      min: StrokePoint { x: 0.0, y: 0.0 },
      max: StrokePoint { x: 10.0, y: 12.0 },
    },
  }
}

#[test]
fn annotations_add_clear_delete() {
  let (mut document, sheet) = document_with_sheet();
  let row = document.append_row(sheet).unwrap();
  let col = column_id(&document, sheet, 0);
  let mine = test_stroke(sheet, row, col, "me", 1.0);
  let theirs = test_stroke(sheet, row, col, "them", 2.0);
  document.add_annotation(sheet, mine.clone()).unwrap();
  document.add_annotation(sheet, theirs.clone()).unwrap();
  assert_eq!(document.projection().sheet(sheet).unwrap().annotations.len(), 2);

  // Clear is originator-scoped.
  document
    .clear_annotations(sheet, &AnnotationOriginator("me".into()))
    .unwrap();
  let remaining = &document.projection().sheet(sheet).unwrap().annotations;
  assert_eq!(remaining.len(), 1);
  assert_eq!(remaining[0].id, theirs.id);

  assert!(
    document
      .delete_annotation(sheet, theirs.id, &AnnotationOriginator("them".into()))
      .unwrap()
  );
  assert!(document.projection().sheet(sheet).unwrap().annotations.is_empty());
  assert!(
    !document
      .delete_annotation(sheet, theirs.id, &AnnotationOriginator("them".into()))
      .unwrap()
  );
}

#[test]
fn delete_sheet_sweeps_its_ink() {
  let mut document = FlowDocument::new();
  let sheet_type = document.projection().format.sheet_types[0].id;
  let doomed = document.create_sheet("Doomed", sheet_type).unwrap();
  let kept = document.create_sheet("Kept", sheet_type).unwrap();
  let doomed_row = document.append_row(doomed).unwrap();
  let kept_row = document.append_row(kept).unwrap();
  let doomed_col = column_id(&document, doomed, 0);
  let kept_col = column_id(&document, kept, 0);
  document
    .add_annotation(doomed, test_stroke(doomed, doomed_row, doomed_col, "me", 1.0))
    .unwrap();
  let survivor = test_stroke(kept, kept_row, kept_col, "me", 2.0);
  document.add_annotation(kept, survivor.clone()).unwrap();

  document.delete_sheet(doomed).unwrap();
  let map = loro_schema::annotations_map(document.doc());
  assert_eq!(loro_schema::map_keys(&map).len(), 1, "doomed sheet's strokes were swept");
  assert_eq!(document.projection().sheet(kept).unwrap().annotations[0].id, survivor.id);
}

#[test]
fn rigid_body_law_structure_changes_only_translate_ink() {
  let (mut document, sheet) = document_with_sheet();
  let rows = document.insert_rows(sheet, None, 3).unwrap();
  let col = column_id(&document, sheet, 1);
  let stroke = test_stroke(sheet, rows[1], col, "me", 3.0);
  document.add_annotation(sheet, stroke.clone()).unwrap();

  let stored = |document: &FlowDocument| document.projection().sheet(sheet).unwrap().annotations[0].clone();
  let resolved = |document: &FlowDocument| {
    let sheet_ref = document.projection().sheet(sheet).unwrap();
    sheet_ref.resolve_anchor(&sheet_ref.annotations[0].anchor)
  };

  assert_eq!(resolved(&document), (1, 1));

  // Row inserted above: the anchor slot moves; the geometry must not.
  document.insert_row(sheet, Some(rows[0])).unwrap();
  assert_eq!(resolved(&document), (2, 1), "anchor translated down one row");
  assert_eq!(stored(&document).points, stroke.points, "geometry is a constant");
  assert_eq!(stored(&document).bbox, stroke.bbox);
  assert_eq!(stored(&document).anchor, stroke.anchor);

  // Row moved: still translation only.
  document.move_rows(sheet, vec![rows[1]], None).unwrap();
  assert_eq!(resolved(&document), (3, 1), "anchor rides its row");
  assert_eq!(stored(&document).points, stroke.points);

  // Height overrides and column widths change nothing the stroke stores.
  document.set_row_height(sheet, rows[0], Some(200.0)).unwrap();
  document
    .apply_intent(&FlowIntent::SetColumnWidth {
      sheet_id: sheet,
      column_id: col,
      width: Some(340.0),
    })
    .unwrap();
  assert_eq!(stored(&document), stroke, "no structure op can deform the stroke");

  // Dead anchor row: deterministic fallback (last live row).
  document.delete_rows(sheet, vec![rows[1]]).unwrap();
  let (fallback_row, fallback_col) = resolved(&document);
  assert_eq!(fallback_col, 1);
  assert_eq!(
    fallback_row,
    document.projection().sheet(sheet).unwrap().rows.len() - 1,
    "dead anchors fall back to the last live row"
  );
  assert_eq!(stored(&document).points, stroke.points, "fallback still never deforms");
}

#[test]
fn concurrent_ink_add_vs_clear_converges() {
  let (mut a, sheet) = document_with_sheet();
  let row = a.append_row(sheet).unwrap();
  let col = column_id(&a, sheet, 0);
  a.add_annotation(sheet, test_stroke(sheet, row, col, "me", 1.0)).unwrap();
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.add_annotation(sheet, test_stroke(sheet, row, col, "me", 2.0)).unwrap();
  b.clear_annotations(sheet, &AnnotationOriginator("me".into())).unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  // The concurrent add survives the clear (write-once map semantics);
  // the pre-existing stroke is gone.
  assert_eq!(a.projection().sheet(sheet).unwrap().annotations.len(), 1);
}

#[test]
fn concurrent_sheet_delete_vs_rename_converges() {
  let mut a = FlowDocument::new();
  let sheet_type = a.projection().format.sheet_types[0].id;
  let sheet = a.create_sheet("Case", sheet_type).unwrap();
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.delete_sheet(sheet).unwrap();
  b.rename_sheet(sheet, "Renamed").unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  a.projection().validate().unwrap();
}

#[test]
fn concurrent_sheet_moves_converge() {
  let mut a = FlowDocument::new();
  let sheet_type = a.projection().format.sheet_types[0].id;
  let s1 = a.create_sheet("One", sheet_type).unwrap();
  let s2 = a.create_sheet("Two", sheet_type).unwrap();
  let s3 = a.create_sheet("Three", sheet_type).unwrap();
  let mut b = FlowDocument::from_snapshot(&a.snapshot().unwrap()).unwrap();

  a.move_sheet(s3, Some(s1)).unwrap();
  b.move_sheet(s1, None).unwrap();

  sync(&mut a, &mut b);
  assert_converged(&a, &b);
  let order: Vec<SheetId> = a.projection().sheets.iter().map(|sheet| sheet.id).collect();
  assert_eq!(order.len(), 3);
  assert!(order.contains(&s1) && order.contains(&s2) && order.contains(&s3));
}

// ------------------------------------------------------------------ fuzz ----

/// Deterministic LCG so the fuzz replays byte-identically.
struct Lcg(u64);

impl Lcg {
  fn next(&mut self) -> u64 {
    self.0 = self
      .0
      .wrapping_mul(6364136223846793005)
      .wrapping_add(1442695040888963407);
    self.0 >> 33
  }

  fn pick(&mut self, bound: usize) -> usize {
    (self.next() % bound.max(1) as u64) as usize
  }

  fn uuid(&mut self) -> Uuid {
    Uuid::from_u128((u128::from(self.next()) << 64) | u128::from(self.next()))
  }
}

fn random_intent(rng: &mut Lcg, document: &FlowDocument) -> Option<FlowIntent> {
  let board = document.projection();
  let sheet = if board.sheets.is_empty() {
    None
  } else {
    Some(&board.sheets[rng.pick(board.sheets.len())])
  };
  let action = rng.pick(16);
  match action {
    0 => Some(FlowIntent::CreateSheet {
      sheet_id: rng.uuid(),
      name: format!("Sheet {}", rng.pick(1000)),
      sheet_type_id: board.format.sheet_types[rng.pick(board.format.sheet_types.len())].id,
    }),
    1 => sheet.map(|sheet| FlowIntent::RenameSheet {
      sheet_id: sheet.id,
      name: format!("Renamed {}", rng.pick(1000)),
    }),
    2 => sheet.map(|sheet| FlowIntent::MoveSheet {
      sheet_id: sheet.id,
      before: None,
    }),
    3 => sheet.map(|sheet| FlowIntent::InsertRows {
      sheet_id: sheet.id,
      before: if sheet.rows.is_empty() || rng.pick(2) == 0 {
        None
      } else {
        Some(sheet.rows[rng.pick(sheet.rows.len())].id)
      },
      row_ids: vec![rng.uuid()],
    }),
    4 => sheet.and_then(|sheet| {
      if sheet.rows.is_empty() {
        return None;
      }
      Some(FlowIntent::MoveRows {
        sheet_id: sheet.id,
        row_ids: vec![sheet.rows[rng.pick(sheet.rows.len())].id],
        before: None,
      })
    }),
    5 => sheet.and_then(|sheet| {
      if sheet.rows.is_empty() {
        return None;
      }
      Some(FlowIntent::DeleteRows {
        sheet_id: sheet.id,
        row_ids: vec![sheet.rows[rng.pick(sheet.rows.len())].id],
      })
    }),
    6 => sheet.and_then(|sheet| {
      if sheet.rows.is_empty() {
        return None;
      }
      Some(FlowIntent::SetRowHeight {
        sheet_id: sheet.id,
        row_id: sheet.rows[rng.pick(sheet.rows.len())].id,
        height: (rng.pick(2) == 0).then(|| 40.0 + rng.pick(200) as f32),
      })
    }),
    7 => sheet.map(|sheet| FlowIntent::AddColumn {
      sheet_id: sheet.id,
      column_id: rng.uuid(),
      label: format!("Col {}", rng.pick(100)),
      side: if rng.pick(2) == 0 { ArgumentSide::One } else { ArgumentSide::Two },
      before: None,
    }),
    8 => sheet.and_then(|sheet| {
      if sheet.columns.len() < 2 {
        return None;
      }
      Some(FlowIntent::DeleteColumn {
        sheet_id: sheet.id,
        column_id: sheet.columns[rng.pick(sheet.columns.len())].id,
      })
    }),
    9 => sheet.and_then(|sheet| {
      if sheet.rows.is_empty() || sheet.columns.is_empty() {
        return None;
      }
      Some(FlowIntent::AddCell {
        sheet_id: sheet.id,
        cell_id: rng.uuid(),
        row_id: sheet.rows[rng.pick(sheet.rows.len())].id,
        column_id: sheet.columns[rng.pick(sheet.columns.len())].id,
        seed: CellSeed::Paragraphs(paragraphs(&format!("cell {}", rng.pick(1000)))),
      })
    }),
    10 => sheet.and_then(|sheet| {
      let cells: Vec<CellId> = sheet.cells().map(|cell| cell.id).collect();
      if cells.is_empty() || sheet.rows.is_empty() || sheet.columns.is_empty() {
        return None;
      }
      Some(FlowIntent::SetCellAddress {
        sheet_id: sheet.id,
        cell_id: cells[rng.pick(cells.len())],
        row_id: sheet.rows[rng.pick(sheet.rows.len())].id,
        column_id: sheet.columns[rng.pick(sheet.columns.len())].id,
      })
    }),
    11 => sheet.and_then(|sheet| {
      let cells: Vec<CellId> = sheet.cells().map(|cell| cell.id).collect();
      if cells.is_empty() {
        return None;
      }
      Some(FlowIntent::DeleteCell {
        sheet_id: sheet.id,
        cell_id: cells[rng.pick(cells.len())],
      })
    }),
    12 => sheet.and_then(|sheet| {
      let cells: Vec<CellId> = sheet.cells().map(|cell| cell.id).collect();
      if cells.is_empty() {
        return None;
      }
      Some(FlowIntent::ReplaceCellContent {
        sheet_id: sheet.id,
        cell_id: cells[rng.pick(cells.len())],
        paragraphs: paragraphs(&format!("replaced {}", rng.pick(1000))),
      })
    }),
    13 => sheet.and_then(|sheet| {
      let cells: Vec<CellId> = sheet.cells().map(|cell| cell.id).collect();
      if cells.is_empty() {
        return None;
      }
      Some(FlowIntent::SetCellStruck {
        sheet_id: sheet.id,
        cell_id: cells[rng.pick(cells.len())],
        struck: rng.pick(2) == 0,
      })
    }),
    14 => sheet.and_then(|sheet| {
      if sheet.rows.is_empty() || sheet.columns.is_empty() {
        return None;
      }
      let mut stroke = test_stroke(
        sheet.id,
        sheet.rows[rng.pick(sheet.rows.len())].id,
        sheet.columns[rng.pick(sheet.columns.len())].id,
        "fuzz",
        rng.pick(50) as f32,
      );
      stroke.id = rng.uuid();
      Some(FlowIntent::AddAnnotation { stroke })
    }),
    _ => sheet.and_then(|sheet| {
      (!sheet.rows.is_empty() && rng.pick(3) == 0).then_some(FlowIntent::DeleteSheet { sheet_id: sheet.id })
    }),
  }
}

#[test]
fn seeded_multi_peer_determinism() {
  for seed in [3_u64, 17, 4242, 90210] {
    let mut rng = Lcg(seed);
    let mut base = FlowDocument::new();
    let sheet_type = base.projection().format.sheet_types[0].id;
    base.create_sheet("Fuzz", sheet_type).unwrap();
    let snapshot = base.snapshot().unwrap();
    let mut peers = [
      FlowDocument::from_snapshot(&snapshot).unwrap(),
      FlowDocument::from_snapshot(&snapshot).unwrap(),
      FlowDocument::from_snapshot(&snapshot).unwrap(),
    ];

    for _step in 0..120 {
      let peer_ix = rng.pick(peers.len());
      if let Some(intent) = random_intent(&mut rng, &peers[peer_ix]) {
        // Rejections are legal (occupied slots, vanished targets) — the fuzz
        // exercises resolve-and-reject as much as the happy path.
        let _ = peers[peer_ix].apply_intent(&intent);
      }
      if rng.pick(6) == 0 {
        let first = rng.pick(peers.len());
        let second = rng.pick(peers.len());
        if first != second {
          let (low, high) = if first < second { (first, second) } else { (second, first) };
          let (head, tail) = peers.split_at_mut(high);
          sync(&mut head[low], &mut tail[0]);
        }
      }
    }

    // Quiescent full mesh sync, twice (second pass drains anything the first
    // pass's imports produced).
    for _round in 0..2 {
      for first in 0..peers.len() {
        for second in (first + 1)..peers.len() {
          let (head, tail) = peers.split_at_mut(second);
          sync(&mut head[first], &mut tail[0]);
        }
      }
    }

    for peer in &peers[1..] {
      assert_eq!(
        peers[0].projection(),
        peer.projection(),
        "seed {seed}: all peers must materialize the identical board"
      );
      assert_eq!(peers[0].defects(), peer.defects(), "seed {seed}: identical defect reports");
    }
    peers[0]
      .projection()
      .validate()
      .unwrap_or_else(|error| panic!("seed {seed}: normalized board must validate: {error}"));
  }
}
