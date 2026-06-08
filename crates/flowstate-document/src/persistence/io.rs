use std::{
  collections::HashSet,
  fs,
  io::{self, Cursor, Read as _, Write as _},
  ops::Range,
  path::{Path, PathBuf},
  sync::Arc,
  time::Instant,
};

use crop::Rope;
use flowstate_collab::{
  ActorId, CollabDocument, Db8CollabDocument, DocumentId as CollabDocumentId, FormatKind, GranularBinaryRecord, GranularOrderRecord,
  GranularSource, GranularTextMark, GranularTextRecord, GranularValue, NativeAssetRecord, NativeFileInput, blake3_hash, decode_native_file,
  encode_native_file, granular_record_id_to_u128, granular_record_id_u128,
};
use tempfile::NamedTempFile;

use crate::{
  AssetId, AssetRecord, AssetStore, Block, BlockAlignment, BlockId, Document, DocumentIds, DocumentSection, DocumentStyleManifest,
  DocumentTheme, EquationBlock, EquationDisplay, EquationSyntax, HIGHLIGHT_ALTERNATIVE, HIGHLIGHT_INSERT, HIGHLIGHT_SPOKEN, HighlightStyle,
  ImageBlock, ImageSizing, PARAGRAPH_ANALYTIC, PARAGRAPH_BLOCK, PARAGRAPH_HAT, PARAGRAPH_POCKET, PARAGRAPH_TAG, PARAGRAPH_UNDERTAG, Paragraph,
  ParagraphId, ParagraphOffsetIndex, ParagraphStyle, RunSemanticStyle, RunStyles, SEMANTIC_CITE, SEMANTIC_CONDENSED, SEMANTIC_EMPHASIS,
  SEMANTIC_ULTRACONDENSED, SEMANTIC_UNDERLINE, SectionId, SectionKind, TableBlock, TableCell, TableCellBlock, TableCellParagraph,
  TableColumnWidth, TableRow, TableStyle, TextRun, demo_document, document_text_slice, merge_adjacent_runs, paragraph_byte_range,
  paragraph_index_for_id, rebuild_document_offset_index, rebuild_document_sections, reconcile_document_ids,
};

fn log_timing_lazy(label: &str, start: Instant, detail: impl FnOnce() -> String) {
  if std::env::var_os("FLOWSTATE_TIMING").is_some() {
    eprintln!("[timing] {label}: {:?} {}", start.elapsed(), detail());
  }
}

fn validate_document(document: &Document) -> io::Result<()> {
  if document.paragraphs.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 document has no paragraphs"));
  }
  if document.ids.paragraph_ids.len() != document.paragraphs.len() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 paragraph ID count mismatch"));
  }
  if document.ids.block_ids.len() != document.blocks.len() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 block ID count mismatch"));
  }
  validate_document_ids(&document.ids)?;
  validate_document_paragraphs(document)?;
  validate_document_blocks(document)?;
  validate_document_sections(document)?;
  Ok(())
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct Db8GranularSourceMetadata {
  projection_cache: Vec<u8>,
  style_manifest: DocumentStyleManifest,
}

fn validate_document_ids(ids: &DocumentIds) -> io::Result<()> {
  validate_unique_ids(ids.paragraph_ids.iter().copied(), "DB8 paragraph ID duplicate")?;
  validate_unique_ids(ids.block_ids.iter().copied(), "DB8 block ID duplicate")?;
  Ok(())
}
fn validate_document_paragraphs(document: &Document) -> io::Result<()> {
  let text = document.text.to_string();
  for paragraph in document.paragraphs.iter() {
    validate_utf8_range_in_text(&text, paragraph.byte_range.clone(), "DB8 paragraph byte range")?;
    if paragraph_runs_len_from_runs(&paragraph.runs) != paragraph.byte_range.len() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 paragraph run length mismatch"));
    }
  }
  Ok(())
}

fn validate_document_blocks(document: &Document) -> io::Result<()> {
  let mut paragraph_ix = 0;
  for block in document.blocks.iter() {
    validate_block(document, block)?;
    if let Block::Paragraph(paragraph) = block {
      let Some(expected) = document.paragraphs.get(paragraph_ix) else {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "DB8 granular paragraph block count exceeds paragraphs",
        ));
      };
      if paragraph != expected {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 granular paragraph projection mismatch"));
      }
      if paragraph.byte_range != paragraph_byte_range(document, paragraph_ix) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 granular paragraph byte range mismatch"));
      }
      paragraph_ix += 1;
    }
  }
  if paragraph_ix != document.paragraphs.len() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "DB8 granular paragraph order exceeds paragraph blocks",
    ));
  }
  Ok(())
}

fn validate_block(document: &Document, block: &Block) -> io::Result<()> {
  match block {
    Block::Paragraph(_) => Ok(()),
    Block::Image(image) => {
      if !document.assets.assets.contains_key(&image.asset_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 image block references missing asset"));
      }
      if let Some(caption) = &image.caption {
        validate_paragraph_structure(caption)?;
      }
      Ok(())
    },
    Block::Equation(_) => Ok(()),
    Block::Table(table) => validate_table_block(table),
  }
}

