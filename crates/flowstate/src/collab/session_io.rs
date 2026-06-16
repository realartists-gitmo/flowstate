use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow};
use flowstate_collab::{
  SessionId,
  binding::DocBinding,
  net::{
    NetCommand, PublishPayload,
    anti_entropy::{GapAction, VersionVectorRelation},
    direct::DirectServeRequest,
  },
  proto_direct::AssetBytes,
  remote_apply::RemoteApplier,
};
use gpui::Context;
use loro::{ExportMode, LoroDoc, Subscription as LoroSubscription, VersionVector, event::Subscriber};

use crate::rich_text_element::{AssetId, CollabPatch, Document, UndoRedirect};

use super::{CollabSession, DetachReason};

impl CollabSession {
  pub fn import_update_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() || self.editor.is_none() {
      tracing::debug!(
        session = %self.session,
        bytes = bytes.len(),
        queued_updates = self.pending_remote_updates.len() + 1,
        has_doc = self.doc.is_some(),
        has_binding = self.binding.is_some(),
        has_editor = self.editor.is_some(),
        "queueing remote collaboration update until session is attached",
      );
      self.pending_remote_updates.push(bytes.to_vec());
      return Ok(());
    }

    tracing::debug!(session = %self.session, bytes = bytes.len(), "importing remote collaboration update");
    let editor = self
      .editor
      .clone()
      .context("collaboration session has no editor")?;
    let document = Arc::new(editor.read(cx).document().clone());
    let doc = self
      .doc
      .clone()
      .context("collaboration session has no Loro document")?;
    let binding = self
      .binding
      .take()
      .context("collaboration session has no document binding")?;
    let binding = Arc::new(Mutex::new(binding));
    let patches = Arc::new(Mutex::new(Vec::<CollabPatch>::new()));
    let sub = self.diff_subscription(doc.clone(), document, binding.clone(), patches.clone());

