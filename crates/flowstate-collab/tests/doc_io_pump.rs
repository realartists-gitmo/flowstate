//! §act-eleven C8: the doc I/O pump harness. `DocIoHandle` is the field's
//! delivery surface — the §6.4 import coalescing, the P1.A second drain behind
//! a held gate, deferred non-import ordering, and the publish pump — and it
//! previously had ONE test spawning it (a package-export latency probe). This
//! suite drives it with scripted drips: bursts behind a held gate, duplicated
//! and out-of-order blobs, and interleaved non-import requests.

use anyhow::Result;
use flowstate_collab::crdt_runtime::CrdtRuntime;
use flowstate_collab::doc_io::DocIoHandle;
use flowstate_collab::local_write::{GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, TextAnchor};
use std::sync::Arc;

/// A fully-based (source, receiver) pair — sentinel init ops EXCHANGED both
/// ways (the known one-way trap) — plus `edits` sequential source inserts
/// exported as one delta blob PER EDIT (the network drip shape) and the
/// source's final body text.
type ReceiverGate = Arc<flowstate_collab::local_write::WriteGate<CrdtRuntime>>;

fn drip_pair(edits: usize) -> Result<(String, Vec<Vec<u8>>, ReceiverGate)> {
  let source = CrdtRuntime::new_empty("drip-source")?;
  let (source_handle, source_gate) = LocalDocHandle::new(source, LocalWriteConfig::default());
  let receiver = CrdtRuntime::new_empty("drip-receiver")?;
  let (_receiver_handle, receiver_gate) = LocalDocHandle::new(receiver, LocalWriteConfig::default());
  let full_export = |gate: &ReceiverGate| {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("init export")
  };
  {
    let mut guard = receiver_gate.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&full_export(&source_gate))?
  };
  {
    let mut guard = source_gate.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&full_export(&receiver_gate))?
  };

  let paragraph = *source_handle.projection()?.ids.paragraph_ids.last().expect("source paragraph");
  let mut blobs = Vec::with_capacity(edits);
  for edit_ix in 0..edits {
    let vv_before = {
      let guard = source_gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
      guard.doc().state_vv()
    };
    source_handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: format!("{}", edit_ix % 10),
        style_override: None,
      })
      .map_err(|error| anyhow::anyhow!("drip edit {edit_ix} rejected: {error:?}"))?;
    let blob = {
      let guard = source_gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
      guard.doc().export(loro::ExportMode::updates(&vv_before)).expect("delta export")
    };
    blobs.push(blob);
  }
  let final_text = {
    let guard = source_gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
    flowstate_document::loro_schema::body_text(guard.doc()).to_string()
  };
  Ok((final_text, blobs, receiver_gate))
}

fn body_text(gate: &ReceiverGate) -> String {
  let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
  flowstate_document::loro_schema::body_text(guard.doc()).to_string()
}

