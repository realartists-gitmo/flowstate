//! `FlowIoService` — the flow's background I/O service (flow architecture
//! spec Part 2.2 / S6), the `DocIoService` sibling for `.fl0` documents. It
//! shares the flow core with the UI thread's [`super::FlowDocHandle`] through
//! the write gate and acquires it for every doc-touching operation (I-9a).
//! Duties are the transport-facing subset:
//!
//! * remote import chunks — the §6.4 coalescer shape verbatim (batch budget +
//!   the P1.A second drain behind a held gate, deferred non-import ordering);
//! * draining the publish queue filled by gate-held local commits;
//! * update/snapshot exports (snapshots fork under the gate, export off it);
//! * `.fl0` persistence: atomic saves and recovery-file encodes.
//!
//! Package/checkpoint/revision/asset services are `.db8`-only and
//! deliberately absent — a flow document IS its snapshot. Comments joined in
//! C-S2 (the formats-are-peers law): they live in the flow doc itself
//! (`flow.comments_by_id`), so no package machinery is needed.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};
use flowstate_flow::FlowBoardProjection;
use loro::{ExportMode, VersionVector};

use super::runtime::{FlowCommentThread, FlowPublishEvent, FlowRuntime};
use flowstate_flow::CellId;
use crate::io_util::{gate_call, send_reply};
use crate::local_write::{GateHolder, WriteGate};

const IO_REQUEST_CHANNEL_CAPACITY: usize = 256;
/// Maximum queued import blobs coalesced into one gate hold (spec §6.4).
const IMPORT_COALESCE_MAX: usize = 16;

/// Reply channel alias (clippy type-complexity).
type ReplySender<T> = Sender<anyhow::Result<T>>;

pub enum FlowIoRequest {
  ImportRemoteUpdate {
    bytes: Vec<u8>,
    reply: ReplySender<()>,
  },
  /// Drain the publish queue filled by gate-held local commits for network
  /// broadcast.
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
  /// Atomic `.fl0` write (fork under the gate, encode + write off it).
  SaveTo {
    path: PathBuf,
    reply: ReplySender<()>,
  },
  /// Framed `.fl0` bytes without touching disk (recovery files).
  EncodeBytes {
    reply: ReplySender<Vec<u8>>,
  },
  Comments {
    reply: ReplySender<Vec<FlowCommentThread>>,
  },
  CreateComment {
    cell: Option<CellId>,
    body: String,
    author_user_id: u128,
    author_display_name: String,
    reply: ReplySender<u128>,
  },
  ReplyToComment {
    comment_id: u128,
    body: String,
    author_user_id: u128,
    author_display_name: String,
    reply: ReplySender<u128>,
  },
  SetCommentResolved {
    comment_id: u128,
    resolved: bool,
    reply: ReplySender<()>,
  },
  EditCommentMessage {
    comment_id: u128,
    message_id: u128,
    body: String,
    actor_user_id: u128,
    reply: ReplySender<()>,
  },
  DeleteCommentMessage {
    comment_id: u128,
    message_id: u128,
    actor_user_id: u128,
    reply: ReplySender<()>,
  },
  DeleteComment {
    comment_id: u128,
    actor_user_id: u128,
    reply: ReplySender<()>,
  },
}

fn io_request_kind(request: &FlowIoRequest) -> &'static str {
  match request {
    FlowIoRequest::ImportRemoteUpdate { .. } => "import-remote-update",
    FlowIoRequest::PumpPublish { .. } => "pump-publish",
    FlowIoRequest::BoardSnapshot { .. } => "board-snapshot",
    FlowIoRequest::OplogVersionVector { .. } => "oplog-version-vector",
    FlowIoRequest::ExportUpdatesFor { .. } => "export-updates-for",
    FlowIoRequest::SnapshotBytes { .. } => "snapshot-bytes",
    FlowIoRequest::SaveTo { .. } => "save-to",
    FlowIoRequest::EncodeBytes { .. } => "encode-bytes",
    FlowIoRequest::Comments { .. } => "comments",
    FlowIoRequest::CreateComment { .. } => "create-comment",
    FlowIoRequest::ReplyToComment { .. } => "reply-to-comment",
    FlowIoRequest::SetCommentResolved { .. } => "set-comment-resolved",
    FlowIoRequest::EditCommentMessage { .. } => "edit-comment-message",
    FlowIoRequest::DeleteCommentMessage { .. } => "delete-comment-message",
    FlowIoRequest::DeleteComment { .. } => "delete-comment",
  }
}

