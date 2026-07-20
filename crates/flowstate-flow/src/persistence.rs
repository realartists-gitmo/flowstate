use std::fs;
use std::io::{self, Cursor, Read as _};
use std::path::Path;

use anyhow::{Context as _, bail};
use uuid::Uuid;

use crate::document::FlowDocument;

const MAGIC: &[u8; 8] = b"FLOWFL0\0";
const VERSION: u32 = 3;
/// Refuse to read pathological on-disk sizes before allocating for them.
const MAX_FL0_BYTES: u64 = 64 * 1024 * 1024;

pub fn load_flow_document(path: impl AsRef<Path>) -> anyhow::Result<FlowDocument> {
  decode(&read_fl0_bytes(path.as_ref())?)
}

/// Read a `.fl0` and unframe it to the raw Loro snapshot WITHOUT materializing a
/// `FlowDocument` — for building a `FlowRuntime` directly on open. Opening via a
/// `FlowDocument` would import + materialize the board, then the runtime does it
/// a SECOND time; this path skips the throwaway first build.
pub fn load_flow_snapshot(path: impl AsRef<Path>) -> anyhow::Result<Vec<u8>> {
  decode_snapshot(&read_fl0_bytes(path.as_ref())?)
}

fn read_fl0_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
  let metadata = fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
  if metadata.len() > MAX_FL0_BYTES {
    bail!(
      "refusing to read {}: file too large ({} bytes > {} bytes)",
      path.display(),
      metadata.len(),
      MAX_FL0_BYTES
    );
  }
  Ok(fs::read(path)?)
}

pub fn save_flow_document(path: impl AsRef<Path>, document: &FlowDocument) -> anyhow::Result<()> {
  save_encoded_to(path.as_ref(), &encode(document)?)
}

/// Frame + atomically write an ALREADY-ENCODED .fl0 (the runtime services fork
/// under the gate and encode off it — they never hold a `FlowDocument`).
pub fn save_snapshot_to(path: impl AsRef<Path>, snapshot: &[u8]) -> anyhow::Result<()> {
  save_encoded_to(path.as_ref(), &encode_snapshot(snapshot)?)
}

fn save_encoded_to(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    fs::create_dir_all(parent)?;
  }
  // Write-then-rename so a crash mid-save can't leave a truncated .fl0 in
  // place of the previous good document.
  let temp_path = path.with_file_name(format!(
    ".{}.{}.tmp",
    path
      .file_name()
      .and_then(|name| name.to_str())
      .unwrap_or("flowstate"),
    Uuid::new_v4()
  ));
  fs::write(&temp_path, bytes).with_context(|| format!("failed to write temporary {}", temp_path.display()))?;
  fs::rename(&temp_path, path).with_context(|| format!("failed to atomically replace {} with {}", path.display(), temp_path.display()))?;
  Ok(())
}

pub fn encode(document: &FlowDocument) -> anyhow::Result<Vec<u8>> {
  encode_snapshot(&document.snapshot()?)
}

/// Frame a raw Loro snapshot as .fl0 v3 bytes.
pub fn encode_snapshot(snapshot: &[u8]) -> anyhow::Result<Vec<u8>> {
  let compressed = zstd::stream::encode_all(snapshot, 3)?;
  let mut bytes = Vec::with_capacity(MAGIC.len() + 8 + compressed.len());
  bytes.extend_from_slice(MAGIC);
  bytes.extend_from_slice(&VERSION.to_le_bytes());
  bytes.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
  bytes.extend_from_slice(&compressed);
  Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> anyhow::Result<FlowDocument> {
  FlowDocument::from_snapshot(&decode_snapshot(bytes)?)
}

/// Unframe .fl0 v3 bytes back to the raw Loro snapshot (schema validation is
/// the runtime's job — `from_snapshot` checks `flow.meta/schema_version`).
pub fn decode_snapshot(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0; 8];
  cursor.read_exact(&mut magic)?;
  if &magic != MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid .fl0 signature").into());
  }
  let version = read_u32(&mut cursor)?;
  if version == 1 || version == 2 {
    // D3: v2 (the Living Grid schema) is dropped like v1 — it never left the
    // known alpha circle, so it gets the same explicit rejection instead of
    // a frozen legacy module.
    return Err(
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unsupported .fl0 version {version} (pre-release format, never shipped; no migration path)"),
      )
      .into(),
    );
  }
  if version != VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsupported .fl0 version {version}")).into());
  }
  let payload_len = read_u64(&mut cursor)? as usize;
  if payload_len != bytes.len().saturating_sub(cursor.position() as usize) {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid .fl0 payload length").into());
  }
  let mut compressed = Vec::with_capacity(payload_len);
  cursor.read_to_end(&mut compressed)?;
  Ok(zstd::stream::decode_all(compressed.as_slice())?)
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
  use std::{path::PathBuf, time::SystemTime};

  fn test_dir(name: &str) -> PathBuf {
    let unique = format!(
      "{name}-{}-{}",
      std::process::id(),
      SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    fs::create_dir_all(&path).unwrap();
    path
  }

  #[test]
  fn rejects_oversized_documents_before_reading() {
    let dir = test_dir("flowstate-fl0-size");
    let path = dir.join("oversized.fl0");
    fs::write(&path, vec![b'{'; (MAX_FL0_BYTES as usize) + 1]).unwrap();

    let error = load_flow_document(&path).map(|_| ()).unwrap_err();
    assert!(error.to_string().contains("file too large"));
  }

  #[test]
  fn load_failure_leaves_the_file_untouched() {
    let dir = test_dir("flowstate-fl0-corrupt");
    let path = dir.join("broken.fl0");
    fs::write(&path, b"{not a flow document").unwrap();

    assert!(load_flow_document(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{not a flow document");
  }

  #[test]
  fn save_replaces_atomically_without_leaving_temp_files() {
    let dir = test_dir("flowstate-fl0-atomic");
    let path = dir.join("doc.fl0");
    let document = FlowDocument::new();
    save_flow_document(&path, &document).unwrap();
    save_flow_document(&path, &document).unwrap();

    let entries: Vec<_> = fs::read_dir(&dir)
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    assert_eq!(entries, vec!["doc.fl0".to_owned()]);
    assert!(decode(&fs::read(&path).unwrap()).is_ok());
  }

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

  #[test]
  fn snapshot_framing_round_trips() {
    let document = FlowDocument::new();
    let snapshot = document.snapshot().unwrap();
    assert_eq!(decode_snapshot(&encode_snapshot(&snapshot).unwrap()).unwrap(), snapshot);
  }

  #[test]
  fn rejects_v1_and_v2_with_the_no_migration_message() {
    for version in [1_u32, 2_u32] {
      let mut bytes = encode(&FlowDocument::new()).unwrap();
      bytes[8..12].copy_from_slice(&version.to_le_bytes());
      let error = decode(&bytes).map(|_| ()).unwrap_err().to_string();
      assert!(error.contains("pre-release format, never shipped"), "v{version} got: {error}");
    }
  }
}