fn validate_paragraph_structure(paragraph: &Paragraph) -> io::Result<()> {
  if paragraph_runs_len_from_runs(&paragraph.runs) != paragraph.byte_range.len() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 paragraph run length mismatch"));
  }
  Ok(())
}

fn validate_table_block(table: &TableBlock) -> io::Result<()> {
  for row in &table.rows {
    for cell in &row.cells {
      for block in &cell.blocks {
        match block {
          TableCellBlock::Paragraph(cell_paragraph) => {
            validate_utf8_range_in_text(
              &cell_paragraph.text,
              cell_paragraph.paragraph.byte_range.clone(),
              "DB8 table cell paragraph byte range",
            )?;
            validate_paragraph_structure(&cell_paragraph.paragraph)?;
            if cell_paragraph.paragraph.byte_range.len() != cell_paragraph.text.len() {
              return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 table cell paragraph text mismatch"));
            }
          },
          TableCellBlock::Table(nested) => validate_table_block(nested)?,
        }
      }
    }
  }
  Ok(())
}

fn validate_document_sections(document: &Document) -> io::Result<()> {
  let section_ids = document
    .sections
    .iter()
    .map(|section| section.id)
    .collect::<HashSet<_>>();
  let mut seen = HashSet::with_capacity(section_ids.len());
  for section in document.sections.iter() {
    if !seen.insert(section.id) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 section ID duplicate"));
    }
    validate_section_paragraph_ref(document, section.start_paragraph, "DB8 section start paragraph missing")?;
    if let Some(heading) = section.heading_paragraph {
      validate_section_paragraph_ref(document, heading, "DB8 section heading paragraph missing")?;
    }
    if let Some(end) = section.end_paragraph_exclusive {
      validate_section_paragraph_ref(document, end, "DB8 section end paragraph missing")?;
    }
    if let Some(parent) = section.parent_id
      && (parent == section.id || !section_ids.contains(&parent))
    {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 section parent missing"));
    }
  }
  Ok(())
}

fn validate_section_paragraph_ref(document: &Document, paragraph_id: ParagraphId, message: &'static str) -> io::Result<()> {
  if paragraph_index_for_id(document, paragraph_id).is_none() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, message));
  }
  Ok(())
}

fn validate_unique_ids<T: Eq + std::hash::Hash>(ids: impl IntoIterator<Item = T>, message: &'static str) -> io::Result<()> {
  let mut seen = HashSet::new();
  for id in ids {
    if !seen.insert(id) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    }
  }
  Ok(())
}

fn validate_utf8_range_in_text(text: &str, range: Range<usize>, message: &'static str) -> io::Result<()> {
  if range.start > range.end || range.end > text.len() || !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
    return Err(io::Error::new(io::ErrorKind::InvalidData, message));
  }
  Ok(())
}
include!("codec.rs");
include!("records.rs");

const DB8_PARAGRAPH_ORDER: &str = "paragraph_order";
const DB8_BLOCK_ORDER: &str = "block_order";
const DB8_MARK_SEMANTIC: &str = "semantic";
const DB8_MARK_DIRECT_UNDERLINE: &str = "direct_underline";
const DB8_MARK_STRIKETHROUGH: &str = "strikethrough";
const DB8_MARK_HIGHLIGHT: &str = "highlight";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct Db8GranularParagraphMetadata {
  style: ParagraphStyle,
  runs: Vec<TextRun>,
}

pub fn deserialize_paragraph_metadata(bytes: &[u8]) -> Option<(ParagraphStyle, Vec<TextRun>)> {
  postcard::from_bytes::<Db8GranularParagraphMetadata>(bytes)
    .ok()
    .map(|m| (m.style, m.runs))
}

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
const CHUNK_STYLE_MANIFEST: u8 = 8;
const BLOCK_PARAGRAPH: u8 = 0;
const BLOCK_IMAGE: u8 = 1;
const BLOCK_EQUATION: u8 = 2;
const BLOCK_TABLE: u8 = 3;
const TABLE_CELL_PARAGRAPH: u8 = 0;
const TABLE_CELL_TABLE: u8 = 1;

#[hotpath::measure]
#[expect(dead_code, reason = "kept as public persistence API for external callers and tests")]
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
#[expect(dead_code, reason = "used by collaboration and projection-cache integrations outside this crate")]
pub fn db8_projection_cache_bytes(document: &Document) -> io::Result<Vec<u8>> {
  let document = document_for_serialization(document);
  validate_document(&document)?;
  Ok(serialize_db8_projection(&document))
}

#[hotpath::measure]
pub fn read_db8_projection_cache_bytes(bytes: &[u8]) -> io::Result<Document> {
  read_db8_projection_bytes_with_timing(bytes, Instant::now())
}

