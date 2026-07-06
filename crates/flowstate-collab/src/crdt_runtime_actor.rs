use std::{io, path::PathBuf, thread};

use anyhow::{Context as _, Result, anyhow};
use async_channel::{Receiver, Sender};
use flowstate_document::{AssetRecord, DocumentProjection};
use gpui_flowtext::{EditorSelection, SemanticEditCommand as EditorSemanticCommand};
use loro::{ExportMode, LoroDoc, VersionVector};
use rustc_hash::FxHashMap;

use crate::crdt_runtime::{
  CrdtRuntime, EditorCommitResult, ProjectionFallbackStats, RuntimeAssetMetadata, RuntimeEvent, RuntimePresenceCaretRequest,
  RuntimePresenceCarets, RuntimeRevisionInfo, SemanticCommand,
};
use crate::presence::PresenceSelection;

const RUNTIME_REQUEST_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct CrdtRuntimeHandle {
  commands: Sender<RuntimeRequest>,
  /// A shared read handle to the runtime's canonical Loro document. `LoroDoc::clone`
  /// is a reference-counted handle (not a deep fork), and the runtime only ever
  /// MUTATES its doc (never reassigns `self.doc`), so this stays live for the
  /// runtime's whole life and observes every edit. It lets read-only pulls
  /// (snapshot / updates export) be served WITHOUT queueing behind the mutation
  /// actor's FIFO — Loro's own internal locks make concurrent read-vs-edit safe.
  read_doc: LoroDoc,
}

impl CrdtRuntimeHandle {
  pub fn spawn(runtime: CrdtRuntime) -> io::Result<Self> {
    // Capture the shared read handle before the runtime moves onto its thread.
    let read_doc = runtime.doc().clone();
    // Bounded so a stalled runtime thread backpressures callers via
    // `send().await` instead of growing an unbounded request queue.
    let (commands, receiver) = async_channel::bounded(RUNTIME_REQUEST_CHANNEL_CAPACITY);
    thread::Builder::new()
      .name("flowstate-crdt-runtime".to_string())
      .spawn(move || runtime_loop(runtime, receiver))?;
    Ok(Self { commands, read_doc })
  }

  /// A shared read handle to the canonical Loro document, for serving read-only
  /// pulls off the mutation actor's queue. See [`Self::read_doc`] (the field).
  #[must_use]
  pub fn read_doc(&self) -> LoroDoc {
    self.read_doc.clone()
  }
}

impl CrdtRuntimeHandle {
  pub async fn apply_editor_commands(
    &self,
    transaction_id: u128,
    base_frontier: Vec<u8>,
    commands: Vec<EditorSemanticCommand>,
    assets: Vec<AssetRecord>,
    selection_after: Option<EditorSelection>,
  ) -> Result<EditorCommitResult> {
    self
      .request(|reply| RuntimeRequest::ApplyEditorCommands {
        transaction_id,
        base_frontier,
        commands,
        assets,
        selection_after,
        reply,
      })
      .await
  }

