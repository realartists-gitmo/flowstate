use std::fs;
use std::io::{self, Cursor, Read as _};
use std::path::Path;

use anyhow::{Context as _, bail};
use uuid::Uuid;

const MAGIC: &[u8; 8] = b"FLOWFL0\0";
/// The v2 container (spec Part A): same magic/framing, payload is the
/// zstd-compressed Loro snapshot of a v2 schema doc. v1 (the pre-release
/// record-blob format) is explicitly rejected — it never shipped.
pub const FL0_VERSION: u32 = 2;
/// Refuse to read pathological on-disk sizes before allocating for them.
const MAX_FL0_BYTES: u64 = 64 * 1024 * 1024;

/// Encode a raw Loro snapshot as v2 `.fl0` bytes.
#[must_use]
pub fn encode_fl0_snapshot(snapshot: &[u8]) -> Vec<u8> {
  let compressed = zstd::stream::encode_all(snapshot, 3).expect("zstd encoding an in-memory buffer cannot fail");
  let mut bytes = Vec::with_capacity(MAGIC.len() + 12 + compressed.len());
  bytes.extend_from_slice(MAGIC);
  bytes.extend_from_slice(&FL0_VERSION.to_le_bytes());
  bytes.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
  bytes.extend_from_slice(&compressed);
  bytes
}

/// Decode v2 `.fl0` bytes back to the raw Loro snapshot. The caller imports it
/// into a fresh doc and validates `flow.meta/schema_version` (wrong-kind /
/// wrong-schema defense).
pub fn decode_fl0_snapshot(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
  if bytes.len() as u64 > MAX_FL0_BYTES {
    bail!("refusing to decode oversized .fl0 payload ({} bytes)", bytes.len());
  }
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0; 8];
  cursor.read_exact(&mut magic)?;
  if &magic != MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid .fl0 signature").into());
  }
  let version = read_u32(&mut cursor)?;
  if version == 1 {
    // v1 was a pre-release format that never shipped; there is nothing to
    // migrate. Loud, specific message instead of a generic version error.
    return Err(
      io::Error::new(
        io::ErrorKind::InvalidData,
        "this .fl0 uses the pre-release v1 format, which never shipped and cannot be opened",
      )
      .into(),
    );
  }
  if version != FL0_VERSION {
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

/// Read a v2 `.fl0` file (size-capped) and return the raw Loro snapshot.
pub fn read_fl0(path: impl AsRef<Path>) -> anyhow::Result<Vec<u8>> {
  let path = path.as_ref();
  let metadata = fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
  if metadata.len() > MAX_FL0_BYTES {
    bail!(
      "refusing to read {}: file too large ({} bytes > {} bytes)",
      path.display(),
      metadata.len(),
      MAX_FL0_BYTES
    );
  }
  decode_fl0_snapshot(&fs::read(path)?)
}

/// Atomically write a raw Loro snapshot as a v2 `.fl0` file
/// (write-then-rename, same law as the v1 saver).
pub fn write_fl0(path: impl AsRef<Path>, snapshot: &[u8]) -> anyhow::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    fs::create_dir_all(parent)?;
  }
  let temp_path = path.with_file_name(format!(
    ".{}.{}.tmp",
    path
      .file_name()
      .and_then(|name| name.to_str())
      .unwrap_or("flowstate"),
    Uuid::new_v4()
  ));
  fs::write(&temp_path, encode_fl0_snapshot(snapshot)).with_context(|| format!("failed to write temporary {}", temp_path.display()))?;
  fs::rename(&temp_path, path).with_context(|| format!("failed to atomically replace {} with {}", path.display(), temp_path.display()))?;
  Ok(())
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
    let error = read_fl0(&path).map(|_| ()).unwrap_err();
    assert!(error.to_string().contains("file too large"));
  }

  #[test]
  fn load_failure_leaves_the_file_untouched() {
    let dir = test_dir("flowstate-fl0-corrupt");
    let path = dir.join("broken.fl0");
    fs::write(&path, b"{not a flow document").unwrap();
    assert!(read_fl0(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{not a flow document");
  }

  #[test]
  fn save_replaces_atomically_without_leaving_temp_files() {
    let dir = test_dir("flowstate-fl0-atomic");
    let path = dir.join("doc.fl0");
    write_fl0(&path, b"snapshot bytes").unwrap();
    write_fl0(&path, b"snapshot bytes").unwrap();
    let entries: Vec<_> = fs::read_dir(&dir)
      .unwrap()
      .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
      .collect();
    assert_eq!(entries, vec!["doc.fl0".to_owned()]);
    assert_eq!(read_fl0(&path).unwrap(), b"snapshot bytes");
  }

  #[test]
  fn rejects_corruption() {
    assert!(decode_fl0_snapshot(b"{}").is_err());
  }

  #[test]
  fn round_trips() {
    let decoded = decode_fl0_snapshot(&encode_fl0_snapshot(b"loro snapshot")).unwrap();
    assert_eq!(decoded, b"loro snapshot");
  }
}
