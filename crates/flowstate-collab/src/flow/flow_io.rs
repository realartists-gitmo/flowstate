//! `FlowIoService` — the background flow I/O service, sibling of
//! `DocIoService` (Loro-first spec §3/§6, applied to the flow gate).
//!
//! Shares the `WriteGate<FlowRuntime>` with the UI thread's `FlowDocHandle`
//! and acquires it per doc-touching operation. Duties: remote import chunks
//! (coalesced under one gate hold with the §6.4 byte budget), publish-queue
//! drains, update/snapshot exports (fork under the gate, export off it), and
//! `.fl0` persistence (atomic save + recovery-file bytes).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};
use flowstate_flow::projection::FlowBoardProjection;
use loro::{ExportMode, VersionVector};

use super::runtime::{FlowPublishEvent, FlowRuntime};
use crate::io_util::{gate_call, send_reply};
use crate::local_write::{GateHolder, WriteGate};

const IO_REQUEST_CHANNEL_CAPACITY: usize = 256;
/// Maximum queued import blobs coalesced into one gate hold (spec §6.4).
const IMPORT_COALESCE_MAX: usize = 16;
/// §A14.1.1 byte budget: never glue chunked mass-op slices back into one hold.
const IMPORT_COALESCE_BYTE_BUDGET: usize = 64 * 1024;

type ReplySender<T> = Sender<Result<T>>;

pub enum FlowIoRequest {
  ImportRemoteUpdate {
    bytes: Vec<u8>,
    reply: ReplySender<()>,
  },
  PumpPublish {
    reply: ReplySender<Vec<FlowPublishEvent>>,
  },
  BoardSnapshot {
    reply: ReplySender<FlowBoardProjection>,
  },
  OplogVersionVector {
    reply: ReplySender<Vec<u8>>,
  },
  ExportUpdatesFor {
    remote_vv: Vec<u8>,
    reply: ReplySender<Vec<u8>>,
  },
  SnapshotBytes {
    reply: ReplySender<Vec<u8>>,
  },
  /// Atomic `.fl0` save (encode v2 + write-then-rename).
  SaveTo {
    path: PathBuf,
    reply: ReplySender<()>,
  },
  /// Encoded `.fl0` bytes (recovery files for pathless joined tabs).
  EncodeBytes {
    reply: ReplySender<Vec<u8>>,
  },
}

fn request_kind(request: &FlowIoRequest) -> &'static str {
  match request {
    FlowIoRequest::ImportRemoteUpdate { .. } => "import-remote-update",
    FlowIoRequest::PumpPublish { .. } => "pump-publish",
    FlowIoRequest::BoardSnapshot { .. } => "board-snapshot",
    FlowIoRequest::OplogVersionVector { .. } => "oplog-version-vector",
    FlowIoRequest::ExportUpdatesFor { .. } => "export-updates-for",
    FlowIoRequest::SnapshotBytes { .. } => "snapshot-bytes",
    FlowIoRequest::SaveTo { .. } => "save-to",
    FlowIoRequest::EncodeBytes { .. } => "encode-bytes",
  }
}

/// Marker type for the service thread (spawned via [`FlowIoHandle::spawn`]).
pub struct FlowIoService;

/// Cloneable handle to the flow I/O service thread.
#[derive(Clone)]
pub struct FlowIoHandle {
  requests: Sender<FlowIoRequest>,
}

impl FlowIoHandle {
  pub fn spawn(core: Arc<WriteGate<FlowRuntime>>) -> io::Result<Self> {
    let (sender, receiver) = async_channel::bounded(IO_REQUEST_CHANNEL_CAPACITY);
    std::thread::Builder::new()
      .name("flowstate-flow-io".to_string())
      .spawn(move || io_loop(&core, &receiver))?;
    Ok(Self { requests: sender })
  }

  async fn request<T>(&self, build: impl FnOnce(ReplySender<T>) -> FlowIoRequest) -> Result<T> {
    let (reply, response) = async_channel::bounded(1);
    self
      .requests
      .send(build(reply))
      .await
      .map_err(|_| anyhow::anyhow!("flow I/O service stopped"))?;
    response
      .recv()
      .await
      .map_err(|_| anyhow::anyhow!("flow I/O service dropped the reply"))?
  }

  pub async fn import_remote_update(&self, bytes: Vec<u8>) -> Result<()> {
    self
      .request(|reply| FlowIoRequest::ImportRemoteUpdate { bytes, reply })
      .await
  }

  /// Drain committed-but-unpublished local events.
  pub async fn pump_publish(&self) -> Result<Vec<FlowPublishEvent>> {
    self.request(|reply| FlowIoRequest::PumpPublish { reply }).await
  }

  pub async fn board_snapshot(&self) -> Result<FlowBoardProjection> {
    self.request(|reply| FlowIoRequest::BoardSnapshot { reply }).await
  }

  pub async fn oplog_version_vector(&self) -> Result<Vec<u8>> {
    self
      .request(|reply| FlowIoRequest::OplogVersionVector { reply })
      .await
  }

  pub async fn export_updates_for(&self, remote_vv: Vec<u8>) -> Result<Vec<u8>> {
    self
      .request(|reply| FlowIoRequest::ExportUpdatesFor { remote_vv, reply })
      .await
  }

  pub async fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    self.request(|reply| FlowIoRequest::SnapshotBytes { reply }).await
  }

  pub async fn save_to(&self, path: PathBuf) -> Result<()> {
    self.request(|reply| FlowIoRequest::SaveTo { path, reply }).await
  }

  pub async fn encode_bytes(&self) -> Result<Vec<u8>> {
    self.request(|reply| FlowIoRequest::EncodeBytes { reply }).await
  }
}