#[hotpath::measure]
#[expect(dead_code, reason = "used by collaboration integrations outside this crate")]
pub fn db8_collab_document(document: &Document, created_by_actor: ActorId) -> io::Result<Db8CollabDocument> {
  let document = document_for_serialization(document);
  db8_collab_document_with_id_from_serialized(
    &document,
    CollabDocumentId(uuid::Uuid::from_u128(document.ids.document_id)),
    created_by_actor,
  )
}

#[hotpath::measure]
pub fn db8_collab_document_with_id(
  document: &Document,
  document_id: CollabDocumentId,
  created_by_actor: ActorId,
) -> io::Result<Db8CollabDocument> {
  let mut document = document_for_serialization(document);
  document.ids.document_id = document_id.0.as_u128();
  db8_collab_document_with_id_from_serialized(&document, document_id, created_by_actor)
}

fn db8_collab_document_with_id_from_serialized(
  document: &Document,
  document_id: CollabDocumentId,
  created_by_actor: ActorId,
) -> io::Result<Db8CollabDocument> {
  validate_document(document)?;
  let projection_cache = serialize_db8_projection(document);
  let asset_manifest = postcard::to_stdvec(&db8_native_asset_records(document, created_by_actor))
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  let source = db8_granular_source(document, projection_cache.clone())?;
  Db8CollabDocument::from_granular_source(document_id, created_by_actor, &source, &projection_cache, &asset_manifest).map_err(collab_to_io_error)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Db8ProjectionCacheRecovery {
  Reused,
  Rebuilt,
}

#[derive(Debug)]
struct Db8GranularMaterialization {
  document: Document,
  #[allow(
    dead_code,
    reason = "recovery is surfaced by collaboration fidelity tests and diagnostic materialization paths"
  )]
  recovery: Db8ProjectionCacheRecovery,
}

#[hotpath::measure]
pub fn document_from_db8_collab_source(source: &CollabDocument) -> io::Result<Document> {
  Ok(document_from_db8_collab_source_with_recovery(source)?.document)
}

#[hotpath::measure]
fn document_from_db8_collab_source_with_recovery(source: &CollabDocument) -> io::Result<Db8GranularMaterialization> {
  if source.format_kind() != FormatKind::Db8 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "collaboration source is not DB8"));
  }
  if let Some(granular) = source
    .materialize_granular_source()
    .map_err(collab_to_io_error)?
  {
    return Ok(Db8GranularMaterialization {
      document: document_from_db8_granular_source(&granular)?,
      recovery: Db8ProjectionCacheRecovery::Reused,
    });
  }
  Ok(Db8GranularMaterialization {
    document: read_db8_projection_cache_bytes(
      &source
        .materialize_projection_cache()
        .map_err(collab_to_io_error)?,
    )?,
    recovery: Db8ProjectionCacheRecovery::Rebuilt,
  })
}

