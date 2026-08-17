//! Flow architecture S5 gate, leg 2: CELL-TEXT convergence fuzz.
//! The two headline defects of the record-blob era are asserted dead:
//!   1. a reorder can never lose a concurrent keystroke (order lists and
//!      text containers are disjoint), and
//!   2. concurrent same-cell edits merge char-level (no last-writer-wins).
//!
//! Peers type/split/delete/mark inside the SAME cells through the real
//! anchored `LocalIntent` executor while other peers reorder those cells,
//! with full-mesh sync rounds; convergence is checked on full projections
//! (text + durable paragraph identity + run styling), not just summaries.
//!
//! `FLOWSTATE_FLOW_FUZZ_ROUNDS` scales to the 10k+ soak.
#[cfg(test)]
mod tests {
  use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_collab::local_write::GateHolder;
  use flowstate_document::{InputParagraph, InputRun};
  use flowstate_flow::{CellId, CellSeed, ColumnId, FlowIntent, RowId, SheetId};
  use gpui_flowtext::{
    DeleteRangeIntent, InsertTextIntent, LocalIntent, LocalWriteAuthority as _, ParagraphStyle, RunStyles, SetMarksIntent, SplitParagraphIntent,
    TextAnchor,
  };
  use uuid::Uuid;

  struct Rng(u64);
  impl Rng {
    fn next(&mut self) -> u64 {
      self.0 ^= self.0 << 13;
      self.0 ^= self.0 >> 7;
      self.0 ^= self.0 << 17;
      self.0
    }
    fn below(&mut self, bound: usize) -> usize {
      (self.next() % bound.max(1) as u64) as usize
    }

    /// Deterministic uuid so failures replay exactly.
    fn uuid(&mut self) -> Uuid {
      Uuid::from_u128((u128::from(self.next()) << 64) | u128::from(self.next()))
    }
  }

  fn drain_updates(handle: &FlowDocHandle) -> Vec<Vec<u8>> {
    let mut guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
    guard
      .take_pending_publish()
      .into_iter()
      .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
      .collect()
  }

  fn full_mesh_sync(peers: &[FlowDocHandle]) {
    for _ in 0..4 {
      let batches: Vec<Vec<Vec<u8>>> = peers.iter().map(drain_updates).collect();
      let mut any = false;
      for (source, updates) in batches.iter().enumerate() {
        if updates.is_empty() {
          continue;
        }
        any = true;
        for (target, peer) in peers.iter().enumerate() {
          if target == source {
            continue;
          }
          peer
            .import_remote_updates(&updates.iter().map(Vec::as_slice).collect::<Vec<_>>())
            .unwrap();
        }
      }
      if !any {
        break;
      }
    }
  }

  /// A random anchored position in the peer's CURRENT view of the cell:
  /// (paragraph index, durable anchor at a char boundary).
  fn random_anchor(projection: &flowstate_document::DocumentProjection, rng: &mut Rng) -> (usize, TextAnchor) {
    let paragraph = rng.below(projection.paragraphs.len());
    let range = flowstate_document::paragraph_byte_range(projection, paragraph);
    let text = projection.text.byte_slice(range).to_string();
    let boundaries: Vec<usize> = text
      .char_indices()
      .map(|(byte, _)| byte)
      .chain(std::iter::once(text.len()))
      .collect();
    let byte = boundaries[rng.below(boundaries.len())];
    (paragraph, TextAnchor::new(projection.ids.paragraph_ids[paragraph], byte))
  }

  fn ordered_anchors(projection: &flowstate_document::DocumentProjection, rng: &mut Rng) -> (TextAnchor, TextAnchor) {
    let (paragraph_a, anchor_a) = random_anchor(projection, rng);
    let (paragraph_b, anchor_b) = random_anchor(projection, rng);
    if (paragraph_a, anchor_a.byte_hint) <= (paragraph_b, anchor_b.byte_hint) {
      (anchor_a, anchor_b)
    } else {
      (anchor_b, anchor_a)
    }
  }