/// Cloneable handle to the flow I/O service thread.
#[derive(Clone)]
pub struct FlowIoHandle {
  requests: Sender<FlowIoRequest>,
}

impl FlowIoHandle {
  /// Spawn the I/O service over a shared, gate-protected flow core.
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

  /// Drain committed-but-unpublished local events (call after local intents,
  /// debounced by the session).
  pub async fn pump_publish(&self) -> Result<Vec<FlowPublishEvent>> {
    self
      .request(|reply| FlowIoRequest::PumpPublish { reply })
      .await
  }

  pub async fn board_snapshot(&self) -> Result<FlowBoardProjection> {
    self
      .request(|reply| FlowIoRequest::BoardSnapshot { reply })
      .await
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
    self
      .request(|reply| FlowIoRequest::SnapshotBytes { reply })
      .await
  }

  pub async fn save_to(&self, path: PathBuf) -> Result<()> {
    self
      .request(|reply| FlowIoRequest::SaveTo { path, reply })
      .await
  }

  pub async fn encode_bytes(&self) -> Result<Vec<u8>> {
    self
      .request(|reply| FlowIoRequest::EncodeBytes { reply })
      .await
  }

  pub async fn comments(&self) -> Result<Vec<FlowCommentThread>> {
    self.request(|reply| FlowIoRequest::Comments { reply }).await
  }

  pub async fn create_comment(
    &self,
    cell: Option<CellId>,
    body: String,
    author_user_id: u128,
    author_display_name: String,
  ) -> Result<u128> {
    self
      .request(|reply| FlowIoRequest::CreateComment {
        cell,
        body,
        author_user_id,
        author_display_name,
        reply,
      })
      .await
  }

  pub async fn reply_to_comment(&self, comment_id: u128, body: String, author_user_id: u128, author_display_name: String) -> Result<u128> {
    self
      .request(|reply| FlowIoRequest::ReplyToComment {
        comment_id,
        body,
        author_user_id,
        author_display_name,
        reply,
      })
      .await
  }

  pub async fn set_comment_resolved(&self, comment_id: u128, resolved: bool) -> Result<()> {
    self
      .request(|reply| FlowIoRequest::SetCommentResolved { comment_id, resolved, reply })
      .await
  }

  pub async fn edit_comment_message(&self, comment_id: u128, message_id: u128, body: String, actor_user_id: u128) -> Result<()> {
    self
      .request(|reply| FlowIoRequest::EditCommentMessage {
        comment_id,
        message_id,
        body,
        actor_user_id,
        reply,
      })
      .await
  }

  pub async fn delete_comment_message(&self, comment_id: u128, message_id: u128, actor_user_id: u128) -> Result<()> {
    self
      .request(|reply| FlowIoRequest::DeleteCommentMessage {
        comment_id,
        message_id,
        actor_user_id,
        reply,
      })
      .await
  }

  pub async fn delete_comment(&self, comment_id: u128, actor_user_id: u128) -> Result<()> {
    self
      .request(|reply| FlowIoRequest::DeleteComment {
        comment_id,
        actor_user_id,
        reply,
      })
      .await
  }
}