#[hotpath::measure]
fn serialize_db8_native(document: &Document) -> io::Result<Vec<u8>> {
  let projection_cache = serialize_db8_projection(document);
  let mut input = native_file_input_for_document(document, projection_cache.clone());
  input.source_snapshot = Some(db8_source_snapshot_for_input(document, &input, &projection_cache)?);
  encode_native_file(input).map_err(collab_to_io_error)
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
fn serialize_db8_style_manifest(document: &Document) -> io::Result<Vec<u8>> {
  postcard::to_stdvec(&DocumentStyleManifest::from_theme(&document.theme)).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

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
  chunks.push((
    CHUNK_STYLE_MANIFEST,
    serialize_db8_style_manifest(document).expect("DB8 style manifest serializes"),
  ));

  let mut document_meta = Vec::new();
  write_u128(&mut document_meta, document.ids.document_id);
  chunks.push((CHUNK_DOCUMENT_META, document_meta));

  let table_entry_len = 1 + 1 + 2 + 8 + 8;
  let header_len = DB8_PROJECTION_MAGIC.len() + std::mem::size_of::<u32>() + std::mem::size_of::<u32>() + chunks.len() * table_entry_len;
  let payload_len = chunks.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
  let mut bytes = Vec::with_capacity(header_len + payload_len);
  bytes.extend_from_slice(DB8_PROJECTION_MAGIC);
  bytes.extend_from_slice(&DB8_PROJECTION_VERSION.to_le_bytes());
  write_u32(&mut bytes, u32::try_from(chunks.len()).expect("DB8 chunk count is fixed and fits in u32"));
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
  let assets = db8_native_asset_records(document, created_by_actor);

  let mut input = NativeFileInput::new(FormatKind::Db8, projection_cache);
  input.document_id = CollabDocumentId(uuid::Uuid::from_u128(document.ids.document_id));
  input.created_by_actor = created_by_actor;
  input.asset_inline_data = db8_asset_inline_data(document, &assets);
  input.asset_manifest = assets;
  input
}

#[hotpath::measure]
fn db8_source_snapshot_for_input(document: &Document, input: &NativeFileInput, projection_cache: &[u8]) -> io::Result<Vec<u8>> {
  let asset_manifest = postcard::to_stdvec(&input.asset_manifest).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  let source = db8_granular_source(document, projection_cache.to_vec())?;
  Db8CollabDocument::from_granular_source(input.document_id, input.created_by_actor, &source, projection_cache, &asset_manifest)
    .and_then(|document| document.export_snapshot())
    .map_err(collab_to_io_error)
}

#[hotpath::measure]
fn db8_granular_source(document: &Document, projection_cache: Vec<u8>) -> io::Result<GranularSource> {
  let mut paragraph_records = Vec::with_capacity(document.paragraphs.len());
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let Some(paragraph_id) = document.ids.paragraph_ids.get(paragraph_ix).copied() else {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 granular paragraph ID missing"));
    };
    let metadata = postcard::to_stdvec(&Db8GranularParagraphMetadata {
      style: paragraph.style,
      runs: paragraph.runs.clone(),
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    paragraph_records.push(GranularTextRecord {
      id: granular_record_id_u128(paragraph_id.0),
      text: document_text_slice(document, paragraph.byte_range.clone()),
      metadata,
      marks: db8_marks_from_runs(&paragraph.runs),
    });
  }

  let paragraph_order = document
    .ids
    .paragraph_ids
    .iter()
    .map(|id| granular_record_id_u128(id.0))
    .collect::<Vec<_>>();
  let block_order = document
    .ids
    .block_ids
    .iter()
    .map(|id| granular_record_id_u128(id.0))
    .collect::<Vec<_>>();
  let binaries = document
    .blocks
    .iter()
    .zip(document.ids.block_ids.iter())
    .map(|(block, id)| {
      let mut metadata = Vec::new();
      write_block_record(&mut metadata, block);
      GranularBinaryRecord {
        id: granular_record_id_u128(id.0),
        metadata,
      }
    })
    .collect::<Vec<_>>();

  Ok(GranularSource {
    metadata: postcard::to_stdvec(&Db8GranularSourceMetadata {
      projection_cache,
      style_manifest: DocumentStyleManifest::from_theme(&document.theme),
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    orders: vec![
      GranularOrderRecord {
        name: DB8_PARAGRAPH_ORDER.to_string(),
        ids: paragraph_order,
      },
      GranularOrderRecord {
        name: DB8_BLOCK_ORDER.to_string(),
        ids: block_order,
      },
    ],
    texts: paragraph_records,
    binaries,
  })
}

#[hotpath::measure]
fn db8_document_and_manifest(source: &GranularSource) -> io::Result<(Document, Option<DocumentStyleManifest>)> {
  match postcard::from_bytes::<Db8GranularSourceMetadata>(&source.metadata) {
    Ok(metadata) => Ok((
      read_db8_projection_cache_bytes(&metadata.projection_cache)?,
      Some(metadata.style_manifest),
    )),
    Err(_) => Ok((read_db8_projection_cache_bytes(&source.metadata)?, None)),
  }
}

#[hotpath::measure]
fn document_from_db8_granular_source(source: &GranularSource) -> io::Result<Document> {
  let (mut document, style_manifest) = db8_document_and_manifest(source)?;
  if let Some(style_manifest) = style_manifest {
    style_manifest.apply_to_theme(&mut document.theme);
  }
  let paragraph_ids = db8_order_u128(source, DB8_PARAGRAPH_ORDER)?
    .unwrap_or_else(|| document.ids.paragraph_ids.iter().map(|id| id.0).collect())
    .into_iter()
    .map(ParagraphId)
    .collect::<Vec<_>>();
  let block_ids = db8_order_u128(source, DB8_BLOCK_ORDER)?
    .unwrap_or_else(|| document.ids.block_ids.iter().map(|id| id.0).collect())
    .into_iter()
    .map(BlockId)
    .collect::<Vec<_>>();
  let records = db8_paragraph_records(source)?;
  let base_paragraphs = document
    .ids
    .paragraph_ids
    .iter()
    .copied()
    .zip(document.paragraphs.iter().cloned())
    .collect::<std::collections::HashMap<_, _>>();

  let mut text = String::new();
  let mut paragraphs = Vec::with_capacity(paragraph_ids.len());
  for (paragraph_ix, paragraph_id) in paragraph_ids.iter().copied().enumerate() {
    if paragraph_ix > 0 {
      text.push('\n');
    }
    let Some(record) = records.get(&paragraph_id) else {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 granular paragraph record missing"));
    };
    let metadata = postcard::from_bytes::<Db8GranularParagraphMetadata>(&record.metadata)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut paragraph = base_paragraphs
      .get(&paragraph_id)
      .cloned()
      .unwrap_or(Paragraph {
        style: metadata.style,
        byte_range: 0..0,
        runs: Vec::new(),
        version: 0,
      });
    paragraph.style = metadata.style;
    paragraph.runs = if record.marks.is_empty() {
      if paragraph_runs_len_from_runs(&metadata.runs) == record.text.len() {
        metadata.runs
      } else {
        db8_runs_from_marks(record.text.len(), &record.marks)
      }
    } else {
      db8_runs_from_marks(record.text.len(), &record.marks)
    };
    paragraph.byte_range = text.len()..text.len() + record.text.len();
    text.push_str(&record.text);
    paragraphs.push(paragraph);
  }

  document.text = Rope::from(text);
  document.paragraphs = Arc::new(paragraphs);
  document.ids.paragraph_ids = paragraph_ids;
  document.ids.block_ids = block_ids.clone();
  rebuild_document_offset_index(&mut document);
  let block_records = db8_block_records(source)?;
  document.blocks = Arc::new(db8_materialize_blocks(&document, &block_ids, &block_records)?);
  rebuild_document_sections(&mut document);
  validate_document(&document)?;
  Ok(document)
}

fn db8_paragraph_records(source: &GranularSource) -> io::Result<std::collections::HashMap<ParagraphId, &GranularTextRecord>> {
  let mut records = std::collections::HashMap::with_capacity(source.texts.len());
  for record in &source.texts {
    let id = granular_record_id_to_u128(&record.id)
      .map(ParagraphId)
      .map_err(collab_to_io_error)?;
    if records.insert(id, record).is_some() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate DB8 granular paragraph record"));
    }
  }
  Ok(records)
}

fn db8_block_records(source: &GranularSource) -> io::Result<std::collections::HashMap<BlockId, Block>> {
  let mut records = std::collections::HashMap::with_capacity(source.binaries.len());
  for record in &source.binaries {
    if record.metadata.is_empty() {
      continue;
    }
    let id = granular_record_id_to_u128(&record.id)
      .map(BlockId)
      .map_err(collab_to_io_error)?;
    let mut cursor = Cursor::new(record.metadata.as_slice());
    let block = read_block_record(&mut cursor)?;
    if records.insert(id, block).is_some() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate DB8 granular block record"));
    }
  }
  Ok(records)
}

fn db8_materialize_blocks(
  document: &Document,
  block_ids: &[BlockId],
  block_records: &std::collections::HashMap<BlockId, Block>,
) -> io::Result<Vec<Block>> {
  let mut paragraph_iter = document.paragraphs.iter();
  let mut blocks = Vec::with_capacity(block_ids.len());
  for block_id in block_ids {
    let block = block_records
      .get(block_id)
      .cloned()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 granular block record missing"))?;
    match block {
      Block::Paragraph(_) => {
        let Some(paragraph) = paragraph_iter.next() else {
          return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "DB8 granular paragraph block count exceeds paragraphs",
          ));
        };
        blocks.push(Block::Paragraph(paragraph.clone()));
      },
      other => blocks.push(other),
    }
  }
  if paragraph_iter.next().is_some() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "DB8 granular paragraph order exceeds paragraph blocks",
    ));
  }
  Ok(blocks)
}

