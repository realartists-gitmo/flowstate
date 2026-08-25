//! The flow I/O pump harness — the `doc_io_pump` drip suite mirrored onto
//! `FlowIoHandle` (build-order step 6): §6.4 import coalescing behind a held
//! gate, pathological delivery order, deferred non-import ordering, the
//! exactly-once publish pump, and the `.fl0` save/recovery byte paths.

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use anyhow::Result;
  use flowstate_collab::flow::{FlowDocHandle, FlowIoHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_collab::local_write::{GateHolder, WriteGate};
  use flowstate_flow::format::{FlowFormat, SheetId};
  use flowstate_flow::intents::FlowIntent;
  use uuid::Uuid;

  type FlowGate = Arc<WriteGate<FlowRuntime>>;

  /// A based (source handle, receiver gate) pair plus `edits` sheet renames
  /// exported as one delta blob PER EDIT (the network drip shape) and the
  /// source's final board.
  fn drip_pair(edits: usize) -> Result<(flowstate_flow::projection::FlowBoardProjection, Vec<Vec<u8>>, FlowGate)> {
    let format = FlowFormat::policy_debate();
    let (source_handle, source_gate) = FlowDocHandle::new(FlowRuntime::new(&format)?);
    let sheet: SheetId = Uuid::new_v4();
    source_handle
      .apply(FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "Drip".into(),
        sheet_type_id: format.sheet_types[0].id,
      })
      .map_err(|error| anyhow::anyhow!("seed sheet rejected: {error}"))?;
    let snapshot = source_handle.with_test_runtime(|runtime| runtime.snapshot())?;
    let (_receiver_handle, receiver_gate) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot)?);

    let mut blobs = Vec::with_capacity(edits);
    for edit_ix in 0..edits {
      let vv_before = source_gate
        .lock(GateHolder::ExportUpdates)
        .expect("gate healthy")
        .oplog_version_vector();
      source_handle
        .apply(FlowIntent::RenameSheet {
          sheet_id: sheet,
          name: format!("Drip {edit_ix}"),
        })
        .map_err(|error| anyhow::anyhow!("drip edit {edit_ix} rejected: {error}"))?;
      let blob = source_gate
        .lock(GateHolder::ExportUpdates)
        .expect("gate healthy")
        .export_updates_for(&vv_before)?;
      blobs.push(blob);
    }
    let final_board = source_handle
      .board_projection()
      .map_err(|error| anyhow::anyhow!("board: {error}"))?;
    Ok((final_board, blobs, receiver_gate))
  }

  fn board(gate: &FlowGate) -> flowstate_flow::projection::FlowBoardProjection {
    gate
      .lock(GateHolder::ExportUpdates)
      .expect("gate healthy")
      .board_ref()
      .clone()
  }

  fn import_batches(gate: &FlowGate) -> u64 {
    gate
      .lock(GateHolder::ExportUpdates)
      .expect("gate healthy")
      .import_batches_served()
  }

  /// A 40-blob drip fired while the gate is HELD must coalesce into a few
  /// batched imports, every reply must arrive, and the receiver must converge.
  #[test]
  fn held_gate_drip_coalesces_into_bounded_batches() -> Result<()> {
    const EDITS: usize = 40;
    let (source_board, blobs, gate) = drip_pair(EDITS)?;
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let batches_before = import_batches(&gate);
    let hold = gate
      .lock(GateHolder::DocumentService)
      .expect("gate healthy");
    #[allow(clippy::needless_collect, reason = "spawn-all-then-join-all; lazy iteration would serialize the burst")]
    let senders: Vec<_> = blobs
      .iter()
      .map(|blob| {
        let io_import = io.clone();
        let blob = blob.clone();
        std::thread::spawn(move || pollster::block_on(io_import.import_remote_update(blob)))
      })
      .collect();
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
    assert_eq!(board(&gate), source_board, "receiver did not converge to the source board");
    let batches = import_batches(&gate) - batches_before;
    assert!(
      (3..=8).contains(&batches),
      "expected 3..=8 coalesced batches for {EDITS} blobs behind a held gate, saw {batches}"
    );
    Ok(())
  }

  /// Duplicated + reversed blobs: Loro buffers out-of-order updates and
  /// re-applies duplicates as no-ops — every reply Ok, receiver converges.
  #[test]
  fn duplicated_and_reversed_drip_converges() -> Result<()> {
    const EDITS: usize = 12;
    let (source_board, blobs, gate) = drip_pair(EDITS)?;
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let mut schedule: Vec<Vec<u8>> = Vec::new();
    for blob in blobs.iter().rev() {
      schedule.push(blob.clone());
      schedule.push(blob.clone());
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
    assert_eq!(board(&gate), source_board, "receiver did not converge under duplicated reversed delivery");
    Ok(())
  }

  /// A non-import request interleaved mid-burst is DEFERRED, not dropped.
  #[test]
  fn non_import_request_interleaved_with_burst_still_replies() -> Result<()> {
    const EDITS: usize = 10;
    let (source_board, blobs, gate) = drip_pair(EDITS)?;
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
    assert_eq!(board(&gate), source_board);
    Ok(())
  }

  /// The publish pump drains committed local events exactly once.
  #[test]
  fn pump_publish_drains_exactly_once() -> Result<()> {
    let format = FlowFormat::policy_debate();
    let (handle, gate) = FlowDocHandle::new(FlowRuntime::new(&format)?);
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");
    let _ = pollster::block_on(io.pump_publish())?;
    handle
      .apply(FlowIntent::CreateSheet {
        sheet_id: Uuid::new_v4(),
        name: "Published".into(),
        sheet_type_id: format.sheet_types[0].id,
      })
      .map_err(|error| anyhow::anyhow!("edit rejected: {error}"))?;
    let first = pollster::block_on(io.pump_publish())?;
    assert!(
      first
        .iter()
        .any(|event| matches!(event, FlowPublishEvent::LocalUpdate { .. })),
      "first pump must return the committed LocalUpdate events"
    );
    let second = pollster::block_on(io.pump_publish())?;
    assert!(second.is_empty(), "second pump must be empty — double-publish hazard");
    Ok(())
  }

  /// `SaveTo` writes a valid v2 .fl0; `EncodeBytes` yields recovery bytes decoding
  /// to the identical board.
  #[test]
  fn save_and_recovery_bytes_round_trip() -> Result<()> {
    let (source_board, _, gate) = drip_pair(1)?;
    let io = FlowIoHandle::spawn(Arc::clone(&gate)).expect("io service");
    let dir = std::env::temp_dir().join(format!("flowstate-flow-io-{}-{}", std::process::id(), Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("saved.fl0");
    pollster::block_on(io.save_to(path.clone()))?;
    let snapshot = flowstate_flow::read_fl0(&path)?;
    let restored = FlowRuntime::from_snapshot(&snapshot)?;
    // The receiver gate has NOT imported the drip blob here — compare against
    // its own board, not the source's.
    let saved_board = board(&gate);
    assert_eq!(restored.board_ref(), &saved_board);
    let _ = source_board;

    let recovery = pollster::block_on(io.encode_bytes())?;
    let decoded = flowstate_flow::decode_fl0_snapshot(&recovery)?;
    let recovered = FlowRuntime::from_snapshot(&decoded)?;
    assert_eq!(recovered.board_ref(), &saved_board);
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
  }
}