  pub async fn command(&self, command: SemanticCommand) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| RuntimeRequest::Command { command, reply })
      .await
  }

  pub async fn import_remote_update(&self, bytes: Vec<u8>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| RuntimeRequest::ImportRemoteUpdate { bytes, reply })
      .await
  }

  pub async fn projection_snapshot(&self) -> Result<DocumentProjection> {
    self
      .request(|reply| RuntimeRequest::ProjectionSnapshot { reply })
      .await
  }

  pub async fn oplog_version_vector(&self) -> Result<Vec<u8>> {
    self
      .request(|reply| RuntimeRequest::OplogVersionVector { reply })
      .await
  }

  pub async fn export_updates_for(&self, remote_vv: Vec<u8>) -> Result<Vec<u8>> {
    self
      .request(|reply| RuntimeRequest::ExportUpdatesFor { remote_vv, reply })
      .await
  }

  pub async fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    self
      .request(|reply| RuntimeRequest::SnapshotBytes { reply })
      .await
  }

  pub async fn checkpoint_package(&self, title: String, path: Option<PathBuf>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| RuntimeRequest::CheckpointPackage { title, path, reply })
      .await
  }

  pub async fn package_bytes(&self, title: String) -> Result<Vec<u8>> {
    self
      .request(|reply| RuntimeRequest::PackageBytes { title, reply })
      .await
  }

  pub async fn save_package_to(&self, path: PathBuf) -> Result<()> {
    self
      .request(|reply| RuntimeRequest::SavePackageTo { path, reply })
      .await
  }

  pub async fn take_restored_undo_selection(&self) -> Result<Option<crate::crdt_runtime::UndoSelectionSnapshot>> {
    self
      .request(|reply| RuntimeRequest::TakeRestoredUndoSelection { reply })
      .await
  }

  pub async fn asset_metadata(&self) -> Result<Vec<RuntimeAssetMetadata>> {
    self
      .request(|reply| RuntimeRequest::AssetMetadata { reply })
      .await
  }

  pub async fn revisions(&self) -> Result<Vec<RuntimeRevisionInfo>> {
    self
      .request(|reply| RuntimeRequest::Revisions { reply })
      .await
  }

  pub async fn projection_fallback_stats(&self) -> Result<ProjectionFallbackStats> {
    self
      .request(|reply| RuntimeRequest::ProjectionFallbackStats { reply })
      .await
  }

  pub async fn presence_selection(&self, selection: EditorSelection) -> Result<Option<PresenceSelection>> {
    self
      .request(|reply| RuntimeRequest::PresenceSelection { selection, reply })
      .await
  }

  pub async fn resolve_presence_carets(&self, requests: Vec<RuntimePresenceCaretRequest>) -> Result<RuntimePresenceCarets> {
    self
      .request(|reply| RuntimeRequest::ResolvePresenceCarets { requests, reply })
      .await
  }

  /// §15/§31: bind a stable durable author identity to the live runtime so later
  /// revisions record this user as their author and `users_by_id` is populated.
  pub async fn set_author_identity(&self, user_id: u128, display_name: Option<String>) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| RuntimeRequest::SetAuthorIdentity {
        user_id,
        display_name,
        reply,
      })
      .await
  }

  async fn request<T: Send + 'static>(&self, make: impl FnOnce(Sender<Result<T>>) -> RuntimeRequest) -> Result<T> {
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    self
      .commands
      .send(make(reply_tx))
      .await
      .map_err(|_| anyhow!("Flowstate CRDT runtime actor stopped"))?;
    reply_rx
      .recv()
      .await
      .map_err(|_| anyhow!("Flowstate CRDT runtime actor dropped its response"))?
  }
}