fn db8_order_u128(source: &GranularSource, name: &str) -> io::Result<Option<Vec<u128>>> {
  let Some(order) = source.orders.iter().find(|order| order.name == name) else {
    return Ok(None);
  };
  let mut seen = std::collections::HashSet::with_capacity(order.ids.len());
  let mut ids = Vec::with_capacity(order.ids.len());
  for id in &order.ids {
    let parsed = granular_record_id_to_u128(id).map_err(collab_to_io_error)?;
    if !seen.insert(parsed) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate DB8 granular order ID"));
    }
    ids.push(parsed);
  }
  Ok(Some(ids))
}
#[allow(dead_code, reason = "staged granular table ordering helper for collaborative table records")]
fn db8_table_row_order_name(block_id: BlockId) -> String {
  format!("{DB8_BLOCK_ORDER}/table/{:032x}/rows", block_id.0)
}

#[allow(dead_code, reason = "staged granular table ordering helper for collaborative table records")]
fn db8_table_cell_order_name(block_id: BlockId, row_ix: usize) -> String {
  format!("{DB8_BLOCK_ORDER}/table/{:032x}/rows/{row_ix}/cells", block_id.0)
}

#[allow(dead_code, reason = "staged granular table ordering helper for collaborative table records")]
fn db8_table_row_order_ids(table: &TableBlock, block_id: BlockId) -> Vec<u128> {
  table.row_order_ids(block_id.0)
}

#[allow(dead_code, reason = "staged granular table ordering helper for collaborative table records")]
fn db8_table_cell_order_ids(table: &TableBlock, block_id: BlockId, row_ix: usize) -> Vec<u128> {
  table.cell_order_ids(block_id.0, row_ix)
}

