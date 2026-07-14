use anyhow::{Context as _, Result, anyhow};
use flowstate_collab::{
  SessionId,
  net::{
    NetCommand, PublishPayload,
    anti_entropy::{GapAction, VersionVectorRelation},
    direct::DirectServeRequest,
  },
  proto_direct::AssetBytes,
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use gpui::Context;
use loro::VersionVector;

use crate::rich_text_element::{AssetId, AssetRecord};

use super::{CollabSession, DetachReason};

pub(super) const MAX_PENDING_REMOTE_UPDATES: usize = 512;
pub(super) const MAX_PENDING_REMOTE_UPDATE_BYTES: usize = 64 * 1024 * 1024;

impl CollabSession {
  // §perf: borrowed entry point for the genuinely-borrowed callers; copies once into an
  // owned buffer and forwards to import_update_bytes_owned. Owned-Vec callers should call
  // import_update_bytes_owned directly to skip this full-update memcpy on the main thread.
  pub fn import_update_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> Result<()> {
    self.import_update_bytes_owned(bytes.to_vec(), cx)
  }

  pub fn import_update_bytes_owned(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) -> Result<()> {
    if bytes.is_empty() {
      tracing::trace!(session = %self.session, "skipping empty collaboration update import");
      return Ok(());
    }
    if self.runtime.is_none() || self.editor.is_none() {
      if self.pending_remote_updates.len() >= MAX_PENDING_REMOTE_UPDATES
        || self.pending_remote_update_bytes.saturating_add(bytes.len()) > MAX_PENDING_REMOTE_UPDATE_BYTES
      {
        // Drop the whole pre-attach queue: partial update history is useless
        // to Loro anyway, and the overflow flag forces one anti-entropy pull
        // right after attach, which resynchronizes everything we dropped.
        tracing::warn!(
          session = %self.session,
          dropped_updates = self.pending_remote_updates.len() + 1,
          dropped_bytes = self.pending_remote_update_bytes + bytes.len(),
          "pre-attach collaboration update queue overflowed; will resync via anti-entropy pull after attach",
        );
        self.pending_remote_updates.clear();
        self.pending_remote_update_bytes = 0;
        self.pending_remote_updates_overflowed = true;
        return Ok(());
      }
      tracing::debug!(
        session = %self.session,
        bytes = bytes.len(),
        queued_updates = self.pending_remote_updates.len() + 1,
        has_runtime = self.runtime.is_some(),
        has_editor = self.editor.is_some(),
        "queueing remote collaboration update until session is attached",
      );
      self.pending_remote_update_bytes = self.pending_remote_update_bytes.saturating_add(bytes.len());
      self.pending_remote_updates.push(bytes); // §perf: move the owned buffer into the queue instead of copying
      return Ok(());
    }

    tracing::debug!(session = %self.session, bytes = bytes.len(), "importing remote collaboration update");
    let io = match self
      .runtime
      .clone()
      .context("collaboration session has no document I/O service")?
    {
      flowstate_collab::SyncIoHandle::RichText(io) => io,
      flowstate_collab::SyncIoHandle::Flow(io) => {
        // FLOW arm: one import request; the derivation rides the runtime's
        // streams and the pump drains vv/publish events afterwards.
        let session_id = self.session;
        cx.spawn(async move |session, cx| {
          match io.import_remote_update(bytes).await {
            Ok(()) => {
              let _ = session.update(cx, |session, cx| {
                session.pump_publish(cx);
                session.last_document_activity = std::time::Instant::now();
              });
            },
            Err(error) => {
              tracing::error!(session = %session_id, error = %format_args!("{error:#}"), "dropped unimportable remote flow update");
            },
          }
        })
        .detach();
        return Ok(());
      },
    };
    // §perf: bytes is already owned; hand it straight to the I/O service with no extra copy.
    let bytes_len = bytes.len();
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = io.import_remote_update(bytes).await;
      match result {
        Ok(events) => {
          let applied = session.update(cx, |session, cx| session.apply_runtime_events(events, true, cx));
          let projection_error = match applied {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(error) => {
              tracing::debug!(session = %session_id, %error, "collaboration session disappeared while applying remote update");
              return;
            },
          };
          if let Some(detail) = projection_error {
            // Applying a remote patch batch to THE projection failed; repair
            // via the spec §6 fallback: canonical `projection_snapshot()` +
            // full install. Still a fidelity violation worth counting.
            fidelity::violation(FidelityClass::Reconcile, "remote-projection-repair", || {
              format!("session={session_id} bytes={bytes_len} error={detail}")
            });
            tracing::warn!(session = %session_id, bytes = bytes_len, error = %detail, "remote patch application failed; repairing from canonical runtime snapshot");
            match io.projection_snapshot().await {
              Ok(document) => {
                let _ = session.update(cx, |session, cx| {
                  if let Err(error) = session.apply_runtime_projection(document, cx) {
                    session.detach(
                      DetachReason::Fatal(format!("applying canonical collaboration repair failed: {error:#}")),
                      cx,
                    );
                  }
                });
              },
              Err(error) => {
                let _ = session.update(cx, |session, cx| {
                  session.detach(
                    DetachReason::Fatal(format!("fetching canonical collaboration repair failed: {error:#}")),
                    cx,
                  );
                });
              },
            }
          } else {
            tracing::debug!(session = %session_id, bytes = bytes_len, "remote collaboration update imported and projected");
          }
        },
        Err(error) => {
          // A malformed or unimportable update is dropped, not session-fatal:
          // real content re-arrives via digest-driven anti-entropy pulls, and a
          // dead I/O service surfaces through the publish pump instead.
          tracing::error!(session = %session_id, bytes = bytes_len, error = %format_args!("{error:#}"), "dropped unimportable remote collaboration update");
        },
      }
    })
    .detach();
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
    if self.runtime.is_none() {
      tracing::debug!(session = %self.session, from = %from, digest_session = %digest_session, vv_bytes = vv.len(), "ignored collaboration digest because Loro doc is missing");
      return Ok(());
    }
    let sender_vv = match VersionVector::decode(vv).context("decoding collaboration digest failed") {
      Ok(sender_vv) => sender_vv,
      Err(error) => {
        // A malformed digest is the sender's defect, never grounds to detach
        // this healthy session; drop the frame and keep serving other peers.
        tracing::warn!(session = %self.session, from = %from, digest_session = %digest_session, vv_bytes = vv.len(), error = %format_args!("{error:#}"), "ignored undecodable collaboration digest from peer");
        return Ok(());
      },
    };
    if self.runtime_vv.is_empty() {
      tracing::debug!(session = %self.session, from = %from, "ignored collaboration digest until local version vector is initialized");
      return Ok(());
    }
    let our_vv = VersionVector::decode(&self.runtime_vv).context("decoding local collaboration version vector failed")?;
    let relation = match sender_vv.partial_cmp(&our_vv) {
      Some(std::cmp::Ordering::Equal) => VersionVectorRelation::Equal,
      Some(std::cmp::Ordering::Greater) => VersionVectorRelation::SenderHasMissingOps,
      Some(std::cmp::Ordering::Less) => VersionVectorRelation::WeHaveMissingOps,
      None => VersionVectorRelation::Concurrent,
    };
    let action = self
      .anti_entropy
      .consider_digest(from, digest_session, relation, self.runtime_vv.clone(), std::time::Instant::now());
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
    if let Err(error) = self.net_tx.try_send(NetCommand::PullBlob {
      session: self.session,
      candidates,
      blob,
      reply: reply_tx,
    }) {
      tracing::warn!(session = %self.session, from = %from, ?blob, error = %error, "queueing collaboration blob pull failed");
      return;
    }
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| match result {
        Ok(Ok(bytes)) => {
          tracing::debug!(session = %session_id, ?blob, bytes = bytes.len(), "collaboration blob pull succeeded");
          // §perf: bytes is owned here; move it in to avoid a full-update memcpy.
          if let Err(error) = session.import_update_bytes_owned(bytes, cx) {
            tracing::error!(session = %session_id, ?blob, error = %format_args!("{error:#}"), "importing pulled collaboration blob failed");
            session.detach(DetachReason::Fatal(format!("pulling collaboration blob failed: {error:#}")), cx);
          }
        },
        Ok(Err(error)) => tracing::warn!(session = %session_id, ?blob, error = %format_args!("{error:#}"), "collaboration blob pull failed"),
        Err(error) => tracing::warn!(session = %session_id, ?blob, error = %error, "collaboration blob pull reply channel closed"),
      });
    })
    .detach();
  }

  pub(super) fn publish_digest(&self) {
    if self.runtime.is_some() {
      let vv = self.runtime_vv.clone();
      if vv.is_empty() {
        tracing::trace!(session = %self.session, "skipping collaboration digest publish until runtime version vector is initialized");
        return;
      }
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

  pub(super) fn flush_pending_asset_records(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(editor) = self.rich_text_editor() else {
      tracing::trace!(session = %self.session, pending_asset_records = self.pending_asset_records.len(), "cannot flush collaboration asset records because no rich-text editor is attached");
      return false;
    };
    let deferred = editor.read(cx).projection_apply_deferred();
    if self.pending_asset_records.is_empty() || deferred {
      tracing::trace!(
        session = %self.session,
        pending_asset_records = self.pending_asset_records.len(),
        deferred,
        "collaboration asset record flush skipped",
      );
      return false;
    }
    let asset_records = std::mem::take(&mut self.pending_asset_records);
    tracing::debug!(session = %self.session, asset_records = asset_records.len(), "flushing collaboration asset records to editor");
    editor.update(cx, |editor, cx| {
      editor.apply_synced_asset_records(&asset_records, cx);
    });
    self.last_document_activity = std::time::Instant::now();
    self.refresh_external_carets(cx);
    tracing::debug!(session = %self.session, asset_records = asset_records.len(), "collaboration asset records flushed to editor");
    true
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
        let runtime = self.runtime.clone();
        let session_id = self.session;
        cx.spawn(async move |_, _| {
          let result = match runtime {
            Some(runtime) => runtime.snapshot_bytes().await,
            None => Err(anyhow!("collaboration session is not attached")),
          };
          log_direct_serve_result(session_id, "snapshot", &result);
          let _ = reply.send(result).await;
        })
        .detach();
      },
      DirectServeRequest::Updates { have_vv, reply } => {
        tracing::trace!(session = %self.session, have_vv_bytes = have_vv.len(), "serving collaboration updates request");
        let runtime = self.runtime.clone();
        let session_id = self.session;
        cx.spawn(async move |_, _| {
          let result = match runtime {
            Some(runtime) => runtime.export_updates_for(have_vv).await,
            None => Err(anyhow!("collaboration session is not attached")),
          };
          log_direct_serve_result(session_id, "updates", &result);
          let _ = reply.send(result).await;
        })
        .detach();
      },
      DirectServeRequest::Asset { asset, reply } => {
        let result = self.asset_bytes(asset, cx);
        match &result {
          Ok(bytes) => tracing::debug!(session = %self.session, asset, bytes = bytes.bytes.len(), "served collaboration asset direct request"),
          Err(error) => {
            tracing::warn!(session = %self.session, asset, error = %format_args!("{error:#}"), "serving collaboration asset direct request failed");
          },
        }
        let _ = reply.try_send(result);
      },
    }
  }

  fn asset_bytes(&self, asset: u128, cx: &mut Context<Self>) -> Result<AssetBytes> {
    let editor = self
      .rich_text_editor()
      .context("collaboration session has no rich-text editor (flow sessions carry no assets)")?;
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

  pub(super) fn queue_asset_records(&mut self, mut asset_records: Vec<(AssetId, AssetRecord)>, cx: &mut Context<Self>) {
    if asset_records.is_empty() {
      tracing::trace!(session = %self.session, "no collaboration asset records to queue");
      return;
    }
    for (id, record) in &asset_records {
      trace_asset_record(self.session, *id, record);
    }
    // Loro-first: fetched asset bytes flow to the editor's UI cache below AND
    // are recorded into canonical state through the I/O service so this
    // replica's own saves/packages include them. The session still never
    // writes document CONTENT (invariant 5) — asset bytes are content-
    // addressed sideband records.
    if let Some(io) = self.runtime.clone().and_then(|io| io.as_rich_text().cloned()) {
      let records: Vec<AssetRecord> = asset_records
        .iter()
        .map(|(_, record)| record.clone())
        .collect();
      cx.spawn(async move |_, _| {
        if let Err(error) = io.record_assets(records).await {
          tracing::warn!(%error, "recording pulled asset bytes into canonical state failed; saves may miss them until re-pull");
        }
      })
      .detach();
    }
    tracing::debug!(session = %self.session, asset_records = asset_records.len(), pending_before = self.pending_asset_records.len(), "queueing collaboration asset records");
    self.pending_asset_records.append(&mut asset_records);
    let flushed = self.flush_pending_asset_records(cx);
    tracing::trace!(session = %self.session, pending_after = self.pending_asset_records.len(), flushed, "collaboration asset record queue updated");
  }

  pub(super) fn start_update_pull(&mut self, from: flowstate_collab::ids::PeerId, our_vv: Vec<u8>, cx: &mut Context<Self>) {
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
      self.anti_entropy.finish_pull(from);
      return;
    }
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        session.anti_entropy.finish_pull(from);
        match result {
          Ok(Ok(bytes)) => {
            tracing::debug!(session = %session_id, from = %from, bytes = bytes.len(), "collaboration update pull succeeded");
            if bytes.is_empty() {
              tracing::trace!(session = %session_id, from = %from, "collaboration update pull returned no missing updates");
              return;
            }
            // §perf: bytes is owned here; move it in to avoid a full-update memcpy.
            if let Err(error) = session.import_update_bytes_owned(bytes, cx) {
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

fn trace_asset_record(session: SessionId, id: AssetId, record: &AssetRecord) {
  tracing::trace!(%session, ?id, bytes = record.bytes.len(), placeholder = record.is_loading_placeholder(), "queued collaboration asset record");
}
