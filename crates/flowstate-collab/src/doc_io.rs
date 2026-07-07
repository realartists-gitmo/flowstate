//! `DocIoService` — the background document I/O service (Loro-first spec §3/§6).
//!
//! Replaces the old CRDT actor. The critical difference: this service does NOT
//! own the document and local edits never pass through it. It shares the doc
//! core with the UI thread's `LocalDocHandle` through the write gate, and
//! acquires that gate for every doc-touching operation (I-9a — every Loro call
//! is a potential commit barrier). Its duties:
//!
//! * remote import chunks (coalesced under one gate hold, spec §6.4);
//! * draining + broadcasting the publish queue filled by local commits;
//! * update/snapshot exports (snapshots fork under the gate, export off it);
//! * presence encode/resolve (gate-held, coalescible);
//! * package/checkpoint/revision/asset services.
//!
//! Local typing can stall behind this service for AT MOST one gate slice; the
//! per-slice hold budget is enforced by the gate's measured records (§11).

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};

/// Reply channel alias (clippy type-complexity).
type ReplySender<T> = Sender<anyhow::Result<T>>;
use flowstate_document::DocumentProjection;
use loro::{ExportMode, VersionVector};

use crate::crdt_runtime::{
  CrdtRuntime, RuntimeAssetMetadata, RuntimeEvent, RuntimePresenceCaretRequest, RuntimePresenceCarets, RuntimeRevisionInfo,
  UndoSelectionSnapshot,
};
use crate::local_write::{GateHolder, WriteGate};
use crate::presence::PresenceSelection;

const IO_REQUEST_CHANNEL_CAPACITY: usize = 256;
/// Maximum queued import blobs coalesced into one gate hold (spec §6.4). The
/// gate hold-time budget is the real bound; this caps pathological queues.
const IMPORT_COALESCE_MAX: usize = 16;