    let import_result = doc.import_with(bytes, "remote");
    drop(sub);
    self.binding = Some(take_mutex_value(binding, "document binding")?);
    let patches = take_mutex_value(patches, "remote patches")?;
    if let Err(error) = import_result.context("importing collaboration update failed") {
      tracing::error!(session = %self.session, bytes = bytes.len(), error = %format_args!("{error:#}"), "remote collaboration update import failed");
      return Err(error);
    }
    tracing::debug!(session = %self.session, bytes = bytes.len(), patches = patches.len(), "remote collaboration update imported");
    self.apply_or_queue_patches(patches, cx);
    Ok(())
  }

  pub(super) fn attach_direct_request_pump(&mut self, cx: &mut Context<Self>) {
    if self.direct_pump_started {
      tracing::trace!(session = %self.session, "collaboration direct request pump already started");
      return;
    }
    self.direct_pump_started = true;
    let requests = self.direct_rx.clone();
    let session_id = self.session;
    tracing::debug!(session = %session_id, "starting collaboration direct request pump");
    cx.spawn(async move |session, cx| {
      while let Ok(request) = requests.recv().await {
        tracing::trace!(session = %session_id, request_kind = direct_serve_request_kind(&request), "received collaboration direct serve request");
        if session
          .update(cx, |session, cx| session.handle_direct_request(request, cx))
          .is_err()
        {
          tracing::debug!(session = %session_id, "collaboration direct request pump session disappeared");
          break;
        }
      }
      tracing::debug!(session = %session_id, "collaboration direct request pump stopped");
    })
    .detach();
  }

  pub(super) fn handle_digest(
    &mut self,
    from: flowstate_collab::ids::PeerId,
    digest_session: SessionId,
    vv: &[u8],
    cx: &mut Context<Self>,
  ) -> Result<()> {
    let Some(doc) = &self.doc else {
      tracing::debug!(session = %self.session, from = %from, digest_session = %digest_session, vv_bytes = vv.len(), "ignored collaboration digest because Loro doc is missing");
      return Ok(());
    };
    let sender_vv = match VersionVector::decode(vv).context("decoding collaboration digest failed") {
      Ok(sender_vv) => sender_vv,
      Err(error) => {
        tracing::warn!(session = %self.session, from = %from, digest_session = %digest_session, vv_bytes = vv.len(), error = %format_args!("{error:#}"), "decoding collaboration digest failed");
        return Err(error);
      },
    };
    let our_vv = doc.oplog_vv();
    let relation = match sender_vv.partial_cmp(&our_vv) {
      Some(std::cmp::Ordering::Equal) => VersionVectorRelation::Equal,
      Some(std::cmp::Ordering::Greater) => VersionVectorRelation::SenderHasMissingOps,
      Some(std::cmp::Ordering::Less) => VersionVectorRelation::WeHaveMissingOps,
      None => VersionVectorRelation::Concurrent,
    };
    let action = self
      .anti_entropy
      .consider_digest(from, digest_session, relation, our_vv.encode());
    tracing::trace!(
      session = %self.session,
      from = %from,
      digest_session = %digest_session,
      vv_bytes = vv.len(),
      ?relation,
      action = gap_action_kind(&action),
      "handled collaboration digest",
    );
    self.handle_gap_action(action, cx);
    Ok(())
  }

  pub(super) fn pull_blob(&mut self, from: flowstate_collab::ids::PeerId, blob: flowstate_collab::BlobId, cx: &mut Context<Self>) {
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    let candidates = self.pull_candidates(Some(from));
    tracing::debug!(session = %self.session, from = %from, ?blob, candidate_count = candidates.len(), "requesting collaboration update blob pull");
    if let Err(error) = self
      .net_tx
      .try_send(NetCommand::PullBlob {
        session: self.session,
        candidates,
        blob,
        reply: reply_tx,
      })
    {
      tracing::warn!(session = %self.session, from = %from, ?blob, error = %error, "queueing collaboration blob pull failed");
      return;
    }
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        match result {
          Ok(Ok(bytes)) => {
            tracing::debug!(session = %session_id, ?blob, bytes = bytes.len(), "collaboration blob pull succeeded");
            if let Err(error) = session.import_update_bytes(&bytes, cx) {
              tracing::error!(session = %session_id, ?blob, error = %format_args!("{error:#}"), "importing pulled collaboration blob failed");
              session.detach(DetachReason::Fatal(format!("pulling collaboration blob failed: {error:#}")), cx);
            }
          },
          Ok(Err(error)) => tracing::warn!(session = %session_id, ?blob, error = %format_args!("{error:#}"), "collaboration blob pull failed"),
          Err(error) => tracing::warn!(session = %session_id, ?blob, error = %error, "collaboration blob pull reply channel closed"),
        }
      });
    })
    .detach();
  }

  pub(super) fn publish_digest(&self) {
    if let Some(doc) = &self.doc {
      let vv = doc.oplog_vv().encode();
      let vv_bytes = vv.len();
      if let Err(error) = self.net_tx.try_send(NetCommand::Publish {
        session: self.session,
        payload: PublishPayload::Digest { vv },
      }) {
        tracing::warn!(session = %self.session, vv_bytes, error = %error, "queueing collaboration digest publish failed");
      } else {
        tracing::trace!(session = %self.session, vv_bytes, "queued collaboration digest publish");
      }
    } else {
      tracing::trace!(session = %self.session, "skipping collaboration digest publish because Loro doc is missing");
    }
  }

  pub(super) fn flush_pending_remote_patches(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(editor) = self.editor.clone() else {
      tracing::trace!(session = %self.session, pending_patches = self.pending_remote_patches.len(), "cannot flush remote collaboration patches because editor is missing");
      return false;
    };
    if self.pending_remote_patches.is_empty() || editor.read(cx).collab_apply_deferred() {
      tracing::trace!(
        session = %self.session,
        pending_patches = self.pending_remote_patches.len(),
        deferred = editor.read(cx).collab_apply_deferred(),
        "remote collaboration patch flush skipped",
      );
      return false;
    }
    let patches = std::mem::take(&mut self.pending_remote_patches);
    tracing::debug!(session = %self.session, patches = patches.len(), "flushing remote collaboration patches to editor");
    editor.update(cx, |editor, cx| {
      editor.clear_undo_redo_stacks();
      editor.apply_collab_patches(&patches, cx);
    });
    self.last_document_activity = std::time::Instant::now();
    self.refresh_external_carets(cx);
    tracing::debug!(session = %self.session, patches = patches.len(), "remote collaboration patches flushed to editor");
    true
  }

  pub(super) fn apply_loro_undo_redirect(&mut self, redirect: UndoRedirect, cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() || self.editor.is_none() || self.undo_manager.is_none() {
      tracing::warn!(
        session = %self.session,
        ?redirect,
        has_doc = self.doc.is_some(),
        has_binding = self.binding.is_some(),
        has_editor = self.editor.is_some(),
        has_undo_manager = self.undo_manager.is_some(),
        "cannot apply collaboration undo redirect because session state is incomplete",
      );
      return Ok(());
    }

    tracing::debug!(session = %self.session, ?redirect, "applying collaboration undo redirect");
    let editor = self
      .editor
      .clone()
      .context("collaboration session has no editor")?;
    let document = Arc::new(editor.read(cx).document().clone());
    let doc = self
      .doc
      .clone()
      .context("collaboration session has no Loro document")?;
    let binding = self
      .binding
      .take()
      .context("collaboration session has no document binding")?;
    let binding = Arc::new(Mutex::new(binding));
    let patches = Arc::new(Mutex::new(Vec::<CollabPatch>::new()));
    let sub = self.diff_subscription(doc, document, binding.clone(), patches.clone());

    let undo_result = match redirect {
      UndoRedirect::Undo => self
        .undo_manager
        .as_mut()
        .context("collaboration session has no undo manager")?
        .undo(),
      UndoRedirect::Redo => self
        .undo_manager
        .as_mut()
        .context("collaboration session has no undo manager")?
        .redo(),
    };
    drop(sub);
    self.binding = Some(take_mutex_value(binding, "document binding")?);
    let patches = take_mutex_value(patches, "undo patches")?;
    let applied = undo_result.context("applying collaboration undo operation failed")?;
    tracing::debug!(session = %self.session, ?redirect, applied, patches = patches.len(), "collaboration undo redirect applied");
    if applied {
      self.apply_or_queue_patches(patches, cx);
      self.publish_digest();
    }
    Ok(())
  }

  pub(super) fn handle_gap_action(&mut self, action: GapAction, cx: &mut Context<Self>) {
    match action {
      GapAction::None => {},
      GapAction::Pull { from, our_vv } => {
        tracing::debug!(session = %self.session, from = %from, our_vv_bytes = our_vv.len(), "collaboration gap action requested update pull");
        self.start_update_pull(from, our_vv, cx);
      },
      GapAction::LineageMismatch { from, expected, got } => {
        tracing::warn!(session = %self.session, from = %from, expected = %expected, got = %got, "ignored mismatched collaboration digest");
      },
    }
  }

  fn handle_direct_request(&mut self, request: DirectServeRequest, cx: &mut Context<Self>) {
    tracing::trace!(session = %self.session, request_kind = direct_serve_request_kind(&request), "serving collaboration direct request from session");
    match request {
      DirectServeRequest::Snapshot { reply } => {
        let result = self.snapshot_bytes();
        log_direct_serve_result(self.session, "snapshot", &result);
        let _ = reply.try_send(result);
      },
      DirectServeRequest::Updates { have_vv, reply } => {
        tracing::trace!(session = %self.session, have_vv_bytes = have_vv.len(), "serving collaboration updates request");
        let result = self.update_bytes(&have_vv);
        log_direct_serve_result(self.session, "updates", &result);
        let _ = reply.try_send(result);
      },
      DirectServeRequest::Asset { asset, reply } => {
        let result = self.asset_bytes(asset, cx);
        match &result {
          Ok(bytes) => tracing::debug!(session = %self.session, asset, bytes = bytes.bytes.len(), "served collaboration asset direct request"),
          Err(error) => tracing::warn!(session = %self.session, asset, error = %format_args!("{error:#}"), "serving collaboration asset direct request failed"),
        }
        let _ = reply.try_send(result);
      },
    }
  }

  fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    let bytes = self
      .doc
      .as_ref()
      .context("collaboration session is not attached")?
      .export(ExportMode::Snapshot)
      .context("exporting collaboration snapshot failed")?;
    tracing::debug!(session = %self.session, bytes = bytes.len(), "exported collaboration snapshot");
    Ok(bytes)
  }

  fn update_bytes(&self, have_vv: &[u8]) -> Result<Vec<u8>> {
    let vv = VersionVector::decode(have_vv).context("decoding collaboration version vector failed")?;
    let bytes = self
      .doc
      .as_ref()
      .context("collaboration session is not attached")?
      .export(ExportMode::updates(&vv))
      .context("exporting collaboration updates failed")?;
    tracing::debug!(session = %self.session, have_vv_bytes = have_vv.len(), bytes = bytes.len(), "exported collaboration updates");
    Ok(bytes)
  }

  fn asset_bytes(&self, asset: u128, cx: &mut Context<Self>) -> Result<AssetBytes> {
    let editor = self
      .editor
      .as_ref()
      .context("collaboration session has no editor")?;
    let bytes = editor
      .read(cx)
      .document()
      .assets
      .assets
      .get(&AssetId(asset))
      .map(|record| record.bytes.as_ref().clone())
      .ok_or_else(|| anyhow!("collaboration asset {asset} is not available"))?;
    tracing::debug!(session = %self.session, asset, bytes = bytes.len(), "exported collaboration asset bytes");
    Ok(AssetBytes { bytes })
  }

  fn diff_subscription(
    &self,
    doc: LoroDoc,
    document: Arc<Document>,
    binding: Arc<Mutex<DocBinding>>,
    patches: Arc<Mutex<Vec<CollabPatch>>>,
  ) -> LoroSubscription {
    let subscribed_doc = doc.clone();
    let callback: Subscriber = Arc::new(move |event| {
      let produced = {
        let mut binding = match binding.lock() {
          Ok(binding) => binding,
          Err(error) => {
            tracing::error!(error = %error, "collaboration binding lock failed during remote apply");
            return;
          },
        };
        let result = {
          let mut applier = RemoteApplier {
            doc: &doc,
            binding: &mut binding,
          };
          applier.apply_event(&document, &event)
        };
        drop(binding);
        result
      };
      match produced {
        Ok(mut produced) => {
          tracing::trace!(patches = produced.len(), "collaboration remote apply produced patches");
          if let Ok(mut patches) = patches.lock() {
            patches.append(&mut produced);
          } else {
            tracing::error!("collaboration remote patch lock failed");
          }
        },
        Err(error) => tracing::error!(error = %format_args!("{error:#}"), "collaboration remote apply failed"),
      }
    });
    subscribed_doc.subscribe_root(callback)
  }

  pub(super) fn apply_or_queue_patches(&mut self, mut patches: Vec<CollabPatch>, cx: &mut Context<Self>) {
    if patches.is_empty() {
      tracing::trace!(session = %self.session, "no remote collaboration patches to queue");
      return;
    }
    for patch in &patches {
      trace_collab_patch(self.session, patch);
    }
    tracing::debug!(session = %self.session, patches = patches.len(), pending_before = self.pending_remote_patches.len(), "queueing remote collaboration patches");
    self.pending_remote_patches.append(&mut patches);
    let flushed = self.flush_pending_remote_patches(cx);
    tracing::trace!(session = %self.session, pending_after = self.pending_remote_patches.len(), flushed, "remote collaboration patch queue updated");
  }

  fn start_update_pull(&mut self, from: flowstate_collab::ids::PeerId, our_vv: Vec<u8>, cx: &mut Context<Self>) {
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    let candidates = self.pull_candidates(Some(from));
    tracing::debug!(session = %self.session, from = %from, our_vv_bytes = our_vv.len(), candidate_count = candidates.len(), "requesting collaboration update pull");
    let send_result = self.net_tx.try_send(NetCommand::PullUpdates {
      session: self.session,
      candidates,
      our_vv,
      reply: reply_tx,
    });
    if let Err(error) = send_result {
      tracing::warn!(session = %self.session, from = %from, error = %error, "queueing collaboration update pull failed");
      self.anti_entropy.finish_pull();
      return;
    }
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        session.anti_entropy.finish_pull();
        match result {
          Ok(Ok(bytes)) => {
            tracing::debug!(session = %session_id, from = %from, bytes = bytes.len(), "collaboration update pull succeeded");
            if let Err(error) = session.import_update_bytes(&bytes, cx) {
              tracing::error!(session = %session_id, from = %from, error = %format_args!("{error:#}"), "importing pulled collaboration updates failed");
              session.detach(DetachReason::Fatal(format!("pulling collaboration updates failed: {error:#}")), cx);
            }
          },
          Ok(Err(error)) => tracing::warn!(session = %session_id, from = %from, error = %format_args!("{error:#}"), "collaboration update pull failed"),
          Err(error) => tracing::warn!(session = %session_id, from = %from, error = %error, "collaboration update pull reply channel closed"),
        }
      });
    })
    .detach();
  }
}