#[hotpath::measure]
fn db8_marks_from_runs(runs: &[TextRun]) -> Vec<GranularTextMark> {
  let mut marks = Vec::new();
  let mut start = 0;
  for run in runs {
    let end = start + run.len;
    if run.styles.semantic != RunSemanticStyle::Plain {
      marks.push(GranularTextMark {
        start_utf8: start,
        end_utf8: end,
        key: DB8_MARK_SEMANTIC.to_string(),
        value: GranularValue::I64(run_semantic_code(run.styles.semantic)),
      });
    }
    if run.styles.direct_underline {
      marks.push(GranularTextMark {
        start_utf8: start,
        end_utf8: end,
        key: DB8_MARK_DIRECT_UNDERLINE.to_string(),
        value: GranularValue::Bool(true),
      });
    }
    if run.styles.strikethrough {
      marks.push(GranularTextMark {
        start_utf8: start,
        end_utf8: end,
        key: DB8_MARK_STRIKETHROUGH.to_string(),
        value: GranularValue::Bool(true),
      });
    }
    if let Some(highlight) = run.styles.highlight {
      marks.push(GranularTextMark {
        start_utf8: start,
        end_utf8: end,
        key: DB8_MARK_HIGHLIGHT.to_string(),
        value: GranularValue::I64(highlight_code(highlight)),
      });
    }
    start = end;
  }
  marks
}

#[hotpath::measure]
fn db8_runs_from_marks(text_len: usize, marks: &[GranularTextMark]) -> Vec<TextRun> {
  let mut boundaries = vec![0, text_len];
  for mark in marks {
    boundaries.push(mark.start_utf8.min(text_len));
    boundaries.push(mark.end_utf8.min(text_len));
  }
  boundaries.sort_unstable();
  boundaries.dedup();

  let mut runs = Vec::new();
  for window in boundaries.windows(2) {
    let start = window[0];
    let end = window[1];
    if start == end {
      continue;
    }
    let mut styles = RunStyles::default();
    for mark in marks {
      if mark.start_utf8 <= start && mark.end_utf8 >= end {
        apply_db8_mark(&mut styles, mark);
      }
    }
    runs.push(TextRun { len: end - start, styles });
  }
  runs = merge_adjacent_runs(runs);
  if runs.is_empty() && text_len > 0 {
    runs.push(TextRun {
      len: text_len,
      styles: RunStyles::default(),
    });
  }
  runs
}

fn apply_db8_mark(styles: &mut RunStyles, mark: &GranularTextMark) {
  match (mark.key.as_str(), &mark.value) {
    (DB8_MARK_SEMANTIC, GranularValue::I64(value)) => styles.semantic = run_semantic_from_code(*value),
    (DB8_MARK_DIRECT_UNDERLINE, GranularValue::Bool(value)) => styles.direct_underline = *value,
    (DB8_MARK_STRIKETHROUGH, GranularValue::Bool(value)) => styles.strikethrough = *value,
    (DB8_MARK_HIGHLIGHT, GranularValue::I64(value)) => styles.highlight = highlight_from_code(*value),
    _ => {},
  }
}

fn run_semantic_code(style: RunSemanticStyle) -> i64 {
  if style == RunSemanticStyle::Plain {
    0
  } else if style == SEMANTIC_CITE {
    1
  } else if style == SEMANTIC_EMPHASIS {
    2
  } else if style == SEMANTIC_UNDERLINE {
    3
  } else if style == SEMANTIC_CONDENSED {
    4
  } else if style == SEMANTIC_ULTRACONDENSED {
    5
  } else if let RunSemanticStyle::Custom(slot) = style {
    i64::from(slot)
  } else {
    0
  }
}

fn run_semantic_from_code(value: i64) -> RunSemanticStyle {
  match value {
    1 => SEMANTIC_CITE,
    2 => SEMANTIC_EMPHASIS,
    3 => SEMANTIC_UNDERLINE,
    4 => SEMANTIC_CONDENSED,
    5 => SEMANTIC_ULTRACONDENSED,
    value if (0..=u8::MAX as i64).contains(&value) => RunSemanticStyle::Custom(value as u8),
    _ => RunSemanticStyle::Plain,
  }
}

fn highlight_code(style: HighlightStyle) -> i64 {
  match style {
    HIGHLIGHT_SPOKEN => 1,
    HIGHLIGHT_INSERT => 2,
    HIGHLIGHT_ALTERNATIVE => 3,
    HighlightStyle::Custom(slot) => i64::from(slot),
  }
}

fn highlight_from_code(value: i64) -> Option<HighlightStyle> {
  match value {
    1 => Some(HIGHLIGHT_SPOKEN),
    2 => Some(HIGHLIGHT_INSERT),
    3 => Some(HIGHLIGHT_ALTERNATIVE),
    value if (0..=u8::MAX as i64).contains(&value) => Some(HighlightStyle::Custom(value as u8)),
    _ => None,
  }
}

fn paragraph_runs_len_from_runs(runs: &[TextRun]) -> usize {
  runs.iter().map(|run| run.len).sum()
}

#[hotpath::measure]
fn db8_native_asset_records(document: &Document, created_by_actor: ActorId) -> Vec<NativeAssetRecord> {
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
  assets
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
  temp_path.persist(path).map_err(|error| error.error)
}

#[cfg(test)]
mod collab_source_tests {
  use super::*;