  fn cell_text_intent(projection: &flowstate_document::DocumentProjection, rng: &mut Rng, tag: &str) -> LocalIntent {
    match rng.below(10) {
      0..=4 => {
        let (_, at) = random_anchor(projection, rng);
        LocalIntent::InsertText(InsertTextIntent {
          at,
          text: format!("{tag}n{}", rng.below(100)),
          style_override: None,
        })
      },
      5 | 6 => {
        let (start, end) = ordered_anchors(projection, rng);
        LocalIntent::DeleteRange(DeleteRangeIntent { start, end })
      },
      7 => {
        let (_, at) = random_anchor(projection, rng);
        LocalIntent::SplitParagraph(SplitParagraphIntent {
          at,
          inherited_style: ParagraphStyle::Normal,
        })
      },
      _ => {
        let (start, end) = ordered_anchors(projection, rng);
        LocalIntent::SetMarks(SetMarksIntent {
          start,
          end,
          styles: RunStyles {
            direct_underline: rng.below(2) == 0,
            strikethrough: rng.below(2) == 0,
            ..Default::default()
          },
        })
      },
    }
  }

  fn assert_cell_projections_converged(peers: &[FlowDocHandle], cells: &[CellId], round: usize) {
    for &cell in cells {
      let reference = peers[0].cell_projection(cell).unwrap();
      for (peer_ix, peer) in peers.iter().enumerate().skip(1) {
        let theirs = peer.cell_projection(cell).unwrap();
        assert_eq!(
          reference.text.to_string(),
          theirs.text.to_string(),
          "round {round}: cell {cell} text diverged on peer {peer_ix}"
        );
        assert_eq!(
          reference.ids.paragraph_ids, theirs.ids.paragraph_ids,
          "round {round}: cell {cell} paragraph identity diverged on peer {peer_ix}"
        );
        for (index, (ours, other)) in reference
          .paragraphs
          .iter()
          .zip(theirs.paragraphs.iter())
          .enumerate()
        {
          assert_eq!(
            format!("{:?}", ours.runs),
            format!("{:?}", other.runs),
            "round {round}: cell {cell} paragraph {index} runs diverged on peer {peer_ix}"
          );
        }
      }
    }
  }

