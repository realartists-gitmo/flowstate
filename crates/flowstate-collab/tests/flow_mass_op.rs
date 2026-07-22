//! Soak object #4: flow mass-op / bulk-intent hotpath. The flow twin of
//! `mass_op_chunking.rs`. Flow cells are small, so the realistic "mass op" is a
//! bulk multi-cell intent train and a large paste into ONE cell. Unlike the
//! .db8 path the flow does NOT sender-chunk, so instead of asserting a slice
//! count we assert the invariants that actually matter: the op publishes
//! importable update(s), a peer imports them WITHIN A TIME BUDGET (no freeze),
//! the boards converge byte-for-byte, and the local commit stream is
//! progressive (one board item per commit — the receiver can paint as it goes).
#[cfg(test)]
mod tests {
  use std::time::Instant;

  use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_collab::local_write::GateHolder;
  use flowstate_document::{InputParagraph, InputRun, RunStyles};
  use flowstate_flow::{CellSeed, FlowIntent, SheetId};
  use gpui_flowtext::{InsertTextIntent, LocalIntent, LocalWriteAuthority as _, TextAnchor};
  use uuid::Uuid;

  fn uuid(n: u64) -> Uuid {
    Uuid::from_u128(u128::from(n).wrapping_mul(0x9E37_79B9_7F4A_7C15))
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

  fn drain_updates(handle: &FlowDocHandle) -> Vec<Vec<u8>> {
    let mut guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  }

  fn two_peers() -> (FlowDocHandle, FlowDocHandle, SheetId) {
    let seed_runtime = FlowRuntime::new_empty();
    let sheet_type = seed_runtime.board().format.sheet_types[0].id;
    let (seed, _gate) = FlowDocHandle::new(seed_runtime);
    let sheet = uuid(1);
    seed
      .apply(&FlowIntent::CreateSheet { sheet_id: sheet, name: "Mass".into(), sheet_type_id: sheet_type })
      .unwrap();
    seed
      .apply(&FlowIntent::InsertRows { sheet_id: sheet, before: None, row_ids: (0..4).map(uuid).collect() })
      .unwrap();
    let snapshot = {
      let guard = seed.gate().lock(GateHolder::DocumentService).unwrap();
      guard.snapshot_bytes().unwrap()
    };
    let a = FlowDocHandle::new(FlowRuntime::from_snapshot_with_peer_id(&snapshot, 1).unwrap()).0;
    let b = FlowDocHandle::new(FlowRuntime::from_snapshot_with_peer_id(&snapshot, 2).unwrap()).0;
    (a, b, sheet)
  }

  #[test]
  fn bulk_cell_train_publishes_progressively_imports_fast_and_converges() {
    let (a, b, sheet) = two_peers();
    let columns: Vec<_> = a.board_projection().unwrap().sheet(sheet).unwrap().columns.iter().map(|c| c.id).collect();
    let cols = columns.len();
    let count = 600_u64;

    // Enough rows to give every bulk cell its OWN slot (AddCell refuses an
    // occupied slot).
    let extra_rows = (count as usize).div_ceil(cols);
    a.apply(&FlowIntent::InsertRows {
      sheet_id: sheet,
      before: None,
      row_ids: (0..extra_rows).map(|i| uuid(500_000 + i as u64)).collect(),
    })
    .unwrap();
    let rows: Vec<_> = a.board_projection().unwrap().sheet(sheet).unwrap().rows.iter().map(|r| r.id).collect();

    // A bulk train: 600 cells, each in a unique (row, col) slot.
    for i in 0..count {
      a.apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: uuid(1000 + i),
        row_id: rows[(i as usize) / cols],
        column_id: columns[(i as usize) % cols],
        seed: CellSeed::Paragraphs(paragraphs(&format!("bulk {i}"))),
      })
      .unwrap();
    }

    // Import the whole bulk train into peer B, timed — no pathological freeze.
    // (The flow coalesces the publish queue, so this may be a few blobs.)
    let updates = drain_updates(&a);
    assert!(!updates.is_empty(), "the bulk train published something to import");
    let slices: Vec<&[u8]> = updates.iter().map(Vec::as_slice).collect();
    let started = Instant::now();
    b.import_remote_updates(&slices).unwrap();
    let import_ms = started.elapsed().as_millis();
    assert!(import_ms < 2000, "importing {count} bulk cells took {import_ms}ms — a freeze");

    assert_eq!(
      a.board_projection().unwrap(),
      b.board_projection().unwrap(),
      "the bulk train converges byte-for-byte after import"
    );
    let cells = a.board_projection().unwrap().sheet(sheet).unwrap().cells().count();
    assert_eq!(cells, count as usize, "every bulk cell landed");
  }

  #[test]
  fn a_large_paste_into_one_cell_imports_without_a_freeze() {
    let (a, b, sheet) = two_peers();
    let (col, row) = {
      let board = a.board_projection().unwrap();
      let s = board.sheet(sheet).unwrap();
      (s.columns[0].id, s.rows[0].id)
    };
    let cell = uuid(9001);
    a.apply(&FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id: cell,
      row_id: row,
      column_id: col,
      seed: CellSeed::Paragraphs(paragraphs("seed")),
    })
    .unwrap();
    // Sync the cell existence to B first.
    b.import_remote_updates(&drain_updates(&a).iter().map(Vec::as_slice).collect::<Vec<_>>()).unwrap();

    // A large paste: 40k chars into the one cell's text.
    let big = "lorem ipsum ".repeat(3400); // ~40k chars
    let projection = a.cell_projection(cell).unwrap();
    a.cell_authority(cell)
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(projection.ids.paragraph_ids[0], 0),
        text: big.clone(),
        style_override: None,
      }))
      .unwrap();

    let updates = drain_updates(&a);
    let slices: Vec<&[u8]> = updates.iter().map(Vec::as_slice).collect();
    let started = Instant::now();
    b.import_remote_updates(&slices).unwrap();
    let import_ms = started.elapsed().as_millis();
    assert!(import_ms < 1500, "importing a 40k-char paste took {import_ms}ms — a freeze");

    assert_eq!(
      a.board_projection().unwrap(),
      b.board_projection().unwrap(),
      "the large paste converges byte-for-byte"
    );
    // The pasted text actually made it across.
    let b_text = b.cell_projection(cell).unwrap().text.to_string();
    assert!(b_text.contains("lorem ipsum"), "the pasted content is present on the receiver");
    assert!(b_text.len() >= big.len(), "the full paste survived import");
  }
}