  #[test]
  #[hotpath::measure]
  fn db8_collab_source_materializes_projection_with_recovery() {
    let document = demo_document();
    let source = db8_collab_document(&document, ActorId::new()).unwrap();
    let materialized = document_from_db8_collab_source_with_recovery(source.inner()).unwrap();
    assert_eq!(materialized.document.ids.document_id, document.ids.document_id);
    assert_eq!(materialized.document.paragraphs.len(), document.paragraphs.len());
    assert_eq!(materialized.recovery, Db8ProjectionCacheRecovery::Reused);
  }

  #[test]
  #[hotpath::measure]
  fn db8_collab_source_roundtrips_style_manifest_through_granular_metadata() {
    let mut document = demo_document();
    let style = document
      .theme
      .custom_paragraph_styles
      .get(&0)
      .cloned()
      .unwrap();
    document.theme.set_custom_paragraph_style(7, style.clone());
    let source = db8_collab_document(&document, ActorId::new()).unwrap();
    let materialized = document_from_db8_collab_source(source.inner()).unwrap();
    let roundtripped = &materialized.theme.custom_paragraph_styles[&7];
    assert_eq!(roundtripped.font_size, style.font_size);
    assert_eq!(roundtripped.bold, style.bold);
    assert_eq!(roundtripped.italic, style.italic);
    assert_eq!(roundtripped.section_kind, style.section_kind);
  }

  #[test]
  #[hotpath::measure]
  fn db8_bytes_roundtrip_preserves_style_manifest_chunk() {
    let document = demo_document();
    let bytes = db8_bytes(&document).unwrap();
    let reread = read_db8_bytes(&bytes).unwrap();
    assert_eq!(db8_bytes(&reread).unwrap(), bytes);
  }

  #[test]
  #[hotpath::measure]
  fn db8_projection_only_source_reports_rebuild() {
    let document = demo_document();
    let actor = ActorId::new();
    let source = Db8CollabDocument::from_projection_source(
      CollabDocumentId::new(),
      actor,
      &serialize_db8_projection(&document),
      &postcard::to_stdvec(&db8_native_asset_records(&document, actor)).unwrap(),
    )
    .unwrap();
    let materialized = document_from_db8_collab_source_with_recovery(source.inner()).unwrap();
    assert_eq!(materialized.recovery, Db8ProjectionCacheRecovery::Rebuilt);
  }

  #[test]
  #[hotpath::measure]
  fn db8_granular_text_update_materializes_into_projection() {
    let document = demo_document();
    let source = db8_collab_document(&document, ActorId::new()).unwrap();
    let paragraph_id = document.ids.paragraph_ids[0];
    let text_id = granular_record_id_u128(paragraph_id.0);
    source
      .inner()
      .insert_granular_text_utf8(flowstate_collab::Role::Owner, &text_id, 0, "SYNC ")
      .unwrap();

    let materialized = document_from_db8_collab_source(source.inner()).unwrap();
    let first_text = document_text_slice(&materialized, paragraph_byte_range(&materialized, 0));
    assert!(first_text.starts_with("SYNC "));
  }