  fn seeded_board(cell_count: usize, rng: &mut Rng) -> (SheetId, Vec<CellId>, Vec<RowId>, Vec<ColumnId>, Vec<u8>) {
    let seed_runtime = FlowRuntime::new_empty();
    let sheet_type = seed_runtime.board().format.sheet_types[0].id;
    let (seed_handle, _gate) = FlowDocHandle::new(seed_runtime);
    let sheet: SheetId = rng.uuid();
    seed_handle
      .apply(&FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "TextFuzz".into(),
        sheet_type_id: sheet_type,
      })
      .unwrap();
    // Rows for every cell plus spare empties so random moves usually land.
    let rows: Vec<RowId> = (0..cell_count + 4).map(|_| rng.uuid()).collect();
    seed_handle
      .apply(&FlowIntent::InsertRows {
        sheet_id: sheet,
        before: None,
        row_ids: rows.clone(),
      })
      .unwrap();
    let columns: Vec<ColumnId> = seed_handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .unwrap()
      .columns
      .iter()
      .map(|column| column.id)
      .collect();
    let cells: Vec<CellId> = (0..cell_count).map(|_| rng.uuid()).collect();
    for (index, &cell_id) in cells.iter().enumerate() {
      seed_handle
        .apply(&FlowIntent::AddCell {
          sheet_id: sheet,
          cell_id,
          row_id: rows[index],
          column_id: columns[index % 2],
          seed: CellSeed::Paragraphs(vec![InputParagraph {
            style: flowstate_document::PARAGRAPH_TAG,
            runs: vec![InputRun {
              text: format!("seed cell {index}"),
              styles: flowstate_document::RunStyles::default(),
            }],
          }]),
        })
        .unwrap();
    }
    let snapshot = {
      let guard = seed_handle
        .gate()
        .lock(GateHolder::DocumentService)
        .unwrap();
      guard.snapshot_bytes().unwrap()
    };
    (sheet, cells, rows, columns, snapshot)
  }

  #[test]
  fn concurrent_cell_text_and_reorder_converge_char_level() {
    let rounds: usize = std::env::var("FLOWSTATE_FLOW_FUZZ_ROUNDS")
      .ok()
      .and_then(|value| value.parse().ok())
      .unwrap_or(50);
    let mut rng = Rng(0xce11_7e57_5eed);

    let (sheet, cells, rows, columns, snapshot) = seeded_board(4, &mut rng);
    let peers: Vec<FlowDocHandle> = (0..3)
      .map(|_| FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap()).0)
      .collect();
    let authorities: Vec<Vec<_>> = peers
      .iter()
      .map(|peer| {
        cells
          .iter()
          .map(|&cell| peer.cell_authority(cell))
          .collect()
      })
      .collect();

    for round in 0..rounds {
      // HEADLINE RACE, staged every round: peer 0 types into a cell while
      // peer 1 concurrently reorders that same cell — before any sync.
      let hot = rng.below(cells.len());
      let projection = peers[0].cell_projection(cells[hot]).unwrap();
      let sentinel = format!("HOT{round}x");
      let (_, at) = random_anchor(&projection, &mut rng);
      authorities[0][hot]
        .apply(LocalIntent::InsertText(InsertTextIntent {
          at,
          text: sentinel.clone(),
          style_override: None,
        }))
        .unwrap();
      // The structural race: a slot move (two LWW register writes) or a row
      // reorder, concurrent with the keystroke above. Occupied targets
      // reject — the race only needs the ATTEMPT to be concurrent.
      if rng.below(2) == 0 {
        let _ = peers[1].apply(&FlowIntent::SetCellAddress {
          sheet_id: sheet,
          cell_id: cells[hot],
          row_id: rows[rng.below(rows.len())],
          column_id: columns[rng.below(columns.len().min(3))],
        });
      } else {
        let _ = peers[1].apply(&FlowIntent::MoveRows {
          sheet_id: sheet,
          row_ids: vec![rows[rng.below(rows.len())]],
          before: None,
        });
      }
      full_mesh_sync(&peers);
      // The reorder must not have cost the keystroke.
      let merged = peers[1]
        .cell_projection(cells[hot])
        .unwrap()
        .text
        .to_string();
      assert!(
        merged.contains(&sentinel),
        "round {round}: reorder clobbered a concurrent keystroke (\"{sentinel}\" missing from {merged:?})"
      );

      // Free-for-all: every peer edits random cells (including the SAME cell
      // concurrently) through the real executors, plus occasional undo/redo.
      for (peer_ix, peer) in peers.iter().enumerate() {
        let edits = 1 + rng.below(4);
        for _ in 0..edits {
          let cell_ix = rng.below(cells.len());
          let projection = peer.cell_projection(cells[cell_ix]).unwrap();
          let intent = cell_text_intent(&projection, &mut rng, &format!("p{peer_ix}r{round}"));
          let _ = authorities[peer_ix][cell_ix].apply(intent);
        }
        if rng.below(8) == 0 {
          let _ = peer.undo();
        }
        if rng.below(16) == 0 {
          let _ = peer.redo();
        }
      }

      full_mesh_sync(&peers);

      let reference = peers[0].board_projection().unwrap();
      reference.validate().unwrap();
      for (peer_ix, peer) in peers.iter().enumerate().skip(1) {
        assert_eq!(
          reference,
          peer.board_projection().unwrap(),
          "round {round}: board diverged on peer {peer_ix}"
        );
      }
      assert_cell_projections_converged(&peers, &cells, round);
    }
  }

  /// Two peers type at the SAME anchored position concurrently — the sharpest
  /// form of the char-level-merge claim: both texts survive on both peers.
  #[test]
  fn same_offset_concurrent_typing_keeps_both_edits() {
    let mut rng = Rng(0x5a3e_0ff5_e7ed);
    let (_sheet, cells, _rows, _columns, snapshot) = seeded_board(1, &mut rng);
    let cell_id = cells[0];
    let peers: Vec<FlowDocHandle> = (0..2)
      .map(|_| FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap()).0)
      .collect();

    for (peer, text) in peers.iter().zip(["ONE", "TWO"]) {
      let projection = peer.cell_projection(cell_id).unwrap();
      peer
        .cell_authority(cell_id)
        .apply(LocalIntent::InsertText(InsertTextIntent {
          at: TextAnchor::new(projection.ids.paragraph_ids[0], 1),
          text: (*text).to_string(),
          style_override: None,
        }))
        .unwrap();
    }
    full_mesh_sync(&peers);

    let first = peers[0].cell_projection(cell_id).unwrap().text.to_string();
    let second = peers[1].cell_projection(cell_id).unwrap().text.to_string();
    assert_eq!(first, second, "peers diverged");
    assert!(
      first.contains("ONE") && first.contains("TWO"),
      "same-cell concurrent edits were clobbered instead of merged: {first:?}"
    );
  }
}
