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
const DB8_LEGACY_VERSION: u32 = 5;
const DB8_VERSION: u32 = 6;

const CHUNK_TEXT: u8 = 1;
const CHUNK_ASSETS: u8 = 2;
const CHUNK_BLOCKS: u8 = 3;
const CHUNK_PARAGRAPH_IDS: u8 = 4;
const CHUNK_BLOCK_IDS: u8 = 5;
const CHUNK_SECTIONS: u8 = 6;

const BLOCK_PARAGRAPH: u8 = 0;
const BLOCK_IMAGE: u8 = 1;
const BLOCK_EQUATION: u8 = 2;
const BLOCK_TABLE: u8 = 3;
const TABLE_CELL_PARAGRAPH: u8 = 0;
const TABLE_CELL_TABLE: u8 = 1;

#[hotpath::measure]
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

#[hotpath::measure]
pub fn read_db8(path: impl AsRef<Path>) -> io::Result<Document> {
  let timing = Instant::now();
  let bytes = fs::read(path)?;
  read_db8_bytes_with_timing(&bytes, timing)
}

#[hotpath::measure]
pub fn read_db8_bytes(bytes: &[u8]) -> io::Result<Document> {
  read_db8_bytes_with_timing(bytes, Instant::now())
}

#[hotpath::measure]
fn read_db8_bytes_with_timing(bytes: &[u8], timing: Instant) -> io::Result<Document> {
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0; 4];
  cursor.read_exact(&mut magic)?;
  if &magic != DB8_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 magic"));
  }
  let version = read_u32(&mut cursor)?;
  if version == DB8_LEGACY_VERSION {
    return read_db8_current(cursor, timing);
  }
  if version == DB8_VERSION {
    return read_db8_vnext(cursor, timing);
  }
  Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported DB8 version"))
}

#[hotpath::measure]
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

#[hotpath::measure]
pub fn db8_bytes(document: &Document) -> io::Result<Vec<u8>> {
  let document = document_for_serialization(document);
  validate_document(&document)?;
  serialize_db8(&document)
}

#[hotpath::measure]
fn document_for_serialization(document: &Document) -> Document {
  let mut document = document.clone();
  // Recovery/autosave can snapshot while live editing is still settling; make
  // sure byte offsets are derived from the paragraph projection we are about
  // to serialize instead of trusting cached offsets.
  rebuild_document_offset_index(&mut document);
  document.blocks = Arc::new(serializable_blocks(&document));
  reconcile_document_ids(&mut document);
  rebuild_document_sections(&mut document);
  document
}

#[hotpath::measure]
fn serialize_db8(document: &Document) -> io::Result<Vec<u8>> {
  let mut chunks = Vec::<(u8, Vec<u8>)>::new();
  let mut text = Vec::with_capacity(document.text.byte_len());
  for chunk in document.text.chunks() {
    text.extend_from_slice(chunk.as_bytes());
  }
  chunks.push((CHUNK_TEXT, text));

  let mut assets = Vec::new();
  write_u64(&mut assets, document.assets.assets.len() as u64);
  for asset in document.assets.assets.values() {
    write_asset_record(&mut assets, asset);
  }
  chunks.push((CHUNK_ASSETS, assets));

  let mut blocks = Vec::new();
  write_u64(&mut blocks, document.blocks.len() as u64);
  for block in document.blocks.iter() {
    write_block_record(&mut blocks, block);
  }
  chunks.push((CHUNK_BLOCKS, blocks));

  let mut paragraph_ids = Vec::new();
  write_u64(&mut paragraph_ids, document.ids.paragraph_ids.len() as u64);
  for id in &document.ids.paragraph_ids {
    write_u128(&mut paragraph_ids, id.0);
  }
  chunks.push((CHUNK_PARAGRAPH_IDS, paragraph_ids));

  let mut block_ids = Vec::new();
  write_u64(&mut block_ids, document.ids.block_ids.len() as u64);
  for id in &document.ids.block_ids {
    write_u128(&mut block_ids, id.0);
  }
  chunks.push((CHUNK_BLOCK_IDS, block_ids));

  let mut sections = Vec::new();
  write_u64(&mut sections, document.sections.len() as u64);
  for section in document.sections.iter() {
    write_section_record(&mut sections, section);
  }
  chunks.push((CHUNK_SECTIONS, sections));

  let table_entry_len = 1 + 1 + 2 + 8 + 8;
  let header_len = DB8_MAGIC.len() + std::mem::size_of::<u32>() + std::mem::size_of::<u32>() + chunks.len() * table_entry_len;
  let payload_len = chunks.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
  let mut bytes = Vec::with_capacity(header_len + payload_len);
  bytes.extend_from_slice(DB8_MAGIC);
  bytes.extend_from_slice(&DB8_VERSION.to_le_bytes());
  write_u32(&mut bytes, chunks.len() as u32);
  let mut offset = header_len;
  for (kind, payload) in &chunks {
    bytes.push(*kind);
    bytes.push(0);
    bytes.extend_from_slice(&0u16.to_le_bytes());
    write_u64(&mut bytes, offset as u64);
    write_u64(&mut bytes, payload.len() as u64);
    offset += payload.len();
  }
  for (_, payload) in chunks {
    bytes.extend_from_slice(&payload);
  }
  Ok(bytes)
}

#[hotpath::measure]
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

#[hotpath::measure]
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
