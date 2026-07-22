//! Minimal repro hunt for the OPEN strikethrough-convergence bug (see
//! flow_convergence.rs header). Isolates concurrent SetCellStruck +
//! EnsureCellEditable on one cell and dumps the divergent cell projection.
#[cfg(test)]
mod tests {
  use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_collab::local_write::GateHolder;
  use flowstate_document::{InputParagraph, InputRun, RunStyles};
  use flowstate_flow::{CellId, CellSeed, FlowIntent, SheetId};
  use uuid::Uuid;

  fn paragraphs(text: &str) -> Vec<InputParagraph> {
    vec![InputParagraph {
      style: flowstate_document::PARAGRAPH_TAG,
      runs: vec![InputRun { text: text.into(), styles: RunStyles::default() }],
    }]
  }

  fn drain_updates(handle: &FlowDocHandle) -> Vec<Vec<u8>> {
    let mut guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
    guard.take_pending_publish().into_iter().map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes).collect()
  }

  fn full_mesh_sync(peers: &[FlowDocHandle]) {
    for _ in 0..6 {
      let batches: Vec<Vec<Vec<u8>>> = peers.iter().map(drain_updates).collect();
      let mut any = false;
      for (source, updates) in batches.iter().enumerate() {
        if updates.is_empty() {
          continue;
        }
        any = true;
        let slices: Vec<&[u8]> = updates.iter().map(Vec::as_slice).collect();
        for (target, peer) in peers.iter().enumerate() {
          if target != source {
            peer.import_remote_updates(&slices).unwrap();
          }
        }
      }
      if !any {
        break;
      }
    }
  }

  fn struck(handle: &FlowDocHandle, sheet: SheetId, cell: CellId) -> bool {
    handle.board_projection().unwrap().sheet(sheet).unwrap().find_cell(cell).unwrap().summary.struck
  }

  fn dump_cell(label: &str, handle: &FlowDocHandle, cell: CellId) {
    let projection = handle.cell_projection(cell).unwrap();
    eprintln!("--- {label} cell projection ---");
    eprintln!("text = {:?}", projection.text.to_string());
    for (pi, para) in projection.paragraphs.iter().enumerate() {
      eprintln!("  paragraph[{pi}] style={:?}", para.style);
      for (ri, run) in para.runs.iter().enumerate() {
        eprintln!("    run[{ri}] len={} strike={} semantic={:?}", run.len, run.styles.strikethrough, run.styles.semantic);
      }
    }
  }

  /// Sweep the order of the two ops across the two peers to find the combination
  /// that diverges.
  fn scenario(strike_on_a: bool, ensure_on_a: bool, strike_on_b: bool, ensure_on_b: bool) -> bool {
    let seed_runtime = FlowRuntime::new_empty();
    let sheet_type = seed_runtime.board().format.sheet_types[0].id;
    let (seed, _gate) = FlowDocHandle::new(seed_runtime);
    let sheet = Uuid::from_u128(1);
    seed.apply(&FlowIntent::CreateSheet { sheet_id: sheet, name: "S".into(), sheet_type_id: sheet_type }).unwrap();
    let row = Uuid::from_u128(2);
    seed.apply(&FlowIntent::InsertRows { sheet_id: sheet, before: None, row_ids: vec![row] }).unwrap();
    let column = seed.board_projection().unwrap().sheet(sheet).unwrap().columns[0].id;
    let cell = Uuid::from_u128(3);
    seed
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: cell,
        row_id: row,
        column_id: column,
        seed: CellSeed::Paragraphs(paragraphs("hello world")),
      })
      .unwrap();
    let snapshot = {
      let guard = seed.gate().lock(GateHolder::DocumentService).unwrap();
      guard.snapshot_bytes().unwrap()
    };
    let a = FlowDocHandle::new(FlowRuntime::from_snapshot_with_peer_id(&snapshot, 1).unwrap()).0;
    let b = FlowDocHandle::new(FlowRuntime::from_snapshot_with_peer_id(&snapshot, 2).unwrap()).0;

    if strike_on_a {
      a.apply(&FlowIntent::SetCellStruck { sheet_id: sheet, cell_id: cell, struck: true }).unwrap();
    }
    if ensure_on_a {
      a.apply(&FlowIntent::EnsureCellEditable { sheet_id: sheet, cell_id: cell }).unwrap();
    }
    if strike_on_b {
      b.apply(&FlowIntent::SetCellStruck { sheet_id: sheet, cell_id: cell, struck: true }).unwrap();
    }
    if ensure_on_b {
      b.apply(&FlowIntent::EnsureCellEditable { sheet_id: sheet, cell_id: cell }).unwrap();
    }

    let peers = vec![a, b];
    full_mesh_sync(&peers);
    let sa = struck(&peers[0], sheet, cell);
    let sb = struck(&peers[1], sheet, cell);
    if sa != sb {
      eprintln!("DIVERGED strike_a={strike_on_a} ensure_a={ensure_on_a} strike_b={strike_on_b} ensure_b={ensure_on_b}: a.struck={sa} b.struck={sb}");
      dump_cell("A", &peers[0], cell);
      dump_cell("B", &peers[1], cell);
    }
    sa == sb
  }

  #[test]
  fn concurrent_strike_and_ensure_editable_sweep_converges() {
    // The suspect combos: strike on one peer, ensure-editable on the other (and
    // both).
    let combos = [
      (true, false, false, true),  // A strikes, B ensures
      (false, true, true, false),  // A ensures, B strikes
      (true, true, false, true),   // A strikes+ensures, B ensures
      (true, false, true, true),   // A strikes, B strikes+ensures
      (true, true, true, true),    // both do both
    ];
    let mut all_ok = true;
    for (sa, ea, sb, eb) in combos {
      if !scenario(sa, ea, sb, eb) {
        all_ok = false;
      }
    }
    assert!(all_ok, "a strike/ensure-editable combination diverged (see stderr dump)");
  }
}
