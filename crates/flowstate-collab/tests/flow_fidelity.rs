//! Soak object #6: the flow fidelity firehose + board-hash drift tripwire. The
//! flow twin of the .db8 `self_check` / `flowstate-fidelity` pattern — the flow
//! runtime now emits a `Structure` pulse and a `Convergence` board-drift check
//! into the SHARED fidelity firehose on every board update (see
//! `runtime::emit_board_fidelity`), so any soak can arm the firehose and assert
//! `take_violations()` is empty. `board_hash` is the cheap `u64` drift
//! signature that makes "live == fresh rematerialization" a scalar compare.
#[cfg(test)]
mod tests {
  use flowstate_collab::flow::{FlowDocHandle, FlowRuntime};
  use flowstate_flow::{FlowIntent, board_hash};
  use uuid::Uuid;

  #[test]
  fn board_hash_is_stable_and_discriminating() {
    let (handle, _gate) = FlowDocHandle::new(FlowRuntime::new_empty());
    let sheet_type = handle.board_projection().unwrap().format.sheet_types[0].id;
    let sheet = Uuid::new_v4();
    handle
      .apply(&FlowIntent::CreateSheet { sheet_id: sheet, name: "H".into(), sheet_type_id: sheet_type })
      .unwrap();

    let before = handle.board_projection().unwrap();
    assert_eq!(board_hash(&before), board_hash(&before), "hash is deterministic");
    assert_eq!(
      board_hash(&before),
      board_hash(&handle.board_projection().unwrap()),
      "the same canonical board hashes identically"
    );

    handle
      .apply(&FlowIntent::InsertRows { sheet_id: sheet, before: None, row_ids: vec![Uuid::new_v4()] })
      .unwrap();
    let after = handle.board_projection().unwrap();
    assert_ne!(board_hash(&before), board_hash(&after), "a real mutation changes the hash");
  }

  #[test]
  fn a_clean_flow_workload_trips_no_fidelity_violations() {
    // Arms the GLOBAL fidelity gate + heavy (whole-board reproject) checks. The
    // firehose only RECORDS on an actual drift, so a concurrent clean test adds
    // no violations; this stays robust in parallel.
    unsafe {
      std::env::set_var("FLOWSTATE_TRACE_FIDELITY_HEAVY", "1");
    }
    flowstate_fidelity::set_enabled(true);
    let _ = flowstate_fidelity::take_violations(); // clear any prior

    let (handle, _gate) = FlowDocHandle::new(FlowRuntime::new_empty());
    let sheet_type = handle.board_projection().unwrap().format.sheet_types[0].id;
    let sheet = Uuid::new_v4();
    handle
      .apply(&FlowIntent::CreateSheet { sheet_id: sheet, name: "Fidelity".into(), sheet_type_id: sheet_type })
      .unwrap();
    let rows: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();
    handle
      .apply(&FlowIntent::InsertRows { sheet_id: sheet, before: None, row_ids: rows.clone() })
      .unwrap();
    let columns: Vec<_> = handle.board_projection().unwrap().sheet(sheet).unwrap().columns.iter().map(|c| c.id).collect();
    // A workload that drives the incremental AND full-rebuild paths.
    for (i, &row) in rows.iter().enumerate() {
      handle
        .apply(&FlowIntent::AddCell {
          sheet_id: sheet,
          cell_id: Uuid::new_v4(),
          row_id: row,
          column_id: columns[i % columns.len()],
          seed: flowstate_flow::CellSeed::Empty,
        })
        .unwrap();
    }
    handle
      .apply(&FlowIntent::MoveRows { sheet_id: sheet, row_ids: vec![rows[5]], before: Some(rows[0]) })
      .unwrap();
    handle
      .apply(&FlowIntent::DeleteRows { sheet_id: sheet, row_ids: vec![rows[2]] })
      .unwrap();

    let violations = flowstate_fidelity::take_violations();
    flowstate_fidelity::set_enabled(false);
    unsafe {
      std::env::remove_var("FLOWSTATE_TRACE_FIDELITY_HEAVY");
    }
    assert!(
      violations.is_empty(),
      "a clean flow workload must not trip the board-drift firehose: {violations:?}"
    );
  }
}
