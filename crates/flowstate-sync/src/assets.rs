use super::*;
use std::ops::Range;

#[derive(Debug)]
pub(crate) struct IncomingAssetUpload {
  expected_len: usize,
  bytes: Vec<u8>,
}

impl IncomingAssetUpload {
  pub(crate) fn new(expected_len: u64) -> AnyResult<Self> {
    let expected_len = usize::try_from(expected_len).context("asset upload length overflows usize")?;
    Ok(Self {
      expected_len,
      bytes: Vec::with_capacity(expected_len),
    })
  }

  pub(crate) fn accept(&mut self, chunk: &AssetChunkMessage, max_chunk_bytes: usize) -> AnyResult<Option<Vec<u8>>> {
    ensure!(!chunk.bytes.is_empty(), "asset upload chunk is empty");
    ensure!(chunk.bytes.len() <= max_chunk_bytes, "asset upload chunk exceeds the configured limit");
    let offset = usize::try_from(chunk.offset).context("asset upload offset overflows usize")?;
    let end = offset
      .checked_add(chunk.bytes.len())
      .context("asset upload range overflows usize")?;
    ensure!(end <= self.expected_len, "asset upload exceeds referenced byte length");
    if offset < self.bytes.len() {
      ensure!(end <= self.bytes.len(), "asset upload chunks overlap incompletely");
      ensure!(self.bytes[offset..end] == chunk.bytes, "asset upload retried with different bytes");
      return Ok(None);
    }
    ensure!(offset == self.bytes.len(), "asset upload chunk is not contiguous");
    self.bytes.extend_from_slice(&chunk.bytes);
    if self.bytes.len() != self.expected_len {
      return Ok(None);
    }
    ensure!(blake3_hash(&self.bytes) == chunk.blake3_hash, "asset upload hash mismatch");
    Ok(Some(std::mem::take(&mut self.bytes)))
  }
}

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