fn io_loop(core: &Arc<WriteGate<FlowRuntime>>, receiver: &Receiver<FlowIoRequest>) {
  let mut deferred: Vec<FlowIoRequest> = Vec::new();
  loop {
    let request = if let Some(request) = deferred.pop() {
      request
    } else {
      match receiver.recv_blocking() {
        Ok(request) => request,
        Err(_) => break,
      }
    };
    let kind = request_kind(&request);
    let started = std::time::Instant::now();
    match request {
      FlowIoRequest::ImportRemoteUpdate { bytes, reply } => {
        // Spec §6.4: coalesce immediately-available import blobs into ONE gate
        // acquisition, respecting the byte budget; non-import requests popped
        // while draining are deferred, order preserved among themselves.
        let mut coalesced_bytes = bytes.len();
        let mut chunk: Vec<(Vec<u8>, ReplySender<()>)> = vec![(bytes, reply)];
        while chunk.len() < IMPORT_COALESCE_MAX && coalesced_bytes < IMPORT_COALESCE_BYTE_BUDGET {
          match receiver.try_recv() {
            Ok(FlowIoRequest::ImportRemoteUpdate { bytes, reply }) => {
              coalesced_bytes += bytes.len();
              chunk.push((bytes, reply));
            },
            Ok(other) => {
              deferred.push(other);
              break;
            },
            Err(_) => break,
          }
        }
        match core.lock(GateHolder::ImportChunk) {
          Ok(mut guard) => {
            // §act-five P1.A second drain: fold in blobs that arrived while
            // this thread was blocked on the gate — no added latency.
            while chunk.len() < IMPORT_COALESCE_MAX && coalesced_bytes < IMPORT_COALESCE_BYTE_BUDGET {
              match receiver.try_recv() {
                Ok(FlowIoRequest::ImportRemoteUpdate { bytes, reply }) => {
                  coalesced_bytes += bytes.len();
                  chunk.push((bytes, reply));
                },
                Ok(other) => {
                  deferred.push(other);
                  break;
                },
                Err(_) => break,
              }
            }
            let blobs: Vec<&[u8]> = chunk.iter().map(|(bytes, _)| bytes.as_slice()).collect();
            match guard.import_remote_updates(&blobs) {
              Ok(()) => {
                for (_, reply) in &chunk {
                  send_reply(reply, Ok(()));
                }
              },
              Err(error) => {
                let error = format!("{error:#}");
                for (_, reply) in &chunk {
                  send_reply(reply, Err(anyhow::anyhow!("batched flow import failed: {error}")));
                }
              },
            }
            if chunk.len() > 1 {
              tracing::debug!(coalesced = chunk.len(), "coalesced flow import chunk under one gate hold");
            }
          },
          Err(poisoned) => {
            for (_, reply) in chunk {
              send_reply(&reply, Err(anyhow::anyhow!(poisoned)));
            }
          },
        }
      },
      FlowIoRequest::PumpPublish { reply } => {
        let result = core
          .lock(GateHolder::ExportUpdates)
          .map(|mut guard| guard.take_pending_publish())
          .map_err(|poisoned| anyhow::anyhow!(poisoned));
        send_reply(&reply, result);
      },
      FlowIoRequest::BoardSnapshot { reply } => send_reply(
        &reply,
        gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.board_ref().clone())),
      ),
      FlowIoRequest::OplogVersionVector { reply } => send_reply(
        &reply,
        gate_call(core, GateHolder::ExportUpdates, |runtime| {
          Ok(runtime.oplog_version_vector().encode())
        }),
      ),
      FlowIoRequest::ExportUpdatesFor { remote_vv, reply } => {
        let result = VersionVector::decode(&remote_vv)
          .context("decoding remote Loro version vector")
          .and_then(|vv| gate_call(core, GateHolder::ExportUpdates, |runtime| runtime.export_updates_for(&vv)));
        send_reply(&reply, result);
      },
      FlowIoRequest::SnapshotBytes { reply } => {
        // I-9a long-export rule: fork under the gate (brief), export off it.
        let forked = gate_call(core, GateHolder::ExportFork, |runtime| Ok(runtime.doc().fork()));
        let result = forked.and_then(|fork| {
          fork
            .export(ExportMode::Snapshot)
            .context("exporting flow snapshot from fork")
        });
        send_reply(&reply, result);
      },
      FlowIoRequest::SaveTo { path, reply } => {
        let forked = gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.doc().fork()));
        let result = forked.and_then(|fork| {
          let snapshot = fork
            .export(ExportMode::Snapshot)
            .context("exporting flow snapshot for save")?;
          flowstate_flow::write_fl0(&path, &snapshot)
        });
        send_reply(&reply, result);
      },
      FlowIoRequest::EncodeBytes { reply } => {
        let forked = gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.doc().fork()));
        let result = forked.and_then(|fork| {
          let snapshot = fork
            .export(ExportMode::Snapshot)
            .context("exporting flow snapshot for recovery bytes")?;
          Ok(flowstate_flow::encode_fl0_snapshot(&snapshot))
        });
        send_reply(&reply, result);
      },
    }
    let elapsed = started.elapsed();
    if elapsed > std::time::Duration::from_millis(250) {
      tracing::debug!(kind, ?elapsed, "slow flow I/O request");
    }
  }
}
