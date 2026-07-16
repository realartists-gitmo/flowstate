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
  CrdtRuntime, RuntimeAssetMetadata, RuntimeCommentThread, RuntimeEvent, RuntimePresenceCaretRequest, RuntimePresenceCarets,
  RuntimeRevisionInfo, UndoSelectionSnapshot,
};
use crate::io_util::{gate_call, send_reply};
use crate::local_write::{GateHolder, WriteGate};
use crate::presence::PresenceSelection;

const IO_REQUEST_CHANNEL_CAPACITY: usize = 256;
/// Maximum queued import blobs coalesced into one gate hold (spec §6.4). The
/// gate hold-time budget is the real bound; this caps pathological queues.
const IMPORT_COALESCE_MAX: usize = 16;

/// C-S6: a comment's original context — the historical projection at the
/// thread's birth frontier and the anchor resolved within it.
pub type CommentHistoryContext = (
  Box<DocumentProjection>,
  Option<(gpui_flowtext::DocumentOffset, gpui_flowtext::DocumentOffset)>,
);

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
    stamp: flowstate_document::RevisionStamp,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  /// H-S4: restore the document to a historical frontier as a forward op
  /// (always preceded by a safety pin — the runtime enforces the law).
  RestoreFrontier {
    frontier: Vec<u8>,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  /// H-S3 "Checkpoint now": mint a named pin at the current frontier.
  CreateNamedPin {
    title: String,
    reply: Sender<Result<u128>>,
  },
  /// T-S2: resolve a stored body cursor to its current paragraph (tub cards
  /// carry cursors so "open" lands on the card even after edits).
  ResolveCursorParagraph {
    cursor: Vec<u8>,
    reply: Sender<Result<Option<usize>>>,
  },
  /// H-S5: two-frontier diff + blame (None = vs now).
  FrontierDiff {
    base_frontier: Vec<u8>,
    newer_frontier: Option<Vec<u8>>,
    reply: Sender<Result<crate::crdt_runtime::RuntimeFrontierDiff>>,
  },
  /// H-S1: rename a revision record (naming pins it as a `named` tier).
  RenameRevision {
    revision_id: u128,
    title: String,
    reply: Sender<Result<()>>,
  },
  PackageBytes {
    title: String,
    reply: Sender<Result<Vec<u8>>>,
  },
  SavePackageTo {
    path: PathBuf,
    reply: Sender<Result<()>>,
  },
  /// §P3 (act two): revision-less cache flush at document close/idle so the
  /// next preview/open of this package takes the projection-cache fast path.
  FlushPackageCaches {
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
  Comments {
    reply: Sender<Result<Vec<RuntimeCommentThread>>>,
  },
  CreateComment {
    /// `None` = a general (unanchored) comment — the F3 decision.
    selection: Option<gpui_flowtext::EditorSelection>,
    body: String,
    author_user_id: u128,
    author_display_name: String,
    reply: Sender<Result<u128>>,
  },
  ReplyToComment {
    comment_id: u128,
    body: String,
    author_user_id: u128,
    author_display_name: String,
    reply: Sender<Result<u128>>,
  },
  SetCommentResolved {
    comment_id: u128,
    resolved: bool,
    reply: Sender<Result<()>>,
  },
  ReanchorComment {
    comment_id: u128,
    selection: gpui_flowtext::EditorSelection,
    reply: Sender<Result<()>>,
  },
  /// C-S6 history-jump: the projection at a comment's birth frontier plus its
  /// anchor resolved at that frontier.
  FrontierCommentContext {
    frontier: Vec<u8>,
    comment_id: u128,
    reply: Sender<Result<CommentHistoryContext>>,
  },
  EditCommentMessage {
    comment_id: u128,
    message_id: u128,
    body: String,
    actor_user_id: u128,
    reply: Sender<Result<()>>,
  },
  DeleteComment {
    comment_id: u128,
    actor_user_id: u128,
    reply: Sender<Result<()>>,
  },
  /// C-S1: author-gated per-message tombstone delete.
  DeleteCommentMessage {
    comment_id: u128,
    message_id: u128,
    actor_user_id: u128,
    reply: Sender<Result<()>>,
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
  /// H-K0: open a read-only view at an arbitrary encoded frontier (no named
  /// revision required) — history preview/tape and comment history-jump.
  OpenFrontier {
    frontier: Vec<u8>,
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
    IoRequest::RestoreFrontier { .. } => "restore-frontier",
    IoRequest::CreateNamedPin { .. } => "create-named-pin",
    IoRequest::ResolveCursorParagraph { .. } => "resolve-cursor-paragraph",
    IoRequest::FrontierDiff { .. } => "frontier-diff",
    IoRequest::RenameRevision { .. } => "rename-revision",
    IoRequest::PackageBytes { .. } => "package-bytes",
    IoRequest::SavePackageTo { .. } => "save-package-to",
    IoRequest::FlushPackageCaches { .. } => "flush-package-caches",
    IoRequest::TakeRestoredUndoSelection { .. } => "take-restored-undo-selection",
    IoRequest::AssetMetadata { .. } => "asset-metadata",
    IoRequest::Revisions { .. } => "revisions",
    IoRequest::Comments { .. } => "comments",
    IoRequest::CreateComment { .. } => "create-comment",
    IoRequest::ReplyToComment { .. } => "reply-to-comment",
    IoRequest::SetCommentResolved { .. } => "set-comment-resolved",
    IoRequest::ReanchorComment { .. } => "reanchor-comment",
    IoRequest::FrontierCommentContext { .. } => "frontier-comment-context",
    IoRequest::EditCommentMessage { .. } => "edit-comment-message",
    IoRequest::DeleteComment { .. } => "delete-comment",
    IoRequest::DeleteCommentMessage { .. } => "delete-comment-message",
    IoRequest::ProjectionFallbackStats { .. } => "projection-fallback-stats",
    IoRequest::PresenceSelection { .. } => "presence-selection",
    IoRequest::ResolvePresenceCarets { .. } => "resolve-presence-carets",
    IoRequest::SetAuthorIdentity { .. } => "set-author-identity",
    IoRequest::OpenRevision { .. } => "open-revision",
    IoRequest::OpenFrontier { .. } => "open-frontier",
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
      .spawn(move || {
        // §A13.1 (Act 13): Loro's state store decodes LAZILY — a
        // shallow-opened doc pays ~65ms of rich-text tree materialization on
        // its FIRST body-text access, which would otherwise land on the
        // user's first keystroke. Warm it here, off the interactive path,
        // right as the pump starts (the A13.2 priority lane lets a racing
        // local intent preempt the acquisition; the hold itself is the
        // one-time materialization we are choosing to pay early).
        if let Ok(guard) = core.lock(GateHolder::DocumentService) {
          let body = flowstate_document::loro_schema::body_text(guard.doc());
          body.iter(|_| true);
        }
        io_loop(&core, &receiver);
      })?;
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
    self
      .request(|reply| IoRequest::ImportRemoteUpdate { bytes, reply })
      .await
  }

  /// Drain committed-but-unpublished local events (call after local intents,
  /// debounced by the session).
  pub async fn pump_publish(&self) -> Result<Vec<RuntimeEvent>> {
    self.request(|reply| IoRequest::PumpPublish { reply }).await
  }

  pub async fn projection_snapshot(&self) -> Result<DocumentProjection> {
    self
      .request(|reply| IoRequest::ProjectionSnapshot { reply })
      .await
  }

  pub async fn oplog_version_vector(&self) -> Result<Vec<u8>> {
    self
      .request(|reply| IoRequest::OplogVersionVector { reply })
      .await
  }

  pub async fn export_updates_for(&self, remote_vv: Vec<u8>) -> Result<Vec<u8>> {
    self
      .request(|reply| IoRequest::ExportUpdatesFor { remote_vv, reply })
      .await
  }

  pub async fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    self
      .request(|reply| IoRequest::SnapshotBytes { reply })
      .await
  }

  pub async fn checkpoint_package(
    &self,
    title: String,
    path: Option<PathBuf>,
    stamp: flowstate_document::RevisionStamp,
  ) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::CheckpointPackage { title, path, stamp, reply })
      .await
  }

  pub async fn restore_frontier(&self, frontier: Vec<u8>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::RestoreFrontier { frontier, reply })
      .await
  }

  pub async fn create_named_pin(&self, title: String) -> Result<u128> {
    self
      .request(|reply| IoRequest::CreateNamedPin { title, reply })
      .await
  }

  pub async fn resolve_cursor_paragraph(&self, cursor: Vec<u8>) -> Result<Option<usize>> {
    self
      .request(|reply| IoRequest::ResolveCursorParagraph { cursor, reply })
      .await
  }

  pub async fn frontier_diff(
    &self,
    base_frontier: Vec<u8>,
    newer_frontier: Option<Vec<u8>>,
  ) -> Result<crate::crdt_runtime::RuntimeFrontierDiff> {
    self
      .request(|reply| IoRequest::FrontierDiff {
        base_frontier,
        newer_frontier,
        reply,
      })
      .await
  }

  pub async fn rename_revision(&self, revision_id: u128, title: String) -> Result<()> {
    self
      .request(|reply| IoRequest::RenameRevision { revision_id, title, reply })
      .await
  }

  pub async fn package_bytes(&self, title: String) -> Result<Vec<u8>> {
    self
      .request(|reply| IoRequest::PackageBytes { title, reply })
      .await
  }

  pub async fn save_package_to(&self, path: PathBuf) -> Result<()> {
    self
      .request(|reply| IoRequest::SavePackageTo { path, reply })
      .await
  }

  pub async fn flush_package_caches(&self) -> Result<()> {
    self
      .request(|reply| IoRequest::FlushPackageCaches { reply })
      .await
  }

  pub async fn take_restored_undo_selection(&self) -> Result<Option<UndoSelectionSnapshot>> {
    self
      .request(|reply| IoRequest::TakeRestoredUndoSelection { reply })
      .await
  }

  pub async fn asset_metadata(&self) -> Result<Vec<RuntimeAssetMetadata>> {
    self
      .request(|reply| IoRequest::AssetMetadata { reply })
      .await
  }

  pub async fn revisions(&self) -> Result<Vec<RuntimeRevisionInfo>> {
    self.request(|reply| IoRequest::Revisions { reply }).await
  }

  pub async fn comments(&self) -> Result<Vec<RuntimeCommentThread>> {
    self.request(|reply| IoRequest::Comments { reply }).await
  }

  pub async fn create_comment(
    &self,
    selection: Option<gpui_flowtext::EditorSelection>,
    body: String,
    author_user_id: u128,
    author_display_name: String,
  ) -> Result<u128> {
    self
      .request(|reply| IoRequest::CreateComment {
        selection,
        body,
        author_user_id,
        author_display_name,
        reply,
      })
      .await
  }

  pub async fn reply_to_comment(&self, comment_id: u128, body: String, author_user_id: u128, author_display_name: String) -> Result<u128> {
    self
      .request(|reply| IoRequest::ReplyToComment {
        comment_id,
        body,
        author_user_id,
        author_display_name,
        reply,
      })
      .await
  }

  pub async fn reanchor_comment(&self, comment_id: u128, selection: gpui_flowtext::EditorSelection) -> Result<()> {
    self
      .request(|reply| IoRequest::ReanchorComment { comment_id, selection, reply })
      .await
  }

  pub async fn frontier_comment_context(&self, frontier: Vec<u8>, comment_id: u128) -> Result<CommentHistoryContext> {
    self
      .request(|reply| IoRequest::FrontierCommentContext { frontier, comment_id, reply })
      .await
  }

  pub async fn set_comment_resolved(&self, comment_id: u128, resolved: bool) -> Result<()> {
    self
      .request(|reply| IoRequest::SetCommentResolved { comment_id, resolved, reply })
      .await
  }

  pub async fn edit_comment_message(&self, comment_id: u128, message_id: u128, body: String, actor_user_id: u128) -> Result<()> {
    self
      .request(|reply| IoRequest::EditCommentMessage {
        comment_id,
        message_id,
        body,
        actor_user_id,
        reply,
      })
      .await
  }

  pub async fn delete_comment(&self, comment_id: u128, actor_user_id: u128) -> Result<()> {
    self
      .request(|reply| IoRequest::DeleteComment {
        comment_id,
        actor_user_id,
        reply,
      })
      .await
  }

  pub async fn delete_comment_message(&self, comment_id: u128, message_id: u128, actor_user_id: u128) -> Result<()> {
    self
      .request(|reply| IoRequest::DeleteCommentMessage {
        comment_id,
        message_id,
        actor_user_id,
        reply,
      })
      .await
  }

  pub async fn projection_fallback_stats(&self) -> Result<crate::crdt_runtime::ProjectionFallbackStats> {
    self
      .request(|reply| IoRequest::ProjectionFallbackStats { reply })
      .await
  }

  pub async fn presence_selection(&self, selection: gpui_flowtext::EditorSelection) -> Result<Option<PresenceSelection>> {
    self
      .request(|reply| IoRequest::PresenceSelection { selection, reply })
      .await
  }

  pub async fn resolve_presence_carets(&self, requests: Vec<RuntimePresenceCaretRequest>) -> Result<RuntimePresenceCarets> {
    self
      .request(|reply| IoRequest::ResolvePresenceCarets { requests, reply })
      .await
  }

  pub async fn open_revision(&self, revision_id: u128) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::OpenRevision { revision_id, reply })
      .await
  }

  /// H-K0: read-only historical view at `frontier` (encoded Frontiers blob).
  pub async fn open_frontier(&self, frontier: Vec<u8>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::OpenFrontier { frontier, reply })
      .await
  }

  pub async fn fork_revision(&self, revision_id: u128) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::ForkRevision { revision_id, reply })
      .await
  }

  /// Record fetched asset bytes into canonical state so saves/packages made by
  /// this replica include assets it pulled from peers.
  pub async fn record_assets(&self, assets: Vec<flowstate_document::AssetRecord>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| IoRequest::RecordAssets { assets, reply })
      .await
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

