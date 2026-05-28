use std::{
  collections::hash_map::DefaultHasher,
  fs,
  hash::{Hash, Hasher},
  io::{self, Cursor, Read, Write},
  ops::Range,
  path::{Path, PathBuf},
  sync::Arc,
  time::Instant,
};

use crop::Rope;
use tempfile::NamedTempFile;

use super::*;

// DB8 is our on-disk document format: a magic header, a version, the raw
// UTF-8 text blob, then per-paragraph run metadata. Keeping the format
// length-prefixed makes the reader resilient against trailing junk.
const DB8_MAGIC: &[u8; 4] = b"DB8\0";
const DB8_VERSION: u32 = 5;

const BLOCK_PARAGRAPH: u8 = 0;
const BLOCK_IMAGE: u8 = 1;
const BLOCK_EQUATION: u8 = 2;
const BLOCK_TABLE: u8 = 3;
const TABLE_CELL_PARAGRAPH: u8 = 0;
const TABLE_CELL_TABLE: u8 = 1;

pub fn load_or_create_document(path: impl AsRef<Path>) -> io::Result<Document> {
  let path = path.as_ref();
  match read_db8(path) {
    Ok(document) => Ok(document),
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      let document = demo_document();
      // Best-effort write: if the path is in a read-only directory (e.g. the
      // default data/demo.db8 when the CWD is not writable) we still open the
      // demo in memory rather than crashing.
      let _ = write_db8(path, &document);
      Ok(document)
    },
    Err(error) => Err(error),
  }
}

pub fn read_db8(path: impl AsRef<Path>) -> io::Result<Document> {
  let timing = Instant::now();
  let bytes = fs::read(path)?;
  let mut cursor = Cursor::new(bytes.as_slice());
  let mut magic = [0; 4];
  cursor.read_exact(&mut magic)?;
  if &magic != DB8_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 magic"));
  }
  let version = read_u32(&mut cursor)?;
  if version != DB8_VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported DB8 version"));
  }
  read_db8_current(cursor, timing)
}

pub fn write_db8(path: impl AsRef<Path>, document: &Document) -> io::Result<()> {
  let path = path.as_ref();
  // Skip directory creation when the parent component is empty (e.g. a bare
  // filename like "doc.db8" with no directory prefix), as create_dir_all("")
  // fails on most platforms. write_bytes_atomic handles it identically.
  if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }
  let document = document_for_serialization(document);
  validate_document(&document)?;
  let bytes = serialize_db8(&document)?;
  write_bytes_atomic(path, &bytes)
}

fn document_for_serialization(document: &Document) -> Document {
  let mut document = document.clone();
  // Recovery/autosave can snapshot while live editing is still settling; make
  // sure byte offsets are derived from the paragraph projection we are about
  // to serialize instead of trusting cached offsets.
  rebuild_document_offset_index(&mut document);
  document.blocks = Arc::new(serializable_blocks(&document));
  document
}

fn serialize_db8(document: &Document) -> io::Result<Vec<u8>> {
  let estimated_asset_bytes = document
    .assets
    .assets
    .values()
    .map(|asset| asset.bytes.len())
    .sum::<usize>();
  let mut bytes = Vec::with_capacity(
    DB8_MAGIC.len()
      + std::mem::size_of::<u32>()
      + document.text.byte_len()
      + estimated_asset_bytes
      + document.blocks.len().saturating_mul(32),
  );
  bytes.extend_from_slice(DB8_MAGIC);
  bytes.extend_from_slice(&DB8_VERSION.to_le_bytes());
  write_u64(&mut bytes, document.text.byte_len() as u64);
  for chunk in document.text.chunks() {
    bytes.extend_from_slice(chunk.as_bytes());
  }

  write_u64(&mut bytes, document.assets.assets.len() as u64);
  for asset in document.assets.assets.values() {
    write_asset_record(&mut bytes, asset);
  }

  write_u64(&mut bytes, document.blocks.len() as u64);
  for block in document.blocks.iter() {
    write_block_record(&mut bytes, block);
  }
  Ok(bytes)
}

fn serializable_blocks(document: &Document) -> Vec<Block> {
  let mut paragraph_ix = 0;
  let mut blocks = Vec::with_capacity(document.blocks.len().max(document.paragraphs.len()));

  // The current editor mutates the paragraph projection. Rebuild paragraph
  // block payloads from that live projection while keeping object/table blocks
  // in their structural positions.
  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(_) => {
        if let Some(paragraph) = document.paragraphs.get(paragraph_ix) {
          let mut paragraph = paragraph.clone();
          paragraph.byte_range = paragraph_byte_range(document, paragraph_ix);
          blocks.push(Block::Paragraph(paragraph));
          paragraph_ix += 1;
        }
      },
      other => blocks.push(other.clone()),
    }
  }

  while let Some(paragraph) = document.paragraphs.get(paragraph_ix) {
    let mut paragraph = paragraph.clone();
    paragraph.byte_range = paragraph_byte_range(document, paragraph_ix);
    blocks.push(Block::Paragraph(paragraph));
    paragraph_ix += 1;
  }

  blocks
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
  // Use "." as fallback when the path has no directory component (e.g. a bare
  // filename) so NamedTempFile::new_in doesn't receive an empty path.
  let parent = path
    .parent()
    .filter(|p| !p.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  let mut temp = NamedTempFile::new_in(parent)?;
  temp.write_all(bytes)?;
  temp.as_file_mut().sync_all()?;
  let temp_path = temp.into_temp_path();
  #[cfg(target_os = "windows")]
  {
    // Windows does not allow the POSIX-style atomic replace that tempfile's
    // `persist` relies on for existing files. Remove the old target first,
    // then rename the fully written temp file into place. This is slightly
    // less atomic on Windows, but avoids false "Access is denied" failures
    // when saving a normal existing document.
    match fs::remove_file(path) {
      Ok(()) => {},
      Err(error) if error.kind() == io::ErrorKind::NotFound => {},
      Err(error) => return Err(error),
    }
  }
  temp_path
    .persist(path)
    .map(|_| ())
    .map_err(|error| error.error)
}