fn take_mutex_value<T>(value: Arc<Mutex<T>>, label: &str) -> Result<T> {
  match Arc::try_unwrap(value) {
    Ok(mutex) => mutex
      .into_inner()
      .map_err(|error| anyhow!("collaboration {label} lock was poisoned: {error}")),
    Err(_) => Err(anyhow!("collaboration {label} is still referenced")),
  }
}

fn direct_serve_request_kind(request: &DirectServeRequest) -> &'static str {
  match request {
    DirectServeRequest::Snapshot { .. } => "snapshot",
    DirectServeRequest::Updates { .. } => "updates",
    DirectServeRequest::Asset { .. } => "asset",
  }
}

fn gap_action_kind(action: &GapAction) -> &'static str {
  match action {
    GapAction::None => "none",
    GapAction::Pull { .. } => "pull",
    GapAction::LineageMismatch { .. } => "lineage_mismatch",
  }
}

fn log_direct_serve_result(session: SessionId, kind: &'static str, result: &Result<Vec<u8>>) {
  match result {
    Ok(bytes) => tracing::debug!(%session, kind, bytes = bytes.len(), "served collaboration direct payload"),
    Err(error) => tracing::warn!(%session, kind, error = %format_args!("{error:#}"), "serving collaboration direct payload failed"),
  }
}