fn io_loop(core: &Arc<WriteGate<CrdtRuntime>>, receiver: &Receiver<IoRequest>) {
  // Buffered non-import requests popped while coalescing an import chunk.
  let mut deferred: Vec<IoRequest> = Vec::new();
  // Bytes imported since the diff-calculator/history caches were last freed.
  // Bursts free at chunk end; a steady one-blob drip previously NEVER freed
  // (the old `coalesced > 1` gate), so the caches grew without bound across a
  // session — part of the receiving peer's 13.1 GB/run import churn.
  let mut unfreed_import_bytes: usize = 0;
  const IMPORT_CACHE_FREE_BYTES: usize = 4 * 1024 * 1024;
  // §perf-heaven T8.18 (first cold import): warm the retained import calculator's
  // trackers ONCE when this doc first shows collaboration activity, so the first
  // remote import reuses the built index rather than paying the cold tracker
  // build synchronously on the receive path. Gated so a single-user doc never
  // pays for it.
  let mut import_calc_warmed = false;
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
        // §A14.1.1: the coalescer respects the sender's mass-op slicing — a
        // BYTE budget stops it from gluing chunked slices back into one
        // mass hold (which would resurrect the ~240ms typing stall the
        // chunking exists to kill). Small drips still coalesce as before.
        const IMPORT_COALESCE_BYTE_BUDGET: usize = 64 * 1024;
        let mut coalesced_bytes = bytes.len();
        let mut chunk: Vec<(Vec<u8>, ReplySender<Vec<RuntimeEvent>>)> = vec![(bytes, reply)];
        while chunk.len() < IMPORT_COALESCE_MAX && coalesced_bytes < IMPORT_COALESCE_BYTE_BUDGET {
          match receiver.try_recv() {
            Ok(IoRequest::ImportRemoteUpdate { bytes, reply }) => {
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
            // §act-five P1.A: a SECOND drain after the gate is held. Acquiring the
            // gate can BLOCK behind a checkpoint/save; every remote blob that
            // arrived during that wait is now queued. Folding them into this same
            // chunk makes a steady remote drip coalesce into ONE import + ONE
            // derive (each import pays a fixed per-call rich-text tracker rebuild,
            // so fewer imports is a direct win) — with NO added latency, since we
            // were already blocked on the gate.
            while chunk.len() < IMPORT_COALESCE_MAX && coalesced_bytes < IMPORT_COALESCE_BYTE_BUDGET {
              match receiver.try_recv() {
                Ok(IoRequest::ImportRemoteUpdate { bytes, reply }) => {
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
            let coalesced = chunk.len();
            // §6.4 + field fix 2026-07-07: ONE batched import for the whole
            // chunk — N cheap Loro imports, then ONE projection derive — where
            // the old shape ran a full derive per blob (41.6% of a receiving
            // peer's runtime).
            let blobs: Vec<&[u8]> = chunk.iter().map(|(bytes, _)| bytes.as_slice()).collect();
            unfreed_import_bytes += blobs.iter().map(|bytes| bytes.len()).sum::<usize>();
            match guard.import_remote_updates(&blobs) {
              Ok(event_batches) => {
                for ((_, reply), events) in chunk.iter().zip(event_batches) {
                  send_reply(reply, Ok(events));
                }
              },
              Err(error) => {
                let error = format!("{error:#}");
                for (_, reply) in &chunk {
                  send_reply(reply, Err(anyhow::anyhow!("batched remote import failed: {error}")));
                }
              },
            }
            // Spec §6.4 memory hygiene: imports build diff-calculator and
            // history caches; free them at burst end OR once a steady drip
            // has accumulated a real footprint (OOM history). Cheap no-ops
            // when the caches are cold.
            if coalesced > 1 || unfreed_import_bytes >= IMPORT_CACHE_FREE_BYTES {
              guard.doc().free_diff_calculator();
              guard.doc().free_history_cache();
              unfreed_import_bytes = 0;
            }
            if coalesced > 1 {
              tracing::debug!(coalesced, "coalesced import chunk under one gate hold");
            }
          },
          Err(poisoned) => {
            for (_, reply) in chunk {
              send_reply(&reply, Err(anyhow::anyhow!(poisoned)));
            }
          },
        }
      },
      IoRequest::PumpPublish { reply } => {
        let result = core
          .lock(GateHolder::ExportUpdates)
          .map(|mut guard| {
            // §perf-heaven T8.18: the first publish marks this doc as an ACTIVE
            // collaboration participant. Warm the retained import calculator now
            // (off the receive path) so the FIRST remote import reuses the
            // id_to_cursor index instead of building it cold. Safe: a stale warmed
            // tracker is rebuilt by `start_tracking`'s validity guard, so warming
            // can only save work, never change a computed diff.
            if !import_calc_warmed {
              guard.doc().inner().warm_import_diff_calculator();
              import_calc_warmed = true;
            }
            guard.take_pending_publish()
          })
          .map_err(|poisoned| anyhow::anyhow!(poisoned));
        send_reply(&reply, result);
      },
      IoRequest::ProjectionSnapshot { reply } => send_reply(
        &reply,
        gate_call(core, GateHolder::DocumentService, |runtime| runtime.projection_snapshot()),
      ),
      IoRequest::OplogVersionVector { reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::ExportUpdates, |runtime| Ok(runtime.doc().oplog_vv().encode())),
        );
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
        //
        // §A12.1.3: a SHALLOW session's fork exports a silently shallow
        // snapshot — a joiner bootstrapped from it would be shallow AND
        // package-less, unable to absorb below-root history later. Clone the
        // package under the gate too (memcpy-scale, rare — joins only) and
        // reconstruct the FULL doc off-gate so joiners keep full history.
        let forked = gate_call(core, GateHolder::ExportFork, |runtime| {
          let package = runtime
            .doc()
            .is_shallow()
            .then(|| runtime.package().cloned())
            .flatten();
          Ok((runtime.doc().fork(), package))
        });
        let result = forked.and_then(|(fork, package)| {
          let source = match package {
            Some(package) => package
              .reconstruct_full_doc(&fork)
              .context("reconstructing full history for a join snapshot")?,
            None => fork,
          };
          source
            .export(ExportMode::Snapshot)
            .context("exporting Loro snapshot from fork")
        });
        send_reply(&reply, result);
      },
      IoRequest::RestoreFrontier { frontier, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| runtime.restore_frontier(&frontier)),
        );
      },
      IoRequest::CreateNamedPin { title, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            let title = title.trim();
            anyhow::ensure!(!title.is_empty(), "A checkpoint name cannot be empty");
            runtime.mint_named_pin_now(title)
          }),
        );
      },
      IoRequest::ResolveCursorParagraph { cursor, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            Ok(runtime.resolve_selection_anchor(&cursor, &cursor).map(|(head, _)| head.paragraph))
          }),
        );
      },
      IoRequest::FrontierDiff {
        base_frontier,
        newer_frontier,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.frontier_diff_vs(&base_frontier, newer_frontier.as_deref())
          }),
        );
      },
      IoRequest::RenameRevision { revision_id, title, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.rename_revision(revision_id, &title)
          }),
        );
      },
      IoRequest::CheckpointPackage { title, path, stamp, reply } => {
        // Field fix 2026-07-07: split checkpoint — live-doc phase under a
        // short gate hold, heavy assembly + disk write on a detached worker,
        // package restored under the gate afterwards. First-save (no package
        // yet) falls back to the in-place path.
        let begun = gate_call(core, GateHolder::DocumentService, |runtime| {
          runtime
            .begin_checkpoint(&title, path.clone(), &stamp)
            .map_err(anyhow::Error::from)
        });
        match begun {
          Ok(Some((job, events))) => {
            let core = Arc::clone(core);
            if let Err(error) = std::thread::Builder::new()
              .name("flowstate-checkpoint".to_string())
              .spawn(move || {
                let (package, wrote) = job.run();
                let restore = core
                  .lock(GateHolder::DocumentService)
                  .map(|mut guard| match wrote {
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
            send_reply(
              &reply,
              gate_call(core, GateHolder::DocumentService, |runtime| {
                runtime
                  .checkpoint_package(&title, path, &stamp)
                  .map_err(anyhow::Error::from)
              }),
            );
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
            .begin_checkpoint("Save", Some(path.clone()), &flowstate_document::RevisionStamp::session())
            .map_err(anyhow::Error::from)
        });
        match begun {
          Ok(Some((job, _events))) => {
            let core = Arc::clone(core);
            if let Err(error) = std::thread::Builder::new()
              .name("flowstate-save".to_string())
              .spawn(move || {
                let (package, wrote) = job.run();
                let restore = core
                  .lock(GateHolder::DocumentService)
                  .map(|mut guard| match wrote {
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
            send_reply(
              &reply,
              gate_call(core, GateHolder::DocumentService, |runtime| {
                runtime.save_package_to(path).map_err(anyhow::Error::from)
              }),
            );
          },
          Err(error) => send_reply(&reply, Err(error)),
        }
      },
      IoRequest::FlushPackageCaches { reply } => {
        // Same split shape as SavePackageTo: brief gate hold to fork +
        // check out the package, heavy assembly + write off-thread, restore
        // under the gate. `None` = no package or cache already current.
        let begun = gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.begin_cache_flush()));
        match begun {
          Ok(Some(job)) => {
            let core = Arc::clone(core);
            if let Err(error) = std::thread::Builder::new()
              .name("flowstate-cache-flush".to_string())
              .spawn(move || {
                let (package, wrote) = job.run();
                let restore = core
                  .lock(GateHolder::DocumentService)
                  .map(|mut guard| match wrote {
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
              tracing::error!(%error, "spawning cache-flush worker failed");
            }
          },
          Ok(None) => send_reply(&reply, Ok(())),
          Err(error) => send_reply(&reply, Err(error)),
        }
      },
      IoRequest::TakeRestoredUndoSelection { reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.take_restored_undo_selection())),
        );
      },
      IoRequest::AssetMetadata { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| runtime.asset_metadata()));
      },
      IoRequest::Revisions { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.revisions())));
      },
      IoRequest::Comments { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.comments())));
      },
      IoRequest::CreateComment {
        selection,
        body,
        author_user_id,
        author_display_name,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.create_comment(selection.as_ref(), &body, author_user_id, &author_display_name)
          }),
        );
      },
      IoRequest::ReplyToComment {
        comment_id,
        body,
        author_user_id,
        author_display_name,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.reply_to_comment(comment_id, &body, author_user_id, &author_display_name)
          }),
        );
      },
      IoRequest::SetCommentResolved { comment_id, resolved, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.set_comment_resolved(comment_id, resolved)
          }),
        );
      },
      IoRequest::ReanchorComment { comment_id, selection, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.reanchor_comment(comment_id, &selection)
          }),
        );
      },
      IoRequest::FrontierCommentContext { frontier, comment_id, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime
              .frontier_comment_context(&frontier, comment_id)
              .map(|(document, anchor)| (Box::new(document), anchor))
          }),
        );
      },
      IoRequest::EditCommentMessage {
        comment_id,
        message_id,
        body,
        actor_user_id,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.edit_comment_message(comment_id, message_id, &body, actor_user_id)
          }),
        );
      },
      IoRequest::DeleteComment {
        comment_id,
        actor_user_id,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.delete_comment(comment_id, actor_user_id)
          }),
        );
      },
      IoRequest::DeleteCommentMessage {
        comment_id,
        message_id,
        actor_user_id,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.delete_comment_message(comment_id, message_id, actor_user_id)
          }),
        );
      },
      IoRequest::ProjectionFallbackStats { reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.projection_fallback_stats())),
        );
      },
      IoRequest::PresenceSelection { selection, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::Presence, |runtime| Ok(runtime.presence_selection(&selection))),
        );
      },
      IoRequest::ResolvePresenceCarets { requests, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::Presence, |runtime| Ok(runtime.resolve_presence_carets(requests))),
        );
      },
      IoRequest::SetAuthorIdentity {
        user_id,
        display_name,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.set_author_identity(user_id, display_name)
          }),
        );
      },
      IoRequest::OpenRevision { revision_id, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.command(crate::crdt_runtime::SemanticCommand::OpenRevision { revision_id })
          }),
        );
      },
      IoRequest::ForkRevision { revision_id, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.command(crate::crdt_runtime::SemanticCommand::ForkRevision { revision_id })
          }),
        );
      },
      IoRequest::OpenFrontier { frontier, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.command(crate::crdt_runtime::SemanticCommand::OpenFrontier { frontier })
          }),
        );
      },
      IoRequest::RecordAssets { assets, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| runtime.merge_asset_records(assets)),
        );
      },
    }
    let elapsed_ms = started.elapsed().as_millis();
    if elapsed_ms > 250 {
      tracing::error!(kind, elapsed_ms, "slow doc I/O request");
    }
  }
}
