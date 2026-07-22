//! Soak object #3: flow network-pathology chaos. Peers edit while their update
//! packets are delivered REORDERED, DUPLICATED, and DELAYED across rounds — so
//! imports routinely arrive with missing causal deps (Loro must park them
//! pending and recover on later delivery). After a final drain to quiescence
//! every peer must converge byte-for-byte AND each live projection must equal a
//! fresh full rematerialization of its own canonical state.
//!
//! This is the flow twin of the rich-text `network_pathology.rs` (which
//! `flow_io_pump.rs` only partly covers — dup/reversed, not delay + pending).
//! It exercises the DELIVERY layer, so it drives a deliberately convergent op
//! subset (adds / rows / placement) — strike + ensure-editable are omitted (a
//! separate tracked convergence bug; see `flow_convergence.rs`).
#[cfg(test)]
mod tests {
  use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_collab::local_write::GateHolder;
  use flowstate_document::{InputParagraph, InputRun, RunStyles};
  use flowstate_flow::{CellId, CellSeed, FlowIntent, RowId, SheetId};
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
    fn uuid(&mut self) -> Uuid {
      Uuid::from_u128((u128::from(self.next()) << 64) | u128::from(self.next()))
    }
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

  fn live_cells(handle: &FlowDocHandle, sheet: SheetId) -> Vec<CellId> {
    handle.board_projection().unwrap().sheet(sheet).map(|s| s.cells().map(|c| c.id).collect()).unwrap_or_default()
  }
  fn live_rows(handle: &FlowDocHandle, sheet: SheetId) -> Vec<RowId> {
    handle.board_projection().unwrap().sheet(sheet).map(|s| s.rows.iter().map(|r| r.id).collect()).unwrap_or_default()
  }

  /// One deliberately-convergent op (no strike / ensure-editable).
  fn safe_op(rng: &mut Rng, peer: &FlowDocHandle, sheet: SheetId, tag: &str) {
    let cells = live_cells(peer, sheet);
    let rows = live_rows(peer, sheet);
    let columns: Vec<_> = peer
      .board_projection()
      .unwrap()
      .sheet(sheet)
      .map(|s| s.columns.iter().map(|c| c.id).collect::<Vec<_>>())
      .unwrap_or_default();
    if columns.is_empty() || rows.is_empty() {
      return;
    }
    // Refusals are legitimate under concurrency — we only care about delivery.
    match rng.below(5) {
      0 | 1 => {
        let _ = peer.apply(&FlowIntent::AddCell {
          sheet_id: sheet,
          cell_id: rng.uuid(),
          row_id: rows[rng.below(rows.len())],
          column_id: columns[rng.below(columns.len())],
          seed: CellSeed::Paragraphs(paragraphs(tag)),
        });
      },
      2 => {
        let _ = peer.apply(&FlowIntent::InsertRows {
          sheet_id: sheet,
          before: None,
          row_ids: vec![rng.uuid()],
        });
      },
      3 if !cells.is_empty() => {
        let _ = peer.apply(&FlowIntent::SetCellAddress {
          sheet_id: sheet,
          cell_id: cells[rng.below(cells.len())],
          row_id: rows[rng.below(rows.len())],
          column_id: columns[rng.below(columns.len())],
        });
      },
      _ if rows.len() > 1 => {
        let _ = peer.apply(&FlowIntent::MoveRows {
          sheet_id: sheet,
          row_ids: vec![rows[rng.below(rows.len())]],
          before: None,
        });
      },
      _ => {},
    }
  }

  /// Deliver every remaining publish to every peer, iterating to quiescence.
  fn drain_to_quiescence(peers: &[FlowDocHandle]) {
    for _ in 0..12 {
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

  #[test]
  fn reordered_duplicated_delayed_delivery_recovers_and_converges() {
    let rounds: usize = std::env::var("FLOWSTATE_FLOW_CHAOS_ROUNDS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(50);
    let mut rng = Rng(0xc4a05_11fe);

    let seed_runtime = FlowRuntime::new_empty();
    let sheet_type = seed_runtime.board().format.sheet_types[0].id;
    let (seed_handle, _gate) = FlowDocHandle::new(seed_runtime);
    let sheet = rng.uuid();
    seed_handle
      .apply(&FlowIntent::CreateSheet { sheet_id: sheet, name: "Chaos".into(), sheet_type_id: sheet_type })
      .unwrap();
    seed_handle
      .apply(&FlowIntent::InsertRows { sheet_id: sheet, before: None, row_ids: (0..3).map(|_| rng.uuid()).collect() })
      .unwrap();
    let snapshot = {
      let guard = seed_handle.gate().lock(GateHolder::DocumentService).unwrap();
      guard.snapshot_bytes().unwrap()
    };
    let peers: Vec<FlowDocHandle> = (1..=3)
      .map(|id| FlowDocHandle::new(FlowRuntime::from_snapshot_with_peer_id(&snapshot, id).unwrap()).0)
      .collect();

    // A packet is (target peer, update bytes). The pool is delivered out of
    // order, duplicated, and delayed.
    let mut in_flight: Vec<(usize, Vec<u8>)> = Vec::new();

    for round in 0..rounds {
      // Everyone edits a little.
      for (ix, peer) in peers.iter().enumerate() {
        for _ in 0..(1 + rng.below(3)) {
          safe_op(&mut rng, peer, sheet, &format!("r{round}p{ix}"));
        }
      }
      // Fan each fresh publish out to every OTHER peer as a pending packet.
      for (src, peer) in peers.iter().enumerate() {
        for bytes in drain_updates(peer) {
          for tgt in 0..peers.len() {
            if tgt != src {
              in_flight.push((tgt, bytes.clone()));
            }
          }
        }
      }
      // REORDER: shuffle the whole pool.
      for i in (1..in_flight.len()).rev() {
        in_flight.swap(i, rng.below(i + 1));
      }
      // Deliver a random PREFIX now; the rest stays DELAYED to a later round
      // (arriving after ops that causally depend on it — Loro parks pending).
      let deliver = if in_flight.is_empty() { 0 } else { rng.below(in_flight.len()) + 1 };
      let delivering: Vec<(usize, Vec<u8>)> = in_flight.drain(0..deliver).collect();
      for (tgt, bytes) in &delivering {
        peers[*tgt].import_remote_updates(&[bytes.as_slice()]).unwrap();
        // DUPLICATE: re-import ~1/3 of packets (idempotent-import contract).
        if rng.below(3) == 0 {
          peers[*tgt].import_remote_updates(&[bytes.as_slice()]).unwrap();
        }
      }
    }

    // Final delivery of everything still delayed, then drain to quiescence.
    for (tgt, bytes) in in_flight.drain(..) {
      peers[tgt].import_remote_updates(&[bytes.as_slice()]).unwrap();
    }
    drain_to_quiescence(&peers);

    // Oracle: every peer's live projection equals a cold rematerialization of
    // its own canonical state, and all peers agree.
    let reference = peers[0].board_projection().unwrap();
    reference.validate().expect("reference board validates");
    for (ix, peer) in peers.iter().enumerate() {
      let live = peer.board_projection().unwrap();
      let snapshot = {
        let guard = peer.gate().lock(GateHolder::DocumentService).unwrap();
        guard.snapshot_bytes().unwrap()
      };
      let cold = FlowRuntime::from_snapshot(&snapshot).unwrap();
      assert_eq!(&live, cold.board(), "peer {ix}: live projection drifted from a cold rematerialization");
      assert_eq!(reference, live, "peer {ix}: did not converge after chaotic delivery");
    }
  }
}
