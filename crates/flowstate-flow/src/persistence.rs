use std::fs;
use std::io::{self, Cursor, Read as _};
use std::path::Path;

use crate::document::FlowDocument;

const MAGIC: &[u8; 8] = b"FLOWFL0\0";
const VERSION: u32 = 1;

pub fn load_flow_document(path: impl AsRef<Path>) -> anyhow::Result<FlowDocument> {
  decode(&fs::read(path)?)
}

pub fn save_flow_document(path: impl AsRef<Path>, document: &FlowDocument) -> anyhow::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }
  fs::write(path, encode(document)?)?;
  Ok(())
}

pub fn encode(document: &FlowDocument) -> anyhow::Result<Vec<u8>> {
  let snapshot = document.snapshot()?;
  let compressed = zstd::stream::encode_all(snapshot.as_slice(), 3)?;
  let mut bytes = Vec::with_capacity(MAGIC.len() + 8 + compressed.len());
  bytes.extend_from_slice(MAGIC);
  bytes.extend_from_slice(&VERSION.to_le_bytes());
  bytes.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
  bytes.extend_from_slice(&compressed);
  Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> anyhow::Result<FlowDocument> {
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0; 8];
  cursor.read_exact(&mut magic)?;
  if &magic != MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid .fl0 signature").into());
  }
  let version = read_u32(&mut cursor)?;
  if version != VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsupported .fl0 version {version}")).into());
  }
  let payload_len = read_u64(&mut cursor)? as usize;
  if payload_len != bytes.len().saturating_sub(cursor.position() as usize) {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid .fl0 payload length").into());
  }
  let mut compressed = Vec::with_capacity(payload_len);
  cursor.read_to_end(&mut compressed)?;
  let snapshot = zstd::stream::decode_all(compressed.as_slice())?;
  FlowDocument::from_snapshot(&snapshot)
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
  let mut bytes = [0; 4];
  cursor.read_exact(&mut bytes)?;
  Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> io::Result<u64> {
  let mut bytes = [0; 8];
  cursor.read_exact(&mut bytes)?;
  Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rejects_corruption() {
    assert!(decode(b"{}").is_err());
  }

  #[test]
  fn round_trips() {
    let document = FlowDocument::new();
    let restored = decode(&encode(&document).unwrap()).unwrap();
    assert_eq!(document.projection(), restored.projection());
  }
}