pub enum IoRequest {
  ImportRemoteUpdate {
    bytes: Vec<u8>,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  /// Drain the publish queue filled by gate-held local commits (`LocalUpdate`
  /// bytes etc.) for network broadcast.
  PumpPublish {
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  ProjectionSnapshot {
    reply: Sender<Result<DocumentProjection>>,
  },
  OplogVersionVector {
    reply: Sender<Result<Vec<u8>>>,
  },
  ExportUpdatesFor {
    remote_vv: Vec<u8>,
    reply: Sender<Result<Vec<u8>>>,
  },
  SnapshotBytes {
    reply: Sender<Result<Vec<u8>>>,
  },
  CheckpointPackage {
    title: String,
    path: Option<PathBuf>,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  PackageBytes {
    title: String,
    reply: Sender<Result<Vec<u8>>>,
  },
  SavePackageTo {
    path: PathBuf,
    reply: Sender<Result<()>>,
  },
  TakeRestoredUndoSelection {
    reply: Sender<Result<Option<UndoSelectionSnapshot>>>,
  },
  AssetMetadata {
    reply: Sender<Result<Vec<RuntimeAssetMetadata>>>,
  },
  Revisions {
    reply: Sender<Result<Vec<RuntimeRevisionInfo>>>,
  },
  ProjectionFallbackStats {
    reply: Sender<Result<crate::crdt_runtime::ProjectionFallbackStats>>,
  },
  PresenceSelection {
    selection: gpui_flowtext::EditorSelection,
    reply: Sender<Result<Option<PresenceSelection>>>,
  },
  ResolvePresenceCarets {
    requests: Vec<RuntimePresenceCaretRequest>,
    reply: Sender<Result<RuntimePresenceCarets>>,
  },
  SetAuthorIdentity {
    user_id: u128,
    display_name: Option<String>,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  /// Open a named revision read-only (projection of historical state).
  OpenRevision {
    revision_id: u128,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  /// Fork a named revision into a fresh document package.
  ForkRevision {
    revision_id: u128,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  /// Record fetched asset bytes into canonical state (joiner asset pulls).
  RecordAssets {
    assets: Vec<flowstate_document::AssetRecord>,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
}

fn io_request_kind(request: &IoRequest) -> &'static str {
  match request {
    IoRequest::ImportRemoteUpdate { .. } => "import-remote-update",
    IoRequest::PumpPublish { .. } => "pump-publish",
    IoRequest::ProjectionSnapshot { .. } => "projection-snapshot",
    IoRequest::OplogVersionVector { .. } => "oplog-version-vector",
    IoRequest::ExportUpdatesFor { .. } => "export-updates-for",
    IoRequest::SnapshotBytes { .. } => "snapshot-bytes",
    IoRequest::CheckpointPackage { .. } => "checkpoint-package",
    IoRequest::PackageBytes { .. } => "package-bytes",
    IoRequest::SavePackageTo { .. } => "save-package-to",
    IoRequest::TakeRestoredUndoSelection { .. } => "take-restored-undo-selection",
    IoRequest::AssetMetadata { .. } => "asset-metadata",
    IoRequest::Revisions { .. } => "revisions",
    IoRequest::ProjectionFallbackStats { .. } => "projection-fallback-stats",
    IoRequest::PresenceSelection { .. } => "presence-selection",
    IoRequest::ResolvePresenceCarets { .. } => "resolve-presence-carets",
    IoRequest::SetAuthorIdentity { .. } => "set-author-identity",
    IoRequest::OpenRevision { .. } => "open-revision",
    IoRequest::ForkRevision { .. } => "fork-revision",
    IoRequest::RecordAssets { .. } => "record-assets",
  }
}

/// Cloneable handle to the I/O service thread.
#[derive(Clone)]
pub struct DocIoHandle {
  requests: Sender<IoRequest>,
}

impl DocIoHandle {
  /// Spawn the I/O service over a shared, gate-protected document core.
  pub fn spawn(core: Arc<WriteGate<CrdtRuntime>>) -> io::Result<Self> {
    let (sender, receiver) = async_channel::bounded(IO_REQUEST_CHANNEL_CAPACITY);
    std::thread::Builder::new()
      .name("flowstate-doc-io".to_string())
      .spawn(move || io_loop(&core, &receiver))?;
    Ok(Self { requests: sender })
  }

  async fn request<T>(&self, build: impl FnOnce(Sender<Result<T>>) -> IoRequest) -> Result<T> {
    let (reply, response) = async_channel::bounded(1);
    self
      .requests
      .send(build(reply))
      .await
      .map_err(|_| anyhow::anyhow!("doc I/O service stopped"))?;
    response
      .recv()
      .await
      .map_err(|_| anyhow::anyhow!("doc I/O service dropped the reply"))?
  }

  pub async fn import_remote_update(&self, bytes: Vec<u8>) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::ImportRemoteUpdate { bytes, reply }).await
  }

  /// Drain committed-but-unpublished local events (call after local intents,
  /// debounced by the session).
  pub async fn pump_publish(&self) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::PumpPublish { reply }).await
  }

  pub async fn projection_snapshot(&self) -> Result<DocumentProjection> {
    self.request(|reply| IoRequest::ProjectionSnapshot { reply }).await
  }

  pub async fn oplog_version_vector(&self) -> Result<Vec<u8>> {
    self.request(|reply| IoRequest::OplogVersionVector { reply }).await
  }

  pub async fn export_updates_for(&self, remote_vv: Vec<u8>) -> Result<Vec<u8>> {
    self.request(|reply| IoRequest::ExportUpdatesFor { remote_vv, reply }).await
  }

  pub async fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    self.request(|reply| IoRequest::SnapshotBytes { reply }).await
  }

  pub async fn checkpoint_package(&self, title: String, path: Option<PathBuf>) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::CheckpointPackage { title, path, reply }).await
  }

  pub async fn package_bytes(&self, title: String) -> Result<Vec<u8>> {
    self.request(|reply| IoRequest::PackageBytes { title, reply }).await
  }

  pub async fn save_package_to(&self, path: PathBuf) -> Result<()> {
    self.request(|reply| IoRequest::SavePackageTo { path, reply }).await
  }

  pub async fn take_restored_undo_selection(&self) -> Result<Option<UndoSelectionSnapshot>> {
    self.request(|reply| IoRequest::TakeRestoredUndoSelection { reply }).await
  }

  pub async fn asset_metadata(&self) -> Result<Vec<RuntimeAssetMetadata>> {
    self.request(|reply| IoRequest::AssetMetadata { reply }).await
  }

  pub async fn revisions(&self) -> Result<Vec<RuntimeRevisionInfo>> {
    self.request(|reply| IoRequest::Revisions { reply }).await
  }

  pub async fn projection_fallback_stats(&self) -> Result<crate::crdt_runtime::ProjectionFallbackStats> {
    self.request(|reply| IoRequest::ProjectionFallbackStats { reply }).await
  }

  pub async fn presence_selection(&self, selection: gpui_flowtext::EditorSelection) -> Result<Option<PresenceSelection>> {
    self.request(|reply| IoRequest::PresenceSelection { selection, reply }).await
  }

  pub async fn resolve_presence_carets(&self, requests: Vec<RuntimePresenceCaretRequest>) -> Result<RuntimePresenceCarets> {
    self.request(|reply| IoRequest::ResolvePresenceCarets { requests, reply }).await
  }

  pub async fn open_revision(&self, revision_id: u128) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::OpenRevision { revision_id, reply }).await
  }

  pub async fn fork_revision(&self, revision_id: u128) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::ForkRevision { revision_id, reply }).await
  }

  /// Record fetched asset bytes into canonical state so saves/packages made by
  /// this replica include assets it pulled from peers.
  pub async fn record_assets(&self, assets: Vec<flowstate_document::AssetRecord>) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::RecordAssets { assets, reply }).await
  }

  pub async fn set_author_identity(&self, user_id: u128, display_name: Option<String>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::SetAuthorIdentity {
        user_id,
        display_name,
        reply,
      })
      .await
  }
}