  #[test]
  #[hotpath::measure]
  fn db8_granular_order_rebuilds_paragraph_projection() {
    let document = crate::document_from_input(
      crate::DocumentTheme::default(),
      vec![
        crate::InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![crate::InputRun {
            text: "first".to_string(),
            styles: RunStyles::default(),
          }],
        },
        crate::InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![crate::InputRun {
            text: "second".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
    );
    let document = document_for_serialization(&document);
    let mut source = db8_granular_source(&document, serialize_db8_projection(&document)).unwrap();
    for order in &mut source.orders {
      if order.name == DB8_PARAGRAPH_ORDER || order.name == DB8_BLOCK_ORDER {
        order.ids.reverse();
      }
    }

    let materialized = document_from_db8_granular_source(&source).unwrap();
    assert_eq!(document_text_slice(&materialized, paragraph_byte_range(&materialized, 0)), "second");
    assert_eq!(document_text_slice(&materialized, paragraph_byte_range(&materialized, 1)), "first");
  }
  #[test]
  fn db8_validate_document_rejects_missing_section_paragraph_reference() {
    let mut document = demo_document();
    document.sections = Arc::new(vec![DocumentSection {
      id: SectionId(1),
      parent_id: None,
      kind: SectionKind::Custom(1),
      heading_paragraph: Some(ParagraphId(u128::MAX)),
      start_paragraph: document.ids.paragraph_ids[0],
      end_paragraph_exclusive: None,
    }]);

    let error = validate_document(&document).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn db8_validate_document_rejects_misaligned_paragraph_blocks() {
    let mut document = demo_document();
    let mut blocks = document.blocks.as_ref().clone();
    if let Some(Block::Paragraph(paragraph)) = blocks.get_mut(0) {
      paragraph.byte_range = 1..paragraph.byte_range.end;
    }
    document.blocks = Arc::new(blocks);

    let error = validate_document(&document).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  #[hotpath::measure]
  fn db8_granular_marks_materialize_run_styles_and_override_metadata() {
    let document = crate::document_from_input(
      crate::DocumentTheme::default(),
      vec![crate::InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![crate::InputRun {
          text: "styled".to_string(),
          styles: RunStyles {
            semantic: SEMANTIC_EMPHASIS,
            direct_underline: true,
            strikethrough: true,
            highlight: Some(HIGHLIGHT_INSERT),
          },
        }],
      }],
    );
    let document = document_for_serialization(&document);
    let mut source = db8_granular_source(&document, serialize_db8_projection(&document)).unwrap();
    {
      let record = &mut source.texts[0];
      record.metadata = postcard::to_stdvec(&Db8GranularParagraphMetadata {
        style: ParagraphStyle::Normal,
        runs: vec![TextRun {
          len: record.text.len(),
          styles: RunStyles::default(),
        }],
      })
      .unwrap();
    };

    let materialized = document_from_db8_granular_source(&source).unwrap();
    assert_eq!(
      materialized.paragraphs[0].runs,
      vec![TextRun {
        len: "styled".len(),
        styles: RunStyles {
          semantic: SEMANTIC_EMPHASIS,
          direct_underline: true,
          strikethrough: true,
          highlight: Some(HIGHLIGHT_INSERT),
        },
      }]
    );

    {
      let record = &mut source.texts[0];
      record.marks.clear();
      record.metadata = postcard::to_stdvec(&Db8GranularParagraphMetadata {
        style: ParagraphStyle::Normal,
        runs: vec![TextRun {
          len: record.text.len(),
          styles: RunStyles {
            semantic: SEMANTIC_CITE,
            direct_underline: false,
            strikethrough: false,
            highlight: None,
          },
        }],
      })
      .unwrap();
    };

    let migrated = document_from_db8_granular_source(&source).unwrap();
    assert_eq!(
      migrated.paragraphs[0].runs,
      vec![TextRun {
        len: "styled".len(),
        styles: RunStyles {
          semantic: SEMANTIC_CITE,
          direct_underline: false,
          strikethrough: false,
          highlight: None,
        },
      }]
    );
  }

  #[test]
  #[hotpath::measure]
  fn db8_table_record_helpers_are_stable() {
    let table = TableBlock {
      rows: vec![
        TableRow {
          cells: vec![
            TableCell {
              blocks: vec![],
              row_span: 1,
              col_span: 1,
            },
            TableCell {
              blocks: vec![],
              row_span: 1,
              col_span: 1,
            },
          ],
        },
        TableRow {
          cells: vec![TableCell {
            blocks: vec![],
            row_span: 1,
            col_span: 1,
          }],
        },
      ],
      column_widths: vec![TableColumnWidth::Fraction(1), TableColumnWidth::Fraction(1)],
      style: TableStyle { header_row: false },
      version: 0,
    };
    let block_id = BlockId(0x1234);
    assert_eq!(
      db8_table_row_order_name(block_id),
      "block_order/table/00000000000000000000000000001234/rows"
    );
    assert_eq!(
      db8_table_cell_order_name(block_id, 1),
      "block_order/table/00000000000000000000000000001234/rows/1/cells"
    );
    assert_eq!(db8_table_row_order_ids(&table, block_id), db8_table_row_order_ids(&table, block_id));
    assert_eq!(
      db8_table_cell_order_ids(&table, block_id, 0),
      db8_table_cell_order_ids(&table, block_id, 0)
    );
    assert_ne!(
      db8_table_row_order_ids(&table, block_id)[0],
      db8_table_cell_order_ids(&table, block_id, 0)[0]
    );
  }

  #[test]
  #[hotpath::measure]
  fn db8_materialize_blocks_is_deterministic_for_tables() {
    let table = TableBlock {
      rows: vec![TableRow {
        cells: vec![TableCell {
          blocks: vec![],
          row_span: 1,
          col_span: 1,
        }],
      }],
      column_widths: vec![TableColumnWidth::Fraction(1)],
      style: TableStyle { header_row: false },
      version: 7,
    };
    let document = Document {
      text: Rope::from(""),
      paragraphs: Arc::new(Vec::new()),
      blocks: Arc::new(Vec::new()),
      assets: AssetStore::default(),
      ids: DocumentIds {
        document_id: 1,
        paragraph_ids: Vec::new(),
        block_ids: vec![BlockId(1)],
      },
      sections: Arc::new(Vec::new()),
      offset_index: ParagraphOffsetIndex {
        widths: Vec::new(),
        tree: Vec::new(),
      },
      theme: DocumentTheme::default(),
    };
    let mut block_records = std::collections::HashMap::new();
    block_records.insert(BlockId(1), Block::Table(table.clone()));
    let materialized = db8_materialize_blocks(&document, &[BlockId(1)], &block_records).unwrap();
    assert_eq!(materialized, vec![Block::Table(table)]);
  }
}
