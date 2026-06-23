use std::{
  io,
  path::PathBuf,
  thread,
};

use anyhow::{Context as _, Result, anyhow};
use async_channel::{Receiver, Sender};
use flowstate_document::{AssetRecord, DocumentProjection};
use gpui_flowtext::{EditorSelection, SemanticEditCommand as EditorSemanticCommand};
use loro::{ExportMode, VersionVector};

use crate::crdt_runtime::{
  CrdtRuntime, ProjectionFallbackStats, RuntimeAssetMetadata, RuntimeEvent, RuntimePresenceCaretRequest, RuntimePresenceCarets,
  RuntimeRevisionInfo, SemanticCommand,
};
use crate::presence::PresenceSelection;

#[derive(Clone)]
pub struct CrdtRuntimeHandle {
  commands: Sender<RuntimeRequest>,
}

impl CrdtRuntimeHandle {
  pub fn spawn(runtime: CrdtRuntime) -> io::Result<Self> {
    let (commands, receiver) = async_channel::unbounded();
    thread::Builder::new()
      .name("flowstate-crdt-runtime".to_string())
      .spawn(move || runtime_loop(runtime, receiver))?;
    Ok(Self { commands })
  }
}

impl CrdtRuntimeHandle {
  pub async fn apply_editor_commands(
    &self,
    base_frontier: Vec<u8>,
    commands: Vec<EditorSemanticCommand>,
    assets: Vec<AssetRecord>,
    selection_after: Option<EditorSelection>,
  ) -> Result<Vec<RuntimeEvent>> {
    self
      .request(|reply| RuntimeRequest::ApplyEditorCommands {
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
    self.request(|reply| RuntimeRequest::ProjectionSnapshot { reply }).await
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
    self.request(|reply| RuntimeRequest::SnapshotBytes { reply }).await
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
    self.request(|reply| RuntimeRequest::AssetMetadata { reply }).await
  }

  pub async fn revisions(&self) -> Result<Vec<RuntimeRevisionInfo>> {
    self.request(|reply| RuntimeRequest::Revisions { reply }).await
  }

  pub async fn projection_fallback_stats(&self) -> Result<ProjectionFallbackStats> {
    self.request(|reply| RuntimeRequest::ProjectionFallbackStats { reply }).await
  }

  pub async fn presence_selection(&self, selection: EditorSelection) -> Result<Option<PresenceSelection>> {
    self
      .request(|reply| RuntimeRequest::PresenceSelection { selection, reply })
      .await
  }

  pub async fn resolve_presence_carets(
    &self,
    requests: Vec<RuntimePresenceCaretRequest>,
  ) -> Result<RuntimePresenceCarets> {
    self
      .request(|reply| RuntimeRequest::ResolvePresenceCarets { requests, reply })
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
    base_frontier: Vec<u8>,
    commands: Vec<EditorSemanticCommand>,
    assets: Vec<AssetRecord>,
    selection_after: Option<EditorSelection>,
    reply: Sender<Result<Vec<RuntimeEvent>>>,
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
}

fn runtime_loop(mut runtime: CrdtRuntime, receiver: Receiver<RuntimeRequest>) {
  let mut deferred = None;
  loop {
    let request = match deferred.take() {
      Some(request) => request,
      None => match receiver.recv_blocking() {
        Ok(request) => request,
        Err(_) => break,
      },
    };
    match request {
      RuntimeRequest::ApplyEditorCommands {
        base_frontier,
        mut commands,
        mut assets,
        mut selection_after,
        reply,
      } => {
        let mut replies = vec![reply];
        while let Ok(next) = receiver.try_recv() {
          match next {
            RuntimeRequest::ApplyEditorCommands {
              base_frontier: next_base_frontier,
              commands: next_commands,
              assets: next_assets,
              selection_after: next_selection,
              reply,
            } if next_base_frontier == base_frontier => {
              commands.extend(next_commands);
              assets.extend(next_assets);
              if next_selection.is_some() {
                selection_after = next_selection;
              }
              replies.push(reply);
            },
            other => {
              deferred = Some(other);
              break;
            },
          }
        }
        let commands = coalesce_editor_commands(commands);
        let result: Result<Vec<RuntimeEvent>> = (|| {
          let mut events = runtime.apply_editor_commands(&base_frontier, &commands, selection_after.as_ref())?;
          events.extend(runtime.merge_asset_records(assets)?);
          Ok(events)
        })();
        match result {
          Ok(events) => {
            let final_reply = replies.pop();
            for reply in replies {
              send_reply(reply, Ok(Vec::new()));
            }
            if let Some(reply) = final_reply {
              send_reply(reply, Ok(events));
            }
          },
          Err(error) => {
            let stale_projection = error.downcast_ref::<crate::crdt_runtime::StaleProjectionError>().copied();
            let message = error.to_string();
            for reply in replies {
              let error = stale_projection
                .map(anyhow::Error::new)
                .unwrap_or_else(|| anyhow!(message.clone()));
              send_reply(reply, Err(error));
            }
          },
        }
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
    }
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

    assert!(error.downcast_ref::<crate::crdt_runtime::StaleProjectionError>().is_some());
    Ok(())
  }
}
