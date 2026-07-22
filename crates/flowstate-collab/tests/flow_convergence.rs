//! Flow architecture S5 gate, leg 1: N-peer STRUCTURAL convergence fuzz.
//! Random adds/deletes/moves/strikes/renames/annotations/columns/swaps race
//! across peers with randomized pairwise sync rounds; after every full mesh
//! sync all boards must be byte-identical, pass the validator, AND equal a
//! fresh from-snapshot materialization (materializer equivalence).
//!
//! Peer ids are FIXED (1..=3) so LWW tie-breaking is reproducible and
//! divergences are bisectable. Default rounds keep CI green;
//! `FLOWSTATE_FLOW_FUZZ_ROUNDS` scales to the deep soak.
//!
//! This soak FOUND and drove FIXES for two real convergence bugs:
//!   - SetColumnWidth was a read-before-write conditional `delete` (diverged);
//!     now an unconditional LWW register (`0.0` = auto).
//!   - EnsureCellEditable wrote the raw paragraph slot (off-by-one from the
//!     canonical `paragraph_style_value` encoding); now the canonical value.
//!
//! A third divergence was suspected here — a strikethrough `struck` split under
//! concurrent `SetCellStruck` + `EnsureCellEditable`, reported diverging near
//! round 299 at 3000 rounds. It was a downstream symptom of the EnsureCellEditable
//! off-by-one (Bug 2 above): with the canonical paragraph-style encoding fixed it
//! no longer reproduces — 500 rounds is clean and a 1200-round run completes green
//! with zero divergences. (A full 3000-round completion is just slow — the
//! per-round live-vs-cold rematerialization check grows with the board — not
//! diverging.) `FLOWSTATE_FLOW_FUZZ_ROUNDS` still scales the deep soak on demand.
#[cfg(test)]
mod tests {
  use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_collab::local_write::GateHolder;
  use flowstate_document::{InputParagraph, InputRun, RunStyles};
  use flowstate_flow::{
    AnnotationOriginator, AnnotationStroke, ArgumentSide, CellId, CellSeed, ColumnId, FlowIntent, GridAnchor, RowId, SheetId, StrokePoint,
    StrokeRect, StrokeStyle,
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

  /// Compact, reviewable divergence report: per-sheet cell tuples.
  fn board_signature(board: &flowstate_flow::FlowBoardProjection) -> Vec<String> {
    let mut lines = Vec::new();
    for sheet in &board.sheets {
      lines.push(format!(
        "sheet {} name={:?} rows={} cells={}",
        sheet.id,
        sheet.name,
        sheet.rows.len(),
        sheet.cells().count()
      ));
      for (ix, column) in sheet.columns.iter().enumerate() {
        lines.push(format!(
          "  col[{ix}] {} label={:?} side={:?} width={:?}",
          column.id, column.label, column.side, column.width
        ));
      }
      for (ix, row) in sheet.rows.iter().enumerate() {
        lines.push(format!("  row[{ix}] {} height={:?}", row.id, row.height_override));
      }
      for cell in sheet.cells() {
        lines.push(format!(
          "  cell {} col={} row={} struck={} empty={} summary={:?}",
          cell.id, cell.column_id, cell.row_id, cell.summary.struck, cell.summary.is_empty, cell.summary.summary_text
        ));
      }
      lines.push(format!("  annotations={}", sheet.annotations.len()));
    }
    lines
  }

  #[track_caller]
  fn assert_boards_equal(reference: &flowstate_flow::FlowBoardProjection, other: &flowstate_flow::FlowBoardProjection, label: &str) {
    if reference == other {
      return;
    }
    let ours = board_signature(reference);
    let theirs = board_signature(other);
    use std::fmt::Write as _;
    let mut diff = String::new();
    for i in 0..ours.len().max(theirs.len()) {
      let a = ours.get(i).map_or("<absent>", String::as_str);
      let b = theirs.get(i).map_or("<absent>", String::as_str);
      if a != b {
        let _ = writeln!(diff, "- {a}\n+ {b}");
      }
    }
    if diff.is_empty() {
      std::fs::write("/tmp/flow_div_reference.txt", format!("{reference:#?}")).ok();
      std::fs::write("/tmp/flow_div_other.txt", format!("{other:#?}")).ok();
      diff = "signatures equal; full boards written to /tmp/flow_div_{reference,other}.txt".into();
    }
    panic!("{label}: boards diverged:\n{diff}");
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

  fn full_mesh_sync(peers: &[FlowDocHandle]) {
    // Exchange rounds until quiescent (bounded): every publish reaches every
    // peer, including updates generated by imports (none today, but cheap).
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

  fn live_cells(handle: &FlowDocHandle, sheet: SheetId) -> Vec<CellId> {
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .map(|sheet| sheet.cells().map(|cell| cell.id).collect())
      .unwrap_or_default()
  }

  fn live_rows(handle: &FlowDocHandle, sheet: SheetId) -> Vec<RowId> {
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .map(|sheet| sheet.rows.iter().map(|row| row.id).collect())
      .unwrap_or_default()
  }

  fn live_columns(handle: &FlowDocHandle, sheet: SheetId) -> Vec<ColumnId> {
    handle
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .map(|sheet| sheet.columns.iter().map(|column| column.id).collect())
      .unwrap_or_default()
  }

  #[test]
  fn n_peer_structural_convergence() {
    let rounds: usize = std::env::var("FLOWSTATE_FLOW_FUZZ_ROUNDS")
      .ok()
      .and_then(|value| value.parse().ok())
      .unwrap_or(60);
    let mut rng = Rng(0xf10c_5eed_0001);

    let seed_runtime = FlowRuntime::new_empty();
    let sheet_type = seed_runtime.board().format.sheet_types[0].id;
    let (seed_handle, _gate) = FlowDocHandle::new(seed_runtime);
    let sheet = rng.uuid();
    seed_handle
      .apply(&FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "Fuzz".into(),
        sheet_type_id: sheet_type,
      })
      .unwrap();
    seed_handle
      .apply(&FlowIntent::InsertRows {
        sheet_id: sheet,
        before: None,
        row_ids: (0..4).map(|_| rng.uuid()).collect(),
      })
      .unwrap();
    let snapshot = {
      let guard = seed_handle
        .gate()
        .lock(GateHolder::DocumentService)
        .unwrap();
      guard.snapshot_bytes().unwrap()
    };
    // Fixed peer ids so LWW tie-breaking is reproducible across runs (a random
    // peer id makes divergences intermittent).
    let peers: Vec<FlowDocHandle> = (1..=3)
      .map(|peer_id| FlowDocHandle::new(FlowRuntime::from_snapshot_with_peer_id(&snapshot, peer_id).unwrap()).0)
      .collect();

    let mut minted = 0_usize;
    for round in 0..rounds {
      for (peer_ix, peer) in peers.iter().enumerate() {
        let ops = 1 + rng.below(3);
        for _ in 0..ops {
          let cells = live_cells(peer, sheet);
          let rows = live_rows(peer, sheet);
          let columns = live_columns(peer, sheet);
          let roll = rng.below(18);
          let result: Result<(), String> = match roll {
            0 | 1 if !rows.is_empty() && !columns.is_empty() => {
              minted += 1;
              peer
                .apply(&FlowIntent::AddCell {
                  sheet_id: sheet,
                  cell_id: rng.uuid(),
                  row_id: rows[rng.below(rows.len())],
                  column_id: columns[rng.below(columns.len())],
                  seed: CellSeed::Paragraphs(paragraphs(&format!("r{round} p{peer_ix} c{minted}"))),
                })
                .map(|_| ())
                .map_err(|error| error.to_string())
            },
            2 => peer
              .apply(&FlowIntent::InsertRows {
                sheet_id: sheet,
                before: if rows.is_empty() || rng.below(2) == 0 {
                  None
                } else {
                  Some(rows[rng.below(rows.len())])
                },
                row_ids: vec![rng.uuid()],
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            3 if cells.len() > 2 => peer
              .apply(&FlowIntent::DeleteCell {
                sheet_id: sheet,
                cell_id: cells[rng.below(cells.len())],
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            4 if cells.len() > 1 && !rows.is_empty() && !columns.is_empty() => peer
              .apply(&FlowIntent::SetCellAddress {
                sheet_id: sheet,
                cell_id: cells[rng.below(cells.len())],
                row_id: rows[rng.below(rows.len())],
                column_id: columns[rng.below(columns.len())],
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            5 if !cells.is_empty() => peer
              .apply(&FlowIntent::SetCellStruck {
                sheet_id: sheet,
                cell_id: cells[rng.below(cells.len())],
                struck: rng.below(2) == 0,
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            6 if !columns.is_empty() => peer
              .apply(&FlowIntent::AddAnnotation {
                stroke: AnnotationStroke {
                  id: rng.uuid(),
                  sheet_id: sheet,
                  originator: AnnotationOriginator(format!("peer-{peer_ix}")),
                  anchor: GridAnchor {
                    row_id: rows.first().copied().unwrap_or_else(Uuid::nil),
                    column_id: columns[rng.below(columns.len())],
                    offset: StrokePoint {
                      x: rng.below(200) as f32,
                      y: rng.below(60) as f32,
                    },
                  },
                  points: vec![
                    StrokePoint { x: 0.0, y: 0.0 },
                    StrokePoint {
                      x: rng.below(120) as f32,
                      y: rng.below(120) as f32,
                    },
                  ],
                  style: StrokeStyle {
                    color_rgba: 0xff33_3333,
                    width: 2.0,
                    opacity: 1.0,
                  },
                  bbox: StrokeRect::default(),
                },
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            7 if rows.len() > 1 => peer
              .apply(&FlowIntent::MoveRows {
                sheet_id: sheet,
                row_ids: vec![rows[rng.below(rows.len())]],
                before: if rng.below(2) == 0 {
                  None
                } else {
                  Some(rows[rng.below(rows.len())])
                },
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            8 if rows.len() > 2 => peer
              .apply(&FlowIntent::DeleteRows {
                sheet_id: sheet,
                row_ids: vec![rows[rng.below(rows.len())]],
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            9 if !rows.is_empty() && rng.below(2) == 0 => peer
              .apply(&FlowIntent::SetRowHeight {
                sheet_id: sheet,
                row_id: rows[rng.below(rows.len())],
                height: (rng.below(2) == 0).then(|| 40.0 + rng.below(160) as f32),
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            10 if cells.len() > 1 => {
              let a = cells[rng.below(cells.len())];
              let b = cells[rng.below(cells.len())];
              if a == b {
                Ok(())
              } else {
                peer
                  .apply(&FlowIntent::SwapCells { sheet_id: sheet, a, b })
                  .map(|_| ())
                  .map_err(|error| error.to_string())
              }
            },
            11 if !cells.is_empty() && !rows.is_empty() && !columns.is_empty() => {
              let count = 1 + rng.below(3);
              let placements = (0..count)
                .map(|_| {
                  (
                    cells[rng.below(cells.len())],
                    rows[rng.below(rows.len())],
                    columns[rng.below(columns.len())],
                  )
                })
                .collect();
              peer
                .apply(&FlowIntent::SetCellAddresses { sheet_id: sheet, placements })
                .map(|_| ())
                .map_err(|error| error.to_string())
            },
            12 if !cells.is_empty() => peer
              .apply(&FlowIntent::EnsureCellEditable {
                sheet_id: sheet,
                cell_id: cells[rng.below(cells.len())],
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            13 => peer
              .apply(&FlowIntent::AddColumn {
                sheet_id: sheet,
                column_id: rng.uuid(),
                label: format!("Col r{round}"),
                side: if rng.below(2) == 0 { ArgumentSide::One } else { ArgumentSide::Two },
                before: if columns.is_empty() || rng.below(2) == 0 {
                  None
                } else {
                  Some(columns[rng.below(columns.len())])
                },
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            14 if columns.len() > 2 => peer
              .apply(&FlowIntent::DeleteColumn {
                sheet_id: sheet,
                column_id: columns[rng.below(columns.len())],
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            15 if !columns.is_empty() => peer
              .apply(&FlowIntent::RenameColumn {
                sheet_id: sheet,
                column_id: columns[rng.below(columns.len())],
                label: format!("C{}", rng.below(1000)),
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            16 if columns.len() > 1 => peer
              .apply(&FlowIntent::MoveColumn {
                sheet_id: sheet,
                column_id: columns[rng.below(columns.len())],
                before: if rng.below(2) == 0 {
                  None
                } else {
                  Some(columns[rng.below(columns.len())])
                },
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            17 if !columns.is_empty() => peer
              .apply(&FlowIntent::SetColumnWidth {
                sheet_id: sheet,
                column_id: columns[rng.below(columns.len())],
                width: (rng.below(2) == 0).then(|| 90.0 + rng.below(400) as f32),
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
            _ => peer
              .apply(&FlowIntent::RenameSheet {
                sheet_id: sheet,
                name: format!("Fuzz r{round}"),
              })
              .map(|_| ())
              .map_err(|error| error.to_string()),
          };
          let _ = result; // refusals are legitimate under concurrency
        }
        // Occasional local undo keeps the undo stack in the mix.
        if rng.below(10) == 0 {
          let _ = peer.undo();
        }
      }

      full_mesh_sync(&peers);

      let reference = peers[0].board_projection().unwrap();
      reference
        .validate()
        .unwrap_or_else(|error| panic!("round {round}: normalized board failed validation: {error}"));
      // Cross-peer convergence: every peer's live board agrees with peer 0's.
      for (peer_ix, peer) in peers.iter().enumerate().skip(1) {
        assert_boards_equal(&reference, &peer.board_projection().unwrap(), &format!("round {round}: peer {peer_ix} diverged"));
      }
      // Per-peer self-consistency (the incremental-cache staleness oracle): each
      // peer's LIVE board (built through the incremental summary cache + touch
      // dirtying) must equal a COLD rematerialization of its own canonical state
      // (dirty=None → every summary re-derived). A mismatch means the peer's
      // cache went stale WITHOUT the peer diverging from anyone — which is how
      // the strikethrough/re-mint bug hid for ~300 rounds (only peer 0 was
      // cold-checked before, so a stale peer 2 slipped through until it happened
      // to disagree with peer 0 on the same cell). Checking EVERY peer catches
      // it the round the cache is first reused stale.
      for (peer_ix, peer) in peers.iter().enumerate() {
        let live = peer.board_projection().unwrap();
        let snapshot = {
          let guard = peer.gate().lock(GateHolder::DocumentService).unwrap();
          guard.snapshot_bytes().unwrap()
        };
        let cold = FlowRuntime::from_snapshot(&snapshot).unwrap();
        assert_boards_equal(&live, cold.board(), &format!("round {round}: peer {peer_ix} incremental board != cold rematerialization (stale summary cache)"));
      }
    }
  }
}
