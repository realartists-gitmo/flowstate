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
use tracing::warn;

use crate::rich_text_element::{AssetId, CollabPatch, Document, UndoRedirect};

use super::{CollabSession, DetachReason};

impl CollabSession {
  pub fn import_update_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() || self.editor.is_none() {
      self.pending_remote_updates.push(bytes.to_vec());
      return Ok(());
    }

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
    import_result.context("importing collaboration update failed")?;
    self.apply_or_queue_patches(patches, cx);
    Ok(())
  }

  pub(super) fn attach_direct_request_pump(&mut self, cx: &mut Context<Self>) {
    if self.direct_pump_started {
      return;
    }
    self.direct_pump_started = true;
    let requests = self.direct_rx.clone();
    cx.spawn(async move |session, cx| {
      while let Ok(request) = requests.recv().await {
        if session
          .update(cx, |session, cx| session.handle_direct_request(request, cx))
          .is_err()
        {
          break;
        }
      }
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
      return Ok(());
    };
    let sender_vv = VersionVector::decode(vv).context("decoding collaboration digest failed")?;
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
    self.handle_gap_action(action, cx);
    Ok(())
  }

  pub(super) fn pull_blob(&mut self, from: flowstate_collab::ids::PeerId, blob: flowstate_collab::BlobId, cx: &mut Context<Self>) {
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if self
      .net_tx
      .try_send(NetCommand::PullBlob {
        session: self.session,
        candidates: self.pull_candidates(Some(from)),
        blob,
        reply: reply_tx,
      })
      .is_err()
    {
      return;
    }
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        if let Ok(Ok(bytes)) = result
          && let Err(error) = session.import_update_bytes(&bytes, cx)
        {
          session.detach(DetachReason::Fatal(format!("pulling collaboration blob failed: {error:#}")), cx);
        }
      });
    })
    .detach();
  }

  pub(super) fn publish_digest(&self) {
    if let Some(doc) = &self.doc {
      let _ = self.net_tx.try_send(NetCommand::Publish {
        session: self.session,
        payload: PublishPayload::Digest { vv: doc.oplog_vv().encode() },
      });
    }
  }

  pub(super) fn flush_pending_remote_patches(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(editor) = self.editor.clone() else {
      return false;
    };
    if self.pending_remote_patches.is_empty() || editor.read(cx).collab_apply_deferred() {
      return false;
    }
    let patches = std::mem::take(&mut self.pending_remote_patches);
    editor.update(cx, |editor, cx| {
      editor.clear_undo_redo_stacks();
      editor.apply_collab_patches(&patches, cx);
    });
    self.last_document_activity = std::time::Instant::now();
    self.refresh_external_carets(cx);
    true
  }

  pub(super) fn apply_loro_undo_redirect(&mut self, redirect: UndoRedirect, cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() || self.editor.is_none() || self.undo_manager.is_none() {
      return Ok(());
    }

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
    if applied {
      self.apply_or_queue_patches(patches, cx);
      self.publish_digest();
    }
    Ok(())
  }

  pub(super) fn handle_gap_action(&mut self, action: GapAction, cx: &mut Context<Self>) {
    match action {
      GapAction::None => {},
      GapAction::Pull { from, our_vv } => self.start_update_pull(from, our_vv, cx),
      GapAction::LineageMismatch { from, expected, got } => {
        warn!("flowstate collab ignored mismatched digest from {from}: expected session {expected}, got {got}");
      },
    }
  }

  fn handle_direct_request(&mut self, request: DirectServeRequest, cx: &mut Context<Self>) {
    match request {
      DirectServeRequest::Snapshot { reply } => {
        let _ = reply.try_send(self.snapshot_bytes());
      },
      DirectServeRequest::Updates { have_vv, reply } => {
        let _ = reply.try_send(self.update_bytes(&have_vv));
      },
      DirectServeRequest::Asset { asset, reply } => {
        let _ = reply.try_send(self.asset_bytes(asset, cx));
      },
    }
  }

  fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    self
      .doc
      .as_ref()
      .context("collaboration session is not attached")?
      .export(ExportMode::Snapshot)
      .context("exporting collaboration snapshot failed")
  }

  fn update_bytes(&self, have_vv: &[u8]) -> Result<Vec<u8>> {
    let vv = VersionVector::decode(have_vv).context("decoding collaboration version vector failed")?;
    self
      .doc
      .as_ref()
      .context("collaboration session is not attached")?
      .export(ExportMode::updates(&vv))
      .context("exporting collaboration updates failed")
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
            tracing::warn!("flowstate collab binding lock failed: {error}");
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
          if let Ok(mut patches) = patches.lock() {
            patches.append(&mut produced);
          }
        },
        Err(error) => tracing::warn!("flowstate collab remote apply failed: {error:#}"),
      }
    });
    subscribed_doc.subscribe_root(callback)
  }

  pub(super) fn apply_or_queue_patches(&mut self, mut patches: Vec<CollabPatch>, cx: &mut Context<Self>) {
    if patches.is_empty() {
      return;
    }
    self.pending_remote_patches.append(&mut patches);
    self.flush_pending_remote_patches(cx);
  }

  fn start_update_pull(&mut self, from: flowstate_collab::ids::PeerId, our_vv: Vec<u8>, cx: &mut Context<Self>) {
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    let send_result = self.net_tx.try_send(NetCommand::PullUpdates {
      session: self.session,
      candidates: self.pull_candidates(Some(from)),
      our_vv,
      reply: reply_tx,
    });
    if send_result.is_err() {
      self.anti_entropy.finish_pull();
      return;
    }
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        session.anti_entropy.finish_pull();
        if let Ok(Ok(bytes)) = result
          && let Err(error) = session.import_update_bytes(&bytes, cx)
        {
          session.detach(DetachReason::Fatal(format!("pulling collaboration updates failed: {error:#}")), cx);
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