fn send_reply<T>(reply: &Sender<Result<T>>, value: Result<T>) {
  if let Err(error) = reply.try_send(value) {
    tracing::warn!(%error, "doc I/O reply channel dropped before the reply was sent");
  }
}

fn io_loop(core: &Arc<WriteGate<CrdtRuntime>>, receiver: &Receiver<IoRequest>) {
  // Buffered non-import requests popped while coalescing an import chunk.
  let mut deferred: Vec<IoRequest> = Vec::new();
  loop {
    let request = if let Some(request) = deferred.pop() {
      request
    } else {
      match receiver.recv_blocking() {
        Ok(request) => request,
        Err(_) => break,
      }
    };
    let kind = io_request_kind(&request);
    let started = std::time::Instant::now();
    match request {
      IoRequest::ImportRemoteUpdate { bytes, reply } => {
        // Spec §6.4: coalesce immediately-available import blobs into ONE gate
        // acquisition. Non-import requests popped while draining are deferred
        // (processed right after, order preserved among themselves).
        let mut chunk: Vec<(Vec<u8>, ReplySender<Vec<RuntimeEvent>>)> = vec![(bytes, reply)];
        while chunk.len() < IMPORT_COALESCE_MAX {
          match receiver.try_recv() {
            Ok(IoRequest::ImportRemoteUpdate { bytes, reply }) => chunk.push((bytes, reply)),
            Ok(other) => {
              deferred.push(other);
              break;
            },
            Err(_) => break,
          }
        }
        let coalesced = chunk.len();
        match core.lock(GateHolder::ImportChunk) {
          Ok(mut guard) => {
            for (bytes, reply) in chunk {
              send_reply(&reply, guard.import_remote_update(&bytes));
            }
            // Spec §6.4 memory hygiene: import bursts build diff-calculator and
            // history caches; free them at chunk end (OOM history). Cheap
            // no-ops when the caches are cold.
            if coalesced > 1 {
              guard.doc().free_diff_calculator();
              guard.doc().free_history_cache();
            }
          },
          Err(poisoned) => {
            for (_, reply) in chunk {
              send_reply(&reply, Err(anyhow::anyhow!(poisoned)));
            }
          },
        }
        if coalesced > 1 {
          tracing::debug!(coalesced, "coalesced import chunk under one gate hold");
        }
      },
      IoRequest::PumpPublish { reply } => {
        let result = core
          .lock(GateHolder::ExportUpdates)
          .map(|mut guard| guard.take_pending_publish())
          .map_err(|poisoned| anyhow::anyhow!(poisoned));
        send_reply(&reply, result);
      },
      IoRequest::ProjectionSnapshot { reply } =>

        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| runtime.projection_snapshot())),
      IoRequest::OplogVersionVector { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::ExportUpdates, |runtime| Ok(runtime.doc().oplog_vv().encode())));
      },
      IoRequest::ExportUpdatesFor { remote_vv, reply } => {
        let result = VersionVector::decode(&remote_vv)
          .context("decoding remote Loro version vector")
          .and_then(|vv| gate_call(core, GateHolder::ExportUpdates, |runtime| runtime.export_updates_for(&vv)));
        send_reply(&reply, result);
      },
      IoRequest::SnapshotBytes { reply } => {
        // I-9a long-export rule: fork under the gate (brief), export the fork
        // off-gate so a large snapshot never stalls typing.
        let forked = gate_call(core, GateHolder::ExportFork, |runtime| Ok(runtime.doc().fork()));
        let result = forked.and_then(|fork| fork.export(ExportMode::Snapshot).context("exporting Loro snapshot from fork"));
        send_reply(&reply, result);
      },
      IoRequest::CheckpointPackage { title, path, reply } => {
        // Field fix 2026-07-07: split checkpoint — live-doc phase under a
        // short gate hold, heavy assembly + disk write on a detached worker,
        // package restored under the gate afterwards. First-save (no package
        // yet) falls back to the in-place path.
        let begun = gate_call(core, GateHolder::DocumentService, |runtime| {
          runtime.begin_checkpoint(&title, path.clone()).map_err(anyhow::Error::from)
        });
        match begun {
          Ok(Some((job, events))) => {
            let core = Arc::clone(core);
            if let Err(error) = std::thread::Builder::new()
              .name("flowstate-checkpoint".to_string())
              .spawn(move || {
                let (package, wrote) = job.run();
                let restore = core.lock(GateHolder::DocumentService).map(|mut guard| match wrote {
                  Ok(wrote) => {
                    guard.finish_checkpoint(package, wrote);
                    Ok(())
                  },
                  Err(error) => {
                    guard.finish_checkpoint(package, false);
                    Err(anyhow::Error::from(error))
                  },
                });
                match restore {
                  Ok(Ok(())) => send_reply(&reply, Ok(events)),
                  Ok(Err(error)) => send_reply(&reply, Err(error)),
                  Err(poisoned) => send_reply(&reply, Err(anyhow::anyhow!(poisoned))),
                }
              })
            {
              tracing::error!(%error, "spawning checkpoint worker failed");
            }
          },
          Ok(None) => {
            send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| {
              runtime.checkpoint_package(&title, path).map_err(anyhow::Error::from)
            }));
          },
          Err(error) => send_reply(&reply, Err(error)),
        }
      },
      IoRequest::PackageBytes { title, reply } => {
        // Field fix 2026-07-07: fork under a SHORT gate hold, assemble the
        // package on a detached worker. The 18s whole-assembly-under-gate
        // holds in the field logs froze typing and starved imports.
        match gate_call(core, GateHolder::ExportFork, |runtime| Ok(runtime.package_export_context())) {
          Ok((fork, projection)) => {
            let title = title.clone();
            if let Err(error) = std::thread::Builder::new()
              .name("flowstate-package-export".to_string())
              .spawn(move || {
                let result = crate::crdt_runtime::assemble_package_bytes(&fork, &projection, &title).map_err(anyhow::Error::from);
                send_reply(&reply, result);
              })
            {
              tracing::error!(%error, "spawning package export worker failed");
            }
          },
          Err(error) => send_reply(&reply, Err(error)),
        }
      },
      IoRequest::SavePackageTo { path, reply } => {
        // Save-as = full checkpoint targeting the new path (split path); the
        // in-place fallback covers package-less first saves.
        let begun = gate_call(core, GateHolder::DocumentService, |runtime| {
          runtime
            .begin_checkpoint("Save", Some(path.clone()))
            .map_err(anyhow::Error::from)
        });
        match begun {
          Ok(Some((job, _events))) => {
            let core = Arc::clone(core);
            if let Err(error) = std::thread::Builder::new()
              .name("flowstate-save".to_string())
              .spawn(move || {
                let (package, wrote) = job.run();
                let restore = core.lock(GateHolder::DocumentService).map(|mut guard| match wrote {
                  Ok(wrote) => {
                    guard.finish_checkpoint(package, wrote);
                    Ok(())
                  },
                  Err(error) => {
                    guard.finish_checkpoint(package, false);
                    Err(anyhow::Error::from(error))
                  },
                });
                match restore {
                  Ok(result) => send_reply(&reply, result),
                  Err(poisoned) => send_reply(&reply, Err(anyhow::anyhow!(poisoned))),
                }
              })
            {
              tracing::error!(%error, "spawning save worker failed");
            }
          },
          Ok(None) => {
            send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| runtime.save_package_to(path).map_err(anyhow::Error::from)));
          },
          Err(error) => send_reply(&reply, Err(error)),
        }
      },
      IoRequest::TakeRestoredUndoSelection { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.take_restored_undo_selection())));
      },
      IoRequest::AssetMetadata { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| runtime.asset_metadata()));
      },
      IoRequest::Revisions { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.revisions())));
      },
      IoRequest::ProjectionFallbackStats { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.projection_fallback_stats())));
      },
      IoRequest::PresenceSelection { selection, reply } => {
        send_reply(&reply, gate_call(core, GateHolder::Presence, |runtime| Ok(runtime.presence_selection(&selection))));
      },
      IoRequest::ResolvePresenceCarets { requests, reply } => {
        send_reply(&reply, gate_call(core, GateHolder::Presence, |runtime| Ok(runtime.resolve_presence_carets(requests))));
      },
      IoRequest::SetAuthorIdentity {
        user_id,
        display_name,
        reply,
      } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| runtime.set_author_identity(user_id, display_name)));
      },
      IoRequest::OpenRevision { revision_id, reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| {
          runtime.command(crate::crdt_runtime::SemanticCommand::OpenRevision { revision_id })
        }));
      },
      IoRequest::ForkRevision { revision_id, reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| {
          runtime.command(crate::crdt_runtime::SemanticCommand::ForkRevision { revision_id })
        }));
      },
      IoRequest::RecordAssets { assets, reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| runtime.merge_asset_records(assets)));
      },
    }
    let elapsed_ms = started.elapsed().as_millis();
    if elapsed_ms > 250 {
      tracing::error!(kind, elapsed_ms, "slow doc I/O request");
    }
  }
}

fn gate_call<T>(
  core: &Arc<WriteGate<CrdtRuntime>>,
  holder: GateHolder,
  call: impl FnOnce(&mut CrdtRuntime) -> Result<T>,
) -> Result<T> {
  let mut guard = core.lock(holder).map_err(|poisoned| anyhow::anyhow!(poisoned))?;
  call(&mut guard)
}