enum RuntimeRequest {
  ApplyEditorCommands {
    transaction_id: u128,
    base_frontier: Vec<u8>,
    commands: Vec<EditorSemanticCommand>,
    assets: Vec<AssetRecord>,
    selection_after: Option<EditorSelection>,
    reply: Sender<Result<EditorCommitResult>>,
  },
  Command {
    command: SemanticCommand,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
  },
  ImportRemoteUpdate {
    bytes: Vec<u8>,
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
    reply: Sender<Result<Option<crate::crdt_runtime::UndoSelectionSnapshot>>>,
  },
  AssetMetadata {
    reply: Sender<Result<Vec<RuntimeAssetMetadata>>>,
  },
  Revisions {
    reply: Sender<Result<Vec<RuntimeRevisionInfo>>>,
  },
  ProjectionFallbackStats {
    reply: Sender<Result<ProjectionFallbackStats>>,
  },
  PresenceSelection {
    selection: EditorSelection,
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
}

/// The request the CRDT actor is currently executing, shared with the hang
/// watchdog thread. `None` between requests (the actor is idle, blocked in
/// `recv_blocking`).
struct InFlightRequest {
  kind: &'static str,
  started: std::time::Instant,
  seq: u64,
}

fn runtime_loop(mut runtime: CrdtRuntime, receiver: Receiver<RuntimeRequest>) {
  // §hang-watchdog: the CRDT actor runs on its own thread and logs almost nothing,
  // yet a large-document collab session pegs it at ~100% CPU and never returns.
  // Two failure shapes need distinguishing:
  //   (a) a request FLOOD — many fast requests of one kind (a self-sustaining
  //       request feedback loop). Caught by the between-requests heartbeat below.
  //   (b) a SINGLE request that never returns — an infinite/quadratic loop inside
  //       one handler. `recv_blocking` never wakes, so the loop-body heartbeat can
  //       NEVER fire; only a SEPARATE thread can observe it. That is the watchdog
  //       thread below, which names the stuck handler even while it spins.
  // All watchdog output uses `error!` because the default log level is `error`
  // (a `warn!` here is filtered out and silently produces nothing).
  let in_flight: std::sync::Arc<std::sync::Mutex<Option<InFlightRequest>>> =
    std::sync::Arc::new(std::sync::Mutex::new(None));
  spawn_hang_watchdog(std::sync::Arc::downgrade(&in_flight));

  // §perf: telemetry keyed by trusted &'static handler-name strings — Fx hashing
  // is faster and no DoS resistance is needed (keys are not remote-controlled).
  let mut request_counts: FxHashMap<&'static str, u64> = FxHashMap::default();
  let mut total_requests: u64 = 0;
  let mut last_progress = std::time::Instant::now();
  while let Ok(request) = receiver.recv_blocking() {
    let kind = runtime_request_kind(&request);
    if last_progress.elapsed() >= std::time::Duration::from_millis(250) {
      tracing::error!("CRDT actor busy (hang watchdog): entering {kind}; {total_requests} requests so far {request_counts:?}");
      last_progress = std::time::Instant::now();
    }
    if let Ok(mut slot) = in_flight.lock() {
      *slot = Some(InFlightRequest { kind, started: std::time::Instant::now(), seq: total_requests + 1 });
    }
    let request_started = std::time::Instant::now();
    match request {
      RuntimeRequest::ApplyEditorCommands {
        transaction_id,
        base_frontier,
        commands,
        assets,
        selection_after,
        reply,
      } => {
        let commands = coalesce_editor_commands(commands);
        send_reply(
          reply,
          runtime.apply_editor_transaction(transaction_id, &base_frontier, &commands, &assets, selection_after.as_ref()),
        );
      },
      RuntimeRequest::Command { command, reply } => send_reply(reply, runtime.command(command)),
      RuntimeRequest::ImportRemoteUpdate { bytes, reply } => send_reply(reply, runtime.import_remote_update(&bytes)),
      RuntimeRequest::ProjectionSnapshot { reply } => send_reply(reply, runtime.projection_snapshot()),
      RuntimeRequest::OplogVersionVector { reply } => send_reply(reply, Ok(runtime.doc().oplog_vv().encode())),
      RuntimeRequest::ExportUpdatesFor { remote_vv, reply } => {
        let result = VersionVector::decode(&remote_vv)
          .context("decoding remote Loro version vector")
          .and_then(|vv| runtime.export_updates_for(&vv));
        send_reply(reply, result);
      },
      RuntimeRequest::SnapshotBytes { reply } => {
        send_reply(
          reply,
          runtime
            .doc()
            .export(ExportMode::Snapshot)
            .context("exporting Loro snapshot"),
        );
      },
      RuntimeRequest::CheckpointPackage { title, path, reply } => {
        send_reply(reply, runtime.checkpoint_package(&title, path).map_err(Into::into));
      },
      RuntimeRequest::PackageBytes { title, reply } => {
        send_reply(reply, runtime.package_bytes(&title).map_err(Into::into));
      },
      RuntimeRequest::SavePackageTo { path, reply } => {
        send_reply(reply, runtime.save_package_to(path).map_err(Into::into));
      },
      RuntimeRequest::TakeRestoredUndoSelection { reply } => {
        send_reply(reply, Ok(runtime.take_restored_undo_selection()));
      },
      RuntimeRequest::AssetMetadata { reply } => send_reply(reply, runtime.asset_metadata()),
      RuntimeRequest::Revisions { reply } => send_reply(reply, Ok(runtime.revisions())),
      RuntimeRequest::ProjectionFallbackStats { reply } => send_reply(reply, Ok(runtime.projection_fallback_stats())),
      RuntimeRequest::PresenceSelection { selection, reply } => {
        send_reply(reply, Ok(runtime.presence_selection(&selection)));
      },
      RuntimeRequest::ResolvePresenceCarets { requests, reply } => {
        send_reply(reply, Ok(runtime.resolve_presence_carets(requests)));
      },
      RuntimeRequest::SetAuthorIdentity {
        user_id,
        display_name,
        reply,
      } => {
        send_reply(reply, runtime.set_author_identity(user_id, display_name));
      },
    }
    if let Ok(mut slot) = in_flight.lock() {
      *slot = None;
    }
    let request_ms = request_started.elapsed().as_millis();
    total_requests += 1;
    *request_counts.entry(kind).or_insert(0) += 1;
    if request_ms > 250 {
      tracing::error!("slow CRDT actor request (hang watchdog): {kind} took {request_ms}ms (request #{total_requests})");
    }
  }
}

/// Watches the actor's in-flight request from a separate thread so a handler
/// stuck in an infinite loop (never returning to `recv_blocking`) is still named
/// in the log. Exits when the actor loop drops its `Arc` (the `Weak` fails to
/// upgrade). See `runtime_loop`'s §hang-watchdog note for why this must be a
/// distinct thread.
fn spawn_hang_watchdog(in_flight: std::sync::Weak<std::sync::Mutex<Option<InFlightRequest>>>) {
  let _ = std::thread::Builder::new()
    .name("flowstate-crdt-hang-watchdog".to_string())
    .spawn(move || {
      let mut last_reported_seq: Option<u64> = None;
      loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let Some(slot) = in_flight.upgrade() else {
          break;
        };
        let Ok(guard) = slot.lock() else {
          continue;
        };
        if let Some(current) = guard.as_ref() {
          let elapsed_ms = current.started.elapsed().as_millis();
          if elapsed_ms > 1000 {
            // Fires every 500ms while the SAME request stays in-flight, so the log
            // shows the handler is spinning (not just slow). `first-seen` marks the
            // transition so the culprit stands out from the repeats.
            let marker = if last_reported_seq == Some(current.seq) { "still stuck" } else { "STUCK (first-seen)" };
            tracing::error!(
              "CRDT actor {marker}: request #{} kind={} has not returned in {elapsed_ms}ms — likely an infinite/quadratic loop inside this handler",
              current.seq,
              current.kind,
            );
            last_reported_seq = Some(current.seq);
          }
        }
      }
    });
}

