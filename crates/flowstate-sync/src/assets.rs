use super::*;
use std::ops::Range;

pub(crate) fn asset_have(document_id: DocumentId, store: &AssetStore) -> WireMessage {
  WireMessage::AssetHave(AssetHaveMessage {
    document_id,
    assets: store.hashes(),
  })
}

pub(crate) fn asset_need(document_id: DocumentId, blake3_hash: [u8; 32], offset: u64, len: u64) -> WireMessage {
  WireMessage::AssetNeed(AssetNeedMessage {
    document_id,
    blake3_hash,
    offset,
    len,
  })
}

pub(crate) fn bounded_range(offset: u64, len: u64, available: usize, max_chunk_bytes: usize) -> AnyResult<Range<usize>> {
  let start = usize::try_from(offset).context("asset offset overflows usize")?;
  let requested = usize::try_from(len).context("asset length overflows usize")?;
  let len = requested.min(max_chunk_bytes);
  let end = start
    .checked_add(len)
    .context("asset range overflows usize")?;
  ensure!(start <= available && end <= available, "asset range is out of bounds");
  Ok(start..end)
}
