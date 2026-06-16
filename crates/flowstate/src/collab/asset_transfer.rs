use std::{
  collections::hash_map::DefaultHasher,
  hash::{Hash, Hasher},
  sync::Arc,
};

use anyhow::{Context as _, Result, anyhow, ensure};
use flowstate_collab::{
  net::NetCommand,
  schema::{BLOCKS, BlockPayload, DATA, KIND, KIND_IMAGE, decode_block_payload},
};
use gpui::{Context, SharedString};
use loro::{Container, LoroValue, ValueOrContainer};

use crate::rich_text_element::{AssetId, AssetRecord, CollabPatch};

use super::CollabSession;

pub(super) fn schedule_missing_assets(
  session: &mut CollabSession,
  preferred_peer: Option<flowstate_collab::ids::PeerId>,
  cx: &mut Context<CollabSession>,
) {
  let Some(editor) = session.editor.clone() else {
    tracing::trace!(session = %session.session, preferred_peer = ?preferred_peer, "skipping collaboration asset scan because editor is missing");
    return;
  };
  let Some(doc) = &session.doc else {
    tracing::trace!(session = %session.session, preferred_peer = ?preferred_peer, "skipping collaboration asset scan because Loro doc is missing");
    return;
  };

  let assets = match image_assets_in_loro(doc) {
    Ok(assets) => assets,
    Err(error) => {
      tracing::warn!(session = %session.session, error = %format_args!("{error:#}"), "collaboration asset scan failed");
      return;
    },
  };
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
    .map(|meta| CollabPatch::AssetArrived {
      id: AssetId(meta.asset_id),
      record: meta.placeholder_record(),
    })
    .collect::<Vec<_>>();
  tracing::debug!(session = %session.session, placeholders = placeholders.len(), "queueing collaboration asset placeholders");
  session.apply_or_queue_patches(placeholders, cx);

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
  if let Err(error) = session
    .net_tx
    .try_send(NetCommand::PullAsset {
      session: session.session,
      candidates,
      asset: meta.asset_id,
      reply: reply_tx,
    })
  {
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
            session.apply_or_queue_patches(vec![CollabPatch::AssetArrived { id, record }], cx);
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
  content_hash: u64,
  byte_len: u64,
}

impl ImageAssetMeta {
  fn placeholder_record(&self) -> AssetRecord {
    AssetRecord {
      id: AssetId(self.asset_id),
      mime_type: SharedString::from(self.mime.clone()),
      original_name: self.original_name.clone().map(SharedString::from),
      content_hash: self.content_hash,
      bytes: Arc::new(Vec::new()),
    }
  }

  fn record_from_bytes(&self, bytes: Vec<u8>) -> Result<AssetRecord> {
    tracing::trace!(asset = self.asset_id, expected_bytes = self.byte_len, received_bytes = bytes.len(), "validating fetched collaboration asset bytes");
    ensure!(bytes.len() as u64 == self.byte_len, "asset byte length mismatch");
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let content_hash = hasher.finish();
    ensure!(content_hash == self.content_hash, "asset content hash mismatch");
    Ok(AssetRecord {
      id: AssetId(self.asset_id),
      mime_type: SharedString::from(self.mime.clone()),
      original_name: self.original_name.clone().map(SharedString::from),
      content_hash,
      bytes: Arc::new(bytes),
    })
  }
}

fn image_assets_in_loro(doc: &loro::LoroDoc) -> Result<Vec<ImageAssetMeta>> {
  let blocks = doc.get_movable_list(BLOCKS);
  let mut assets = Vec::new();
  for ix in 0..blocks.len() {
    let Some(ValueOrContainer::Container(Container::Map(map))) = blocks.get(ix) else {
      continue;
    };
    let Some(ValueOrContainer::Value(LoroValue::String(kind))) = map.get(KIND) else {
      continue;
    };
    if kind.as_ref() != KIND_IMAGE {
      continue;
    }
    let Some(ValueOrContainer::Value(LoroValue::Binary(data))) = map.get(DATA) else {
      return Err(anyhow!("image block {ix} is missing asset metadata"));
    };
    let payload = decode_block_payload(&data).context("decoding image asset metadata failed")?;
    let BlockPayload::Image {
      asset_id,
      mime,
      original_name,
      content_hash,
      byte_len,
      ..
    } = payload
    else {
      continue;
    };
    if byte_len == 0 {
      continue;
    }
    assets.push(ImageAssetMeta {
      asset_id,
      mime,
      original_name,
      content_hash,
      byte_len,
    });
  }
  Ok(assets)
}