fn import_batches(gate: &ReceiverGate) -> u64 {
  let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
  guard.import_batches_served()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A 40-blob drip fired while the gate is HELD must coalesce into a few
  /// batched imports (≤16 blobs per §6.4 chunk, folded further by the P1.A
  /// second drain), every reply must arrive, and the receiver must converge.
  #[test]
  fn held_gate_drip_coalesces_into_bounded_batches() -> Result<()> {
    const EDITS: usize = 40;
    let (source_text, blobs, gate) = drip_pair(EDITS)?;
    let io = DocIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let batches_before = import_batches(&gate);
    // Hold the gate so the io thread blocks on its FIRST import; every blob
    // sent meanwhile queues behind it and must fold into few batches.
    let hold = gate.lock(GateHolder::DocumentService).expect("gate healthy");
    // One OS thread per blob: async fns are LAZY, so sequentially awaiting
    // them would send each request only after the previous replied (zero queue
    // depth, zero coalescing). Independent blocked threads enqueue all 40
    // requests concurrently behind the held gate — the real network shape.
    // NOTE: the intermediate Vec is REQUIRED (spawn all senders BEFORE joining
    // any — a lazy iterator would serialize spawn/join pairs and give zero
    // queue depth, exactly the lazy-future bug this replaced).
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
    let replies: Vec<_> = senders.into_iter().map(|sender| sender.join().expect("sender join")).collect();

    assert_eq!(replies.len(), EDITS);
    for (blob_ix, reply) in replies.iter().enumerate() {
      assert!(reply.is_ok(), "blob {blob_ix} reply failed: {reply:?}");
    }
    assert_eq!(body_text(&gate), source_text, "receiver did not converge to the source text");
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
  /// converges regardless of delivery order. (The recorded-inverse no-op-drip
  /// preservation is unit-tested in `local_write`; here the property is the
  /// PUMP's: no reply is lost or errored under pathological delivery.)
  #[test]
  fn duplicated_and_reversed_drip_converges() -> Result<()> {
    const EDITS: usize = 12;
    let (source_text, blobs, gate) = drip_pair(EDITS)?;
    let io = DocIoHandle::spawn(Arc::clone(&gate)).expect("io service");

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
    assert_eq!(body_text(&gate), source_text, "receiver did not converge under duplicated reversed delivery");
    Ok(())
  }

  /// A non-import request interleaved mid-burst is DEFERRED, not dropped: it
  /// must still reply correctly after the import chunk completes.
  #[test]
  fn non_import_request_interleaved_with_burst_still_replies() -> Result<()> {
    const EDITS: usize = 10;
    let (source_text, blobs, gate) = drip_pair(EDITS)?;
    let io = DocIoHandle::spawn(Arc::clone(&gate)).expect("io service");

    let hold = gate.lock(GateHolder::DocumentService).expect("gate healthy");
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
    // Interleave a snapshot request into the queued burst.
    let io_snapshot = io.clone();
    let snapshot_thread = std::thread::spawn(move || pollster::block_on(io_snapshot.projection_snapshot()));
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(hold);

    let replies = import_thread.join().expect("imports join");
    for reply in &replies {
      assert!(reply.is_ok(), "import reply failed: {reply:?}");
    }
    let snapshot = snapshot_thread.join().expect("snapshot join");
    assert!(snapshot.is_ok(), "deferred non-import request lost/errored: {snapshot:?}");
    assert_eq!(body_text(&gate), source_text);
    Ok(())
  }

  /// The publish pump drains committed local events exactly once: first pump
  /// returns the `LocalUpdate` batch, an immediate second pump returns nothing.
  #[test]
  fn pump_publish_drains_exactly_once() -> Result<()> {
    let runtime = CrdtRuntime::new_empty("pump-once")?;
    let (handle, gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
    let paragraph = handle.projection()?.ids.paragraph_ids[0];
    // Drain the seed/init traffic first so the assertion isolates OUR edit.
    let io = DocIoHandle::spawn(Arc::clone(&gate)).expect("io service");
    let _ = pollster::block_on(io.pump_publish())?;
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: "published".into(),
        style_override: None,
      })
      .map_err(|error| anyhow::anyhow!("edit rejected: {error:?}"))?;
    let first = pollster::block_on(io.pump_publish())?;
    assert!(!first.is_empty(), "first pump must return the committed LocalUpdate events");
    let second = pollster::block_on(io.pump_publish())?;
    assert!(second.is_empty(), "second pump must be empty — events were already drained (double-publish hazard)");
    Ok(())
  }

  #[test]
  fn drip_pair_source_holds_no_unsynced_ops() -> Result<()> {
    // §task #40 net: after `drip_pair`'s bidirectional seed, the SOURCE must not
    // have committed ops the receiver lacks (repair commits during the seed
    // import) — the dripped blobs' deps would leave every import PENDING
    // forever (canonical "\n\n" while all replies are Ok).
    let (_, blobs, gate) = drip_pair(2)?;
    {
      let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
      for blob in &blobs {
        let events = guard.import_remote_update(blob)?;
        let _ = events;
      }
    }
    let text = body_text(&gate);
    assert!(text.contains('0'), "receiver applied blob 0 (deps satisfied); got {text:?}");
    Ok(())
  }
}
