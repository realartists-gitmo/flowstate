//! Flow architecture S6 gate: the flow I/O pump harness — `doc_io_pump`'s
//! mirror over [`FlowIoHandle`]. The properties under guard are the same
//! delivery-surface laws: §6.4 import coalescing bounded behind a held gate
//! (plus the P1.A second drain), deferred non-import ordering, exactly-once
//! publish draining, and convergence under duplicated/reversed delivery.
#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use anyhow::Result;
  use flowstate_collab::flow::{FlowDocHandle, FlowIoHandle, FlowPublishEvent, FlowRuntime, WriteGate};
  use flowstate_collab::local_write::GateHolder;
  use flowstate_document::{InputParagraph, InputRun, RunStyles};
  use flowstate_flow::{CellId, CellSeed, FlowIntent};
  use gpui_flowtext::{InsertTextIntent, LocalIntent, LocalWriteAuthority as _, TextAnchor};
  use uuid::Uuid;

  type FlowGate = Arc<WriteGate<FlowRuntime>>;

  fn seed_paragraphs(text: &str) -> Vec<InputParagraph> {
    vec![InputParagraph {
      style: flowstate_document::PARAGRAPH_TAG,
      runs: vec![InputRun {
        text: text.into(),
        styles: RunStyles::default(),
      }],
    }]
  }

  /// A fully-based (source, receiver) pair over ONE shared seeded board —
  /// the receiver is minted from the source's snapshot, so the dripped delta
  /// blobs' deps are always satisfiable — plus `edits` sequential cell-text
  /// inserts exported as one delta blob PER EDIT (the network drip shape),
  /// the source's final cell text, and the drip target cell.
  fn drip_pair(edits: usize) -> Result<(String, Vec<Vec<u8>>, FlowGate, CellId)> {
    let source_runtime = FlowRuntime::new_empty();
    let sheet_type = source_runtime.board().format.sheet_types[0].id;
    let (source_handle, source_gate) = FlowDocHandle::new(source_runtime);
    let sheet = Uuid::from_u128(0xf10c_0001);
    source_handle
      .apply(&FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "Drip".into(),
        sheet_type_id: sheet_type,
      })
      .map_err(|error| anyhow::anyhow!("seed sheet rejected: {error}"))?;
    let row = Uuid::from_u128(0xf10c_1002);
    source_handle
      .apply(&FlowIntent::InsertRows {
        sheet_id: sheet,
        before: None,
        row_ids: vec![row],
      })
      .map_err(|error| anyhow::anyhow!("seed row rejected: {error}"))?;
    let column = source_handle
      .board_projection()
      .map_err(|error| anyhow::anyhow!("board unavailable: {error}"))?
      .sheet(sheet)
      .expect("seed sheet")
      .columns[0]
      .id;
    let cell: CellId = Uuid::from_u128(0xf10c_0002);
    source_handle
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: cell,
        row_id: row,
        column_id: column,
        seed: CellSeed::Paragraphs(seed_paragraphs("drip seed")),
      })
      .map_err(|error| anyhow::anyhow!("seed cell rejected: {error}"))?;

    let snapshot = {
      let guard = source_gate
        .lock(GateHolder::DocumentService)
        .expect("gate healthy");
      guard.snapshot_bytes()?
    };
    let (_receiver_handle, receiver_gate) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot)?);
    // The seed commits are already inside the receiver's snapshot; drain them
    // from the publish queue so the drip is exactly the edit deltas.
    {
      let mut guard = source_gate
        .lock(GateHolder::ExportUpdates)
        .expect("gate healthy");
      let _ = guard.take_pending_publish();
    }

    let authority = source_handle.cell_authority(cell);
    let mut blobs = Vec::with_capacity(edits);
    for edit_ix in 0..edits {
      let projection = source_handle
        .cell_projection(cell)
        .map_err(|error| anyhow::anyhow!("cell projection unavailable: {error}"))?;
      let last = projection.paragraphs.len() - 1;
      let range = flowstate_document::paragraph_byte_range(&projection, last);
      authority
        .apply(LocalIntent::InsertText(InsertTextIntent {
          at: TextAnchor::new(projection.ids.paragraph_ids[last], range.end - range.start),
          text: format!("{}", edit_ix % 10),
          style_override: None,
        }))
        .map_err(|error| anyhow::anyhow!("drip edit {edit_ix} rejected: {error:?}"))?;
      let mut exported: Vec<Vec<u8>> = {
        let mut guard = source_gate
          .lock(GateHolder::ExportUpdates)
          .expect("gate healthy");
        guard
          .take_pending_publish()
          .into_iter()
          .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
          .collect()
      };
      anyhow::ensure!(
        exported.len() == 1,
        "edit {edit_ix} published {} blobs, expected exactly 1",
        exported.len()
      );
      blobs.push(exported.remove(0));
    }
    let final_text = source_handle
      .cell_projection(cell)
      .map_err(|error| anyhow::anyhow!("final projection unavailable: {error}"))?
      .text
      .to_string();
    Ok((final_text, blobs, receiver_gate, cell))
  }

  fn cell_text(gate: &FlowGate, cell: CellId) -> String {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
    guard
      .cell_projection(cell)
      .expect("cell projection")
      .text
      .to_string()
  }

  fn import_batches(gate: &FlowGate) -> u64 {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
    guard.import_batches_served()
  }

  /// A 40-blob drip fired while the gate is HELD must coalesce into a few
  /// batched imports (≤16 blobs per §6.4 chunk, folded further by the P1.A
  /// second drain), every reply must arrive, and the receiver must converge.
  #[test]
  fn held_gate_drip_coalesces_into_bounded_batches() -> Result<()> {
    const EDITS: usize = 40;
    let (source_text, blobs, gate, cell) = drip_pair(EDITS)?;
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let batches_before = import_batches(&gate);
    // Hold the gate so the io thread blocks on its FIRST import; every blob
    // sent meanwhile queues behind it and must fold into few batches.
    let hold = gate
      .lock(GateHolder::DocumentService)
      .expect("gate healthy");
    // One OS thread per blob: async fns are LAZY, so sequentially awaiting
    // them would send each request only after the previous replied (zero
    // queue depth, zero coalescing). Independent blocked threads enqueue all
    // 40 requests concurrently behind the held gate — the real network shape.
    #[allow(clippy::needless_collect, reason = "spawn-all-then-join-all; lazy iteration would serialize the burst")]
    let senders: Vec<_> = blobs
      .iter()
      .map(|blob| {
        let io_import = io.clone();
        let blob = blob.clone();
        std::thread::spawn(move || pollster::block_on(io_import.import_remote_update(blob)))
      })
      .collect();
    // Give every sender time to enqueue behind the held gate, then release.
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(hold);
    let replies: Vec<_> = senders
      .into_iter()
      .map(|sender| sender.join().expect("sender join"))
      .collect();

    assert_eq!(replies.len(), EDITS);
    for (blob_ix, reply) in replies.iter().enumerate() {
      assert!(reply.is_ok(), "blob {blob_ix} reply failed: {reply:?}");
    }
    assert_eq!(cell_text(&gate, cell), source_text, "receiver did not converge to the source cell text");
    let batches = import_batches(&gate) - batches_before;
    // 40 blobs at ≤16/chunk = ≥3 batches; the bound allows the io thread to
    // have consumed a couple of blobs before the queue filled. The property
    // under guard: NOT one-batch-per-blob (the pre-§6.4 field shape = 40).
    assert!(
      (3..=8).contains(&batches),
      "expected 3..=8 coalesced batches for {EDITS} blobs behind a held gate, saw {batches}"
    );
    Ok(())
  }

  /// Duplicated + reversed blobs: Loro buffers out-of-order updates and
  /// re-applies duplicates as no-ops — every reply is Ok and the receiver
  /// converges regardless of delivery order.
  #[test]
  fn duplicated_and_reversed_drip_converges() -> Result<()> {
    const EDITS: usize = 12;
    let (source_text, blobs, gate, cell) = drip_pair(EDITS)?;
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let mut schedule: Vec<Vec<u8>> = Vec::new();
    for blob in blobs.iter().rev() {
      schedule.push(blob.clone());
      schedule.push(blob.clone()); // duplicate every blob
    }
    let replies = pollster::block_on(async {
      let mut replies = Vec::new();
      for blob in schedule {
        replies.push(io.import_remote_update(blob).await);
      }
      replies
    });
    for (delivery_ix, reply) in replies.iter().enumerate() {
      assert!(reply.is_ok(), "delivery {delivery_ix} reply failed: {reply:?}");
    }
    assert_eq!(
      cell_text(&gate, cell),
      source_text,
      "receiver did not converge under duplicated reversed delivery"
    );
    Ok(())
  }

  /// A non-import request interleaved mid-burst is DEFERRED, not dropped: it
  /// must still reply correctly after the import chunk completes.
  #[test]
  fn non_import_request_interleaved_with_burst_still_replies() -> Result<()> {
    const EDITS: usize = 10;
    let (source_text, blobs, gate, cell) = drip_pair(EDITS)?;
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let hold = gate
      .lock(GateHolder::DocumentService)
      .expect("gate healthy");
    let io_imports = io.clone();
    let import_thread = std::thread::spawn(move || {
      pollster::block_on(async move {
        let mut replies = Vec::new();
        for blob in blobs {
          replies.push(io_imports.import_remote_update(blob).await);
        }
        replies
      })
    });
    // Interleave a board-snapshot request into the queued burst.
    let io_snapshot = io.clone();
    let snapshot_thread = std::thread::spawn(move || pollster::block_on(io_snapshot.board_snapshot()));
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(hold);

    let replies = import_thread.join().expect("imports join");
    for reply in &replies {
      assert!(reply.is_ok(), "import reply failed: {reply:?}");
    }
    let snapshot = snapshot_thread.join().expect("snapshot join");
    assert!(snapshot.is_ok(), "deferred non-import request lost/errored: {snapshot:?}");
    assert_eq!(cell_text(&gate, cell), source_text);
    Ok(())
  }

  /// The publish pump drains committed local events exactly once: first pump
  /// returns the `LocalUpdate` batch, an immediate second pump returns nothing.
  #[test]
  fn pump_publish_drains_exactly_once() -> Result<()> {
    let runtime = FlowRuntime::new_empty();
    let sheet_type = runtime.board().format.sheet_types[0].id;
    let (handle, gate) = FlowDocHandle::new(runtime);
    let sheet = Uuid::from_u128(0xf10c_0003);
    handle
      .apply(&FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "Pump".into(),
        sheet_type_id: sheet_type,
      })
      .map_err(|error| anyhow::anyhow!("seed sheet rejected: {error}"))?;
    let row = Uuid::from_u128(0xf10c_1004);
    handle
      .apply(&FlowIntent::InsertRows {
        sheet_id: sheet,
        before: None,
        row_ids: vec![row],
      })
      .map_err(|error| anyhow::anyhow!("seed row rejected: {error}"))?;
    let column = handle
      .board_projection()
      .map_err(|error| anyhow::anyhow!("board unavailable: {error}"))?
      .sheet(sheet)
      .expect("seed sheet")
      .columns[0]
      .id;
    let cell: CellId = Uuid::from_u128(0xf10c_0004);
    handle
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: cell,
        row_id: row,
        column_id: column,
        seed: CellSeed::Paragraphs(seed_paragraphs("pump seed")),
      })
      .map_err(|error| anyhow::anyhow!("seed cell rejected: {error}"))?;
    // Drain the seed traffic first so the assertion isolates OUR edit.
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");
    let _ = pollster::block_on(io.pump_publish())?;

    let projection = handle
      .cell_projection(cell)
      .map_err(|error| anyhow::anyhow!("cell projection unavailable: {error}"))?;
    handle
      .cell_authority(cell)
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(projection.ids.paragraph_ids[0], 0),
        text: "published".into(),
        style_override: None,
      }))
      .map_err(|error| anyhow::anyhow!("edit rejected: {error:?}"))?;
    let first = pollster::block_on(io.pump_publish())?;
    assert!(!first.is_empty(), "first pump must return the committed LocalUpdate events");
    let second = pollster::block_on(io.pump_publish())?;
    assert!(
      second.is_empty(),
      "second pump must be empty — events were already drained (double-publish hazard)"
    );
    Ok(())
  }

  /// After `drip_pair`'s seed, the SOURCE must not hold committed ops the
  /// receiver lacks — otherwise the dripped blobs' deps would leave every
  /// import PENDING forever (seed text only, while all replies are Ok).
  #[test]
  fn drip_pair_source_holds_no_unsynced_ops() -> Result<()> {
    let (_, blobs, gate, cell) = drip_pair(2)?;
    {
      let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
      for blob in &blobs {
        guard.import_remote_updates(&[blob.as_slice()])?;
      }
    }
    let text = cell_text(&gate, cell);
    assert!(text.contains('0'), "receiver applied blob 0 (deps satisfied); got {text:?}");
    Ok(())
  }
}
