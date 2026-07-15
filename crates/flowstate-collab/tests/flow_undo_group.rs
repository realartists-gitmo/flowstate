//! Undo-group contiguity regression (S8): grouped intents must revert with
//! ONE undo on EVERY run. The excluded "meta" modified-stamp commit used to
//! land between grouped members, splitting the group whenever the wall clock
//! ticked a new timestamp between intents (a once-per-millisecond heisenbug);
//! the runtime now defers the stamp to `undo_group_end`. 500 rounds crosses
//! plenty of millisecond boundaries.
#[cfg(test)]
mod tests {
  use flowstate_collab::flow::{FlowDocHandle, FlowRuntime};
  use flowstate_document::{InputParagraph, InputRun, RunStyles};
  use flowstate_flow::{CellPlacement, CellSeed, FlowIntent};
  use uuid::Uuid;

  #[test]
  fn grouped_strikes_revert_with_one_undo_500x() {
    for round in 0..500 {
      let runtime = FlowRuntime::new_empty();
      let sheet_type = runtime.board().format.sheet_types[0].id;
      let (handle, _gate) = FlowDocHandle::new(runtime);
      let sheet = Uuid::from_u128(1);
      handle
        .apply(&FlowIntent::CreateSheet {
          sheet_id: sheet,
          name: "G".into(),
          sheet_type_id: sheet_type,
        })
        .unwrap();
      let cells = [Uuid::from_u128(2), Uuid::from_u128(3)];
      for (i, &cell) in cells.iter().enumerate() {
        handle
          .apply(&FlowIntent::AddCell {
            sheet_id: sheet,
            cell_id: cell,
            placement: CellPlacement::SheetEnd { column_index: 0 },
            seed: CellSeed::Paragraphs(vec![InputParagraph {
              style: flowstate_document::PARAGRAPH_TAG,
              runs: vec![InputRun {
                text: format!("c{i}"),
                styles: RunStyles::default(),
              }],
            }]),
          })
          .unwrap();
      }
      handle.undo_group_start().unwrap();
      for &cell in &cells {
        handle
          .apply(&FlowIntent::SetCellStruck {
            sheet_id: sheet,
            cell_id: cell,
            struck: true,
          })
          .unwrap();
      }
      handle.undo_group_end().unwrap();
      let board = handle.board_projection().unwrap();
      assert!(board.sheets[0].cells.iter().all(|c| c.summary.struck), "round {round}: strikes applied");
      assert!(handle.undo().unwrap(), "round {round}: undo available");
      let board = handle.board_projection().unwrap();
      if !board.sheets[0].cells.iter().all(|c| !c.summary.struck) {
        let still: Vec<_> = board.sheets[0]
          .cells
          .iter()
          .map(|c| c.summary.struck)
          .collect();
        let undo2 = handle.undo().unwrap();
        let board2 = handle.board_projection().unwrap();
        let after2: Vec<_> = board2.sheets[0]
          .cells
          .iter()
          .map(|c| c.summary.struck)
          .collect();
        panic!("round {round}: one undo left struck={still:?}; second undo (changed={undo2}) -> {after2:?}");
      }
    }
  }
}
