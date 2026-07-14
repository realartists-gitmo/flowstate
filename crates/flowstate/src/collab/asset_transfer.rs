use std::sync::Arc;

use anyhow::{Result, ensure};
use flowstate_collab::{crdt_runtime::RuntimeAssetMetadata, net::NetCommand};
use gpui::{Context, SharedString};

use crate::rich_text_element::{AssetId, AssetRecord};

use super::CollabSession;

pub(super) fn schedule_missing_assets(
  session: &mut CollabSession,
  preferred_peer: Option<flowstate_collab::ids::PeerId>,
  cx: &mut Context<CollabSession>,
) {
  let Some(editor) = session.rich_text_editor() else {
    tracing::trace!(session = %session.session, preferred_peer = ?preferred_peer, "skipping collaboration asset scan because editor is missing");
    return;
  };
  let Some(runtime) = session.runtime.clone() else {
    tracing::trace!(session = %session.session, preferred_peer = ?preferred_peer, "skipping collaboration asset scan because Loro doc is missing");
    return;
  };
  let session_id = session.session;
  cx.spawn(async move |session, cx| {
    let Some(runtime) = runtime.as_rich_text().cloned() else {
      return;
    };
    let result = runtime.asset_metadata().await;
    let _ = session.update(cx, |session, cx| match result {
      Ok(assets) => schedule_missing_assets_from_metadata(session, editor, preferred_peer, assets, cx),
      Err(error) => {
        tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "collaboration asset scan failed");
      },
    });
  })
  .detach();
}

fn schedule_missing_assets_from_metadata(
  session: &mut CollabSession,
  editor: gpui::Entity<crate::rich_text_element::RichTextEditor>,
  preferred_peer: Option<flowstate_collab::ids::PeerId>,
  assets: Vec<RuntimeAssetMetadata>,
  cx: &mut Context<CollabSession>,
) {
  let assets = assets
    .into_iter()
    .map(ImageAssetMeta::from)
    .collect::<Vec<_>>();
  tracing::trace!(session = %session.session, assets = assets.len(), "scanned collaboration image assets");
  if assets.is_empty() {
    return;
  }

  let missing_assets = {
    let editor = editor.read(cx);
    assets
      .iter()
      .filter(|meta| {
        let id = AssetId(meta.asset_id);
        editor
          .document()
          .assets
          .assets
          .get(&id)
          .is_none_or(AssetRecord::is_loading_placeholder)
      })
      .cloned()
      .collect::<Vec<_>>()
  };
  tracing::debug!(session = %session.session, assets = assets.len(), missing_assets = missing_assets.len(), "checked collaboration image assets for missing bytes");
  if missing_assets.is_empty() {
    return;
  }

  let placeholders = missing_assets
    .iter()
    .map(|meta| (AssetId(meta.asset_id), meta.placeholder_record()))
    .collect::<Vec<_>>();
  tracing::debug!(session = %session.session, placeholders = placeholders.len(), "queueing collaboration asset placeholder records");
  session.queue_asset_records(placeholders, cx);

  let candidates = session.pull_candidates(preferred_peer);
  if candidates.is_empty() {
    tracing::warn!(session = %session.session, missing_assets = missing_assets.len(), "cannot pull collaboration assets because no candidate peers are available");
    return;
  }

  for meta in missing_assets {
    let id = AssetId(meta.asset_id);
    if !session.asset_pulls_in_flight.insert(id) {
      tracing::trace!(session = %session.session, ?id, "collaboration asset pull already in flight");
      continue;
    }
    tracing::debug!(session = %session.session, ?id, bytes = meta.byte_len, candidate_count = candidates.len(), "scheduling collaboration asset pull");
    start_asset_pull(session, candidates.clone(), meta, cx);
  }
}

fn start_asset_pull(
  session: &mut CollabSession,
  candidates: Vec<flowstate_collab::ids::PeerId>,
  meta: ImageAssetMeta,
  cx: &mut Context<CollabSession>,
) {
  let (reply_tx, reply_rx) = async_channel::bounded(1);
  let id = AssetId(meta.asset_id);
  let session_id = session.session;
  let candidate_count = candidates.len();
  if let Err(error) = session.net_tx.try_send(NetCommand::PullAsset {
    session: session.session,
    candidates,
    asset: meta.asset_id,
    reply: reply_tx,
  }) {
    tracing::warn!(session = %session_id, ?id, candidate_count, error = %error, "queueing collaboration asset pull failed");
    session.asset_pulls_in_flight.remove(&id);
    return;
  }
  tracing::debug!(session = %session_id, ?id, candidate_count, expected_bytes = meta.byte_len, "queued collaboration asset pull");

  cx.spawn(async move |session, cx| {
    let result = reply_rx.recv().await;
    let _ = session.update(cx, |session, cx| {
      session.asset_pulls_in_flight.remove(&id);
      match result {
        Ok(Ok(bytes)) => match meta.record_from_bytes(bytes.bytes) {
          Ok(record) => {
            tracing::debug!(session = %session_id, ?id, bytes = record.bytes.len(), "collaboration asset pull succeeded");
            session.queue_asset_records(vec![(id, record)], cx);
          },
          Err(error) => tracing::warn!(session = %session_id, ?id, error = %format_args!("{error:#}"), "rejected fetched collaboration asset"),
        },
        Ok(Err(error)) => tracing::warn!(session = %session_id, ?id, error = %format_args!("{error:#}"), "collaboration asset pull failed"),
        Err(error) => tracing::warn!(session = %session_id, ?id, error = %error, "collaboration asset pull channel closed"),
      }
    });
  })
  .detach();
}

#[derive(Clone, Debug)]
struct ImageAssetMeta {
  asset_id: u128,
  mime: String,
  original_name: Option<String>,
  content_hash: [u8; 32],
  byte_len: u64,
}

impl From<RuntimeAssetMetadata> for ImageAssetMeta {
  fn from(value: RuntimeAssetMetadata) -> Self {
    Self {
      asset_id: value.asset_id,
      mime: value.mime_type,
      original_name: value.original_name,
      content_hash: value.content_hash,
      byte_len: value.byte_length,
    }
  }
}

impl ImageAssetMeta {
  fn placeholder_record(&self) -> AssetRecord {
    AssetRecord {
      id: AssetId(self.asset_id),
      mime_type: SharedString::from(self.mime.clone()),
      original_name: self.original_name.clone().map(SharedString::from),
      content_hash: local_cache_hash(&self.content_hash),
      bytes: Arc::new(Vec::new()),
    }
  }

  fn record_from_bytes(&self, bytes: Vec<u8>) -> Result<AssetRecord> {
    tracing::trace!(
      asset = self.asset_id,
      expected_bytes = self.byte_len,
      received_bytes = bytes.len(),
      "validating fetched collaboration asset bytes"
    );
    ensure!(bytes.len() as u64 == self.byte_len, "asset byte length mismatch");
    ensure!(blake3::hash(&bytes).as_bytes() == &self.content_hash, "asset BLAKE3 digest mismatch");
    let content_hash = local_cache_hash(&self.content_hash);
    Ok(AssetRecord {
      id: AssetId(self.asset_id),
      mime_type: SharedString::from(self.mime.clone()),
      original_name: self.original_name.clone().map(SharedString::from),
      content_hash,
      bytes: Arc::new(bytes),
    })
  }
}

fn local_cache_hash(content_hash: &[u8; 32]) -> u64 {
  u64::from_le_bytes(
    content_hash[..8]
      .try_into()
      .expect("BLAKE3 prefix is exactly eight bytes"),
  )
}