/// Short stable name of a runtime request, for the actor-loop hang watchdog.
fn runtime_request_kind(request: &RuntimeRequest) -> &'static str {
  match request {
    RuntimeRequest::ApplyEditorCommands { .. } => "apply-editor-commands",
    RuntimeRequest::Command { .. } => "command",
    RuntimeRequest::ImportRemoteUpdate { .. } => "import-remote-update",
    RuntimeRequest::ProjectionSnapshot { .. } => "projection-snapshot",
    RuntimeRequest::OplogVersionVector { .. } => "oplog-version-vector",
    RuntimeRequest::ExportUpdatesFor { .. } => "export-updates-for",
    RuntimeRequest::SnapshotBytes { .. } => "snapshot-bytes",
    RuntimeRequest::ResolvePresenceCarets { .. } => "resolve-presence-carets",
    RuntimeRequest::PresenceSelection { .. } => "presence-selection",
    _ => "other",
  }
}

fn coalesce_editor_commands(commands: Vec<EditorSemanticCommand>) -> Vec<EditorSemanticCommand> {
  let mut coalesced = Vec::with_capacity(commands.len());
  for command in commands {
    if let EditorSemanticCommand::DeleteRange { range } = &command
      && let Some(EditorSemanticCommand::InsertText { at, text, .. }) = coalesced.last_mut()
      && range.start.paragraph == at.paragraph
      && range.end.paragraph == at.paragraph
      && range.end.byte == at.byte.saturating_add(text.len())
      && range.start.byte >= at.byte
      && range.start.byte <= range.end.byte
    {
      text.truncate(range.start.byte - at.byte);
      if text.is_empty() {
        coalesced.pop();
      }
      continue;
    }
    if let EditorSemanticCommand::InsertText { at, text, styles } = &command
      && let Some(EditorSemanticCommand::InsertText {
        at: previous_at,
        text: previous_text,
        styles: previous_styles,
      }) = coalesced.last_mut()
      && previous_at.paragraph == at.paragraph
      && *previous_styles == *styles
      && previous_at.byte.saturating_add(previous_text.len()) == at.byte
    {
      previous_text.push_str(text);
      continue;
    }
    coalesced.push(command);
  }
  coalesced
}

fn send_reply<T>(reply: Sender<Result<T>>, result: Result<T>) {
  let _ = reply.send_blocking(result);
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_document::{DocumentOffset, RunStyles};

  #[tokio::test]
  async fn stale_projection_error_survives_actor_boundary() -> Result<()> {
    let runtime = CrdtRuntime::new_empty("Actor stale frontier")?;
    let handle = CrdtRuntimeHandle::spawn(runtime)?;
    let base_frontier = handle.projection_snapshot().await?.frontier;

    handle
      .command(SemanticCommand::InsertText {
        unicode_index: 1,
        text: "remote".to_string(),
        styles: RunStyles::default(),
      })
      .await?;

    let error = handle
      .apply_editor_commands(
        1,
        base_frontier,
        vec![EditorSemanticCommand::InsertText {
          at: DocumentOffset { paragraph: 0, byte: 0 },
          text: "local".to_string(),
          styles: RunStyles::default(),
        }],
        Vec::new(),
        None,
      )
      .await
      .expect_err("stale editor commands must be rejected");

    assert!(
      error
        .downcast_ref::<crate::crdt_runtime::StaleProjectionError>()
        .is_some()
    );
    Ok(())
  }

  #[tokio::test]
  async fn set_author_identity_round_trips_through_actor() -> Result<()> {
    let runtime = CrdtRuntime::new_empty("Actor author identity")?;
    let handle = CrdtRuntimeHandle::spawn(runtime)?;

    handle
      .set_author_identity(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef, Some("Author".to_string()))
      .await?;

    // The runtime stays usable after binding the durable author identity.
    handle.projection_snapshot().await?;
    Ok(())
  }
}