fn io_loop(core: &Arc<WriteGate<FlowRuntime>>, receiver: &Receiver<FlowIoRequest>) {
  // Buffered non-import requests popped while coalescing an import chunk.
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
    let kind = io_request_kind(&request);
    let started = std::time::Instant::now();
    match request {
      FlowIoRequest::ImportRemoteUpdate { bytes, reply } => {
        // Spec §6.4: coalesce immediately-available import blobs into ONE gate
        // acquisition, byte-budgeted so a mass-op slice train is never glued
        // back into one mass hold. Non-import requests popped while draining
        // are deferred (processed right after, order preserved among
        // themselves).
        const IMPORT_COALESCE_BYTE_BUDGET: usize = 64 * 1024;
        let mut coalesced_bytes = bytes.len();
        let mut chunk: Vec<(Vec<u8>, ReplySender<()>)> = vec![(bytes, reply)];
        let drain = |chunk: &mut Vec<(Vec<u8>, ReplySender<()>)>, coalesced_bytes: &mut usize, deferred: &mut Vec<FlowIoRequest>| {
          while chunk.len() < IMPORT_COALESCE_MAX && *coalesced_bytes < IMPORT_COALESCE_BYTE_BUDGET {
            match receiver.try_recv() {
              Ok(FlowIoRequest::ImportRemoteUpdate { bytes, reply }) => {
                *coalesced_bytes += bytes.len();
                chunk.push((bytes, reply));
              },
              Ok(other) => {
                deferred.push(other);
                break;
              },
              Err(_) => break,
            }
          }
        };
        drain(&mut chunk, &mut coalesced_bytes, &mut deferred);
        match core.lock(GateHolder::ImportChunk) {
          Ok(mut guard) => {
            // §act-five P1.A: a SECOND drain after the gate is held — every
            // remote blob that arrived while blocked folds into this same
            // chunk (one import + one derive) with no added latency.
            drain(&mut chunk, &mut coalesced_bytes, &mut deferred);
            let coalesced = chunk.len();
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
                  send_reply(reply, Err(anyhow::anyhow!("batched flow remote import failed: {error}")));
                }
              },
            }
            if coalesced > 1 {
              tracing::debug!(coalesced, "coalesced flow import chunk under one gate hold");
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
        gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.board().clone())),
      ),
      FlowIoRequest::OplogVersionVector { reply } => send_reply(
        &reply,
        gate_call(core, GateHolder::ExportUpdates, |runtime| Ok(runtime.oplog_vv().encode())),
      ),
      FlowIoRequest::ExportUpdatesFor { remote_vv, reply } => {
        let result = VersionVector::decode(&remote_vv)
          .context("decoding remote Loro version vector")
          .and_then(|vv| gate_call(core, GateHolder::ExportUpdates, |runtime| runtime.export_updates_for(&vv)));
        send_reply(&reply, result);
      },
      FlowIoRequest::SnapshotBytes { reply } => {
        // I-9a long-export rule: fork under the gate (brief), export off it.
        // Flow docs are never shallow (no package machinery), so the fork IS
        // the full history.
        let result = fork_off_gate(core).and_then(|fork| {
          fork
            .export(ExportMode::Snapshot)
            .context("exporting Loro snapshot from flow fork")
        });
        send_reply(&reply, result);
      },
      FlowIoRequest::SaveTo { path, reply } => {
        let result = fork_off_gate(core)
          .and_then(|fork| {
            fork
              .export(ExportMode::Snapshot)
              .context("exporting Loro snapshot for .fl0 save")
          })
          .and_then(|snapshot| flowstate_flow::persistence::save_snapshot_to(&path, &snapshot));
        send_reply(&reply, result);
      },
      FlowIoRequest::EncodeBytes { reply } => {
        let result = fork_off_gate(core)
          .and_then(|fork| {
            fork
              .export(ExportMode::Snapshot)
              .context("exporting Loro snapshot for .fl0 encode")
          })
          .and_then(|snapshot| flowstate_flow::persistence::encode_snapshot(&snapshot));
        send_reply(&reply, result);
      },
      FlowIoRequest::Comments { reply } => {
        send_reply(&reply, gate_call(core, GateHolder::DocumentService, |runtime| Ok(runtime.flow_comments())));
      },
      FlowIoRequest::CreateComment {
        cell,
        body,
        author_user_id,
        author_display_name,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.create_flow_comment(cell, &body, author_user_id, &author_display_name)
          }),
        );
      },
      FlowIoRequest::ReplyToComment {
        comment_id,
        body,
        author_user_id,
        author_display_name,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.reply_to_flow_comment(comment_id, &body, author_user_id, &author_display_name)
          }),
        );
      },
      FlowIoRequest::SetCommentResolved { comment_id, resolved, reply } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.set_flow_comment_resolved(comment_id, resolved)
          }),
        );
      },
      FlowIoRequest::EditCommentMessage {
        comment_id,
        message_id,
        body,
        actor_user_id,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.edit_flow_comment_message(comment_id, message_id, &body, actor_user_id)
          }),
        );
      },
      FlowIoRequest::DeleteCommentMessage {
        comment_id,
        message_id,
        actor_user_id,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.delete_flow_comment_message(comment_id, message_id, actor_user_id)
          }),
        );
      },
      FlowIoRequest::DeleteComment {
        comment_id,
        actor_user_id,
        reply,
      } => {
        send_reply(
          &reply,
          gate_call(core, GateHolder::DocumentService, |runtime| {
            runtime.delete_flow_comment(comment_id, actor_user_id)
          }),
        );
      },
    }
    let elapsed_ms = started.elapsed().as_millis();
    if elapsed_ms > 250 {
      tracing::error!(kind, elapsed_ms, "slow flow I/O request");
    }
  }
}

/// Fork under a brief gate hold; the caller exports/encodes off the gate.
fn fork_off_gate(core: &Arc<WriteGate<FlowRuntime>>) -> Result<loro::LoroDoc> {
  gate_call(core, GateHolder::ExportFork, |runtime| Ok(runtime.fork_for_export()))
}