fn trace_collab_patch(session: SessionId, patch: &CollabPatch) {
  match patch {
    CollabPatch::ParagraphText { row, delta_utf8, .. } => {
      tracing::trace!(%session, patch_kind = "paragraph_text", row, deltas = delta_utf8.len(), "queued collaboration patch");
    },
    CollabPatch::ParagraphStyle { row, style } => {
      tracing::trace!(%session, patch_kind = "paragraph_style", row, ?style, "queued collaboration patch");
    },
    CollabPatch::ReplaceObjectBlock { row, block } => {
      tracing::trace!(%session, patch_kind = "replace_object_block", row, block_id = ?block.block_id, "queued collaboration patch");
    },
    CollabPatch::InsertBlocks { row, blocks } => {
      tracing::trace!(%session, patch_kind = "insert_blocks", row, blocks = blocks.len(), "queued collaboration patch");
    },
    CollabPatch::DeleteBlocks { row, count } => {
      tracing::trace!(%session, patch_kind = "delete_blocks", row, count, "queued collaboration patch");
    },
    CollabPatch::MoveBlock { from, to } => {
      tracing::trace!(%session, patch_kind = "move_block", from, to, "queued collaboration patch");
    },
    CollabPatch::AssetArrived { id, record } => {
      tracing::trace!(%session, patch_kind = "asset_arrived", ?id, bytes = record.bytes.len(), "queued collaboration patch");
    },
  }
}
