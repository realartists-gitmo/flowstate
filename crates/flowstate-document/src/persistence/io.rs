use std::{
  collections::hash_map::DefaultHasher,
  fs,
  hash::{Hash as _, Hasher as _},
  io::{self, Cursor, Read as _, Write as _},
  ops::Range,
  path::{Path, PathBuf},
  sync::Arc,
  time::Instant,
};

use crop::Rope;
use flowstate_collab::{
  ActorId, DocumentId as CollabDocumentId, FormatKind, NativeAssetRecord, NativeFileInput, blake3_hash, decode_native_file,
  encode_native_file,
};
use tempfile::NamedTempFile;

use super::{Document, demo_document, rebuild_document_offset_index, reconcile_document_ids, rebuild_document_sections, Block, paragraph_byte_range, ParagraphOffsetIndex, DocumentIds, DocumentTheme, log_timing_lazy, AssetStore, Paragraph, ParagraphStyle, ParagraphId, BlockId, DocumentSection, paragraph_index_for_id, TableBlock, TableCellBlock, TextRun, merge_adjacent_runs, SectionId, ImageBlock, AssetId, ImageSizing, EquationBlock, EquationSyntax, EquationDisplay, TableColumnWidth, TableCell, TableRow, TableStyle, TableCellParagraph, AssetRecord, document_text_slice, paragraph_runs_len, paragraph_text_len, BlockAlignment, SectionKind, RunStyles, RunSemanticStyle, HighlightStyle};

// DB8 projection cache stored inside the collaboration-native envelope. The
// external file format is the Flowstate collaboration envelope, not this chunk.
const DB8_PROJECTION_MAGIC: &[u8; 5] = b"DB8P\0";
const DB8_PROJECTION_VERSION: u32 = 1;

const CHUNK_TEXT: u8 = 1;
const CHUNK_ASSETS: u8 = 2;
const CHUNK_BLOCKS: u8 = 3;
const CHUNK_PARAGRAPH_IDS: u8 = 4;
const CHUNK_BLOCK_IDS: u8 = 5;
const CHUNK_SECTIONS: u8 = 6;
const CHUNK_DOCUMENT_META: u8 = 7;

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
  let decoded = decode_native_file(bytes, FormatKind::Db8).map_err(collab_to_io_error)?;
  let document = read_db8_projection_bytes_with_timing(&decoded.projection_cache, timing)?;
  if decoded.manifest.document_id.0.as_u128() != document.ids.document_id {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 document ID mismatch"));
  }
  validate_native_asset_manifest(&document, &decoded.asset_manifest)?;
  Ok(document)
}

#[hotpath::measure]
fn read_db8_projection_bytes_with_timing(bytes: &[u8], timing: Instant) -> io::Result<Document> {
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0; 5];
  cursor.read_exact(&mut magic)?;
  if &magic != DB8_PROJECTION_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 projection magic"));
  }
  let version = read_u32(&mut cursor)?;
  if version == DB8_PROJECTION_VERSION {
    return read_db8_vnext(cursor, timing);
  }
  Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported DB8 projection version"))
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
  let bytes = serialize_db8_native(&document)?;
  write_bytes_atomic(path, &bytes)
}

#[hotpath::measure]
pub fn db8_bytes(document: &Document) -> io::Result<Vec<u8>> {
  let document = document_for_serialization(document);
  validate_document(&document)?;
  serialize_db8_native(&document)
}

#[hotpath::measure]
fn serialize_db8_native(document: &Document) -> io::Result<Vec<u8>> {
  let projection_cache = serialize_db8_projection(document);
  encode_native_file(native_file_input_for_document(document, projection_cache)).map_err(collab_to_io_error)
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
fn serialize_db8_projection(document: &Document) -> Vec<u8> {
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

  let mut document_meta = Vec::new();
  write_u128(&mut document_meta, document.ids.document_id);
  chunks.push((CHUNK_DOCUMENT_META, document_meta));

  let table_entry_len = 1 + 1 + 2 + 8 + 8;
  let header_len =
    DB8_PROJECTION_MAGIC.len() + std::mem::size_of::<u32>() + std::mem::size_of::<u32>() + chunks.len() * table_entry_len;
  let payload_len = chunks.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
  let mut bytes = Vec::with_capacity(header_len + payload_len);
  bytes.extend_from_slice(DB8_PROJECTION_MAGIC);
  bytes.extend_from_slice(&DB8_PROJECTION_VERSION.to_le_bytes());
  write_u32(
    &mut bytes,
    u32::try_from(chunks.len()).expect("DB8 chunk count is fixed and fits in u32"),
  );
  let mut offset = header_len;
  for (kind, payload) in &chunks {
    bytes.push(*kind);
    bytes.push(0);
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    write_u64(&mut bytes, offset as u64);
    write_u64(&mut bytes, payload.len() as u64);
    offset += payload.len();
  }
  for (_, payload) in chunks {
    bytes.extend_from_slice(&payload);
  }
  bytes
}

#[hotpath::measure]
fn native_file_input_for_document(document: &Document, projection_cache: Vec<u8>) -> NativeFileInput {
  let created_by_actor = ActorId::new();
  let mut assets = document
    .assets
    .assets
    .values()
    .map(|asset| NativeAssetRecord {
      asset_id: asset.id.0,
      blake3_hash: blake3_hash(&asset.bytes),
      byte_len: asset.bytes.len() as u64,
      mime_type: asset.mime_type.to_string(),
      original_name: asset.original_name.as_ref().map(ToString::to_string),
      created_by_actor,
      inline: true,
    })
    .collect::<Vec<_>>();
  assets.sort_by_key(|asset| asset.asset_id);

  let mut input = NativeFileInput::new(FormatKind::Db8, projection_cache);
  input.document_id = CollabDocumentId(uuid::Uuid::from_u128(document.ids.document_id));
  input.created_by_actor = created_by_actor;
  input.asset_inline_data = db8_asset_inline_data(document, &assets);
  input.asset_manifest = assets;
  input
}

#[hotpath::measure]
fn db8_asset_inline_data(document: &Document, assets: &[NativeAssetRecord]) -> Vec<u8> {
  let mut bytes = Vec::new();
  write_u64(&mut bytes, assets.len() as u64);
  for asset in assets {
    if let Some(record) = document.assets.assets.get(&AssetId(asset.asset_id)) {
      bytes.extend_from_slice(&asset.blake3_hash);
      write_u64(&mut bytes, record.bytes.len() as u64);
      bytes.extend_from_slice(&record.bytes);
    }
  }
  bytes
}

#[hotpath::measure]
fn validate_native_asset_manifest(document: &Document, assets: &[NativeAssetRecord]) -> io::Result<()> {
  if assets.len() != document.assets.assets.len() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 asset manifest count mismatch"));
  }
  for asset in assets {
    let Some(record) = document.assets.assets.get(&AssetId(asset.asset_id)) else {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 asset manifest references missing asset"));
    };
    if asset.byte_len != record.bytes.len() as u64 {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 asset manifest byte length mismatch"));
    }
    if asset.blake3_hash != blake3_hash(&record.bytes) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 asset manifest hash mismatch"));
    }
  }
  Ok(())
}

#[hotpath::measure]
fn collab_to_io_error(error: flowstate_collab::CollabError) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
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
    .map_err(|error| error.error)
}
