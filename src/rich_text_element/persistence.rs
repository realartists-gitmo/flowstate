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
  let mut bytes = Vec::new();
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

  let blocks = serializable_blocks(document);
  write_u64(&mut bytes, blocks.len() as u64);
  for block in &blocks {
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

fn read_db8_current(mut cursor: Cursor<&[u8]>, timing: Instant) -> io::Result<Document> {
  let text_len = {
    let raw = read_u64(&mut cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 text length overflows usize"))?
  };
  let mut text_bytes = vec![0; text_len];
  cursor.read_exact(&mut text_bytes)?;
  let text = String::from_utf8(text_bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 text is not UTF-8"))?;

  let asset_count = {
    let raw = read_u64(&mut cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 asset count overflows usize"))?
  };
  let mut assets = AssetStore::default();
  for _ in 0..asset_count {
    let asset = read_asset_record(&mut cursor)?;
    assets.assets.insert(asset.id, asset);
  }

  let block_count = {
    let raw = read_u64(&mut cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 block count overflows usize"))?
  };
  let mut blocks = Vec::with_capacity(block_count.min(4096));
  let mut paragraphs = Vec::new();
  for _ in 0..block_count {
    let block = read_block_record(&mut cursor)?;
    if let Block::Paragraph(paragraph) = &block {
      paragraphs.push(paragraph.clone());
    }
    blocks.push(block);
  }
  if paragraphs.is_empty() {
    paragraphs.push(Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    });
    blocks.push(Block::Paragraph(paragraphs[0].clone()));
  }

  let offset_index = ParagraphOffsetIndex::new(&paragraphs);
  let document = Document {
    text: Rope::from(text),
    paragraphs: Arc::new(paragraphs),
    blocks: Arc::new(blocks),
    assets,
    offset_index,
    theme: DocumentTheme::default(),
  };
  validate_document(&document)?;
  log_timing(
    "db8 read",
    timing,
    format!(
      "bytes={} blocks={} paragraphs={}",
      document.text.byte_len(),
      document.blocks.len(),
      document.paragraphs.len()
    ),
  );
  Ok(document)
}

fn read_block_record(cursor: &mut Cursor<&[u8]>) -> io::Result<Block> {
  let kind = read_u8(cursor)?;
  let payload_len = {
    let raw = read_u64(cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 block payload length overflows usize"))?
  };
  let mut payload = vec![0; payload_len];
  cursor.read_exact(&mut payload)?;
  let mut payload = Cursor::new(payload.as_slice());
  match kind {
    BLOCK_PARAGRAPH => read_paragraph_payload(&mut payload).map(Block::Paragraph),
    BLOCK_IMAGE => read_image_payload(&mut payload).map(Block::Image),
    BLOCK_EQUATION => read_equation_payload(&mut payload).map(Block::Equation),
    BLOCK_TABLE => read_table_payload(&mut payload).map(Block::Table),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 block kind")),
  }
}

fn write_block_record(bytes: &mut Vec<u8>, block: &Block) {
  let mut payload = Vec::new();
  let kind = match block {
    Block::Paragraph(paragraph) => {
      write_paragraph_payload(&mut payload, paragraph, paragraph.byte_range.clone());
      BLOCK_PARAGRAPH
    },
    Block::Image(image) => {
      write_image_payload(&mut payload, image);
      BLOCK_IMAGE
    },
    Block::Equation(equation) => {
      write_equation_payload(&mut payload, equation);
      BLOCK_EQUATION
    },
    Block::Table(table) => {
      write_table_payload(&mut payload, table);
      BLOCK_TABLE
    },
  };
  bytes.push(kind);
  write_u64(bytes, payload.len() as u64);
  bytes.extend_from_slice(&payload);
}

fn read_paragraph_payload(cursor: &mut Cursor<&[u8]>) -> io::Result<Paragraph> {
  let style = decode_paragraph_style(read_u8(cursor)?)?;
  let start = {
    let raw = read_u64(cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 paragraph start overflows usize"))?
  };
  let end = {
    let raw = read_u64(cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 paragraph end overflows usize"))?
  };
  let run_count = {
    let raw = read_u64(cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 run count overflows usize"))?
  };
  let mut runs = Vec::with_capacity(run_count.min(4096));
  for _ in 0..run_count {
    let len = {
      let raw = read_u64(cursor)?;
      usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 run length overflows usize"))?
    };
    let styles = read_run_styles(cursor)?;
    runs.push(TextRun { len, styles });
  }
  Ok(Paragraph {
    style,
    byte_range: start..end,
    runs: merge_adjacent_runs(runs),
    version: 0,
  })
}

fn write_paragraph_payload(bytes: &mut Vec<u8>, paragraph: &Paragraph, range: Range<usize>) {
  bytes.push(encode_paragraph_style(paragraph.style));
  write_u64(bytes, range.start as u64);
  write_u64(bytes, range.end as u64);
  write_u64(bytes, paragraph.runs.len() as u64);
  for run in &paragraph.runs {
    write_u64(bytes, run.len as u64);
    write_run_styles(bytes, run.styles);
  }
}

fn read_image_payload(cursor: &mut Cursor<&[u8]>) -> io::Result<ImageBlock> {
  let asset_id = AssetId(read_u128(cursor)?);
  let alt_text = read_string(cursor)?.into();
  let caption = if read_u8(cursor)? == 1 {
    Some(read_paragraph_payload(cursor)?)
  } else {
    None
  };
  let sizing = match read_u8(cursor)? {
    0 => ImageSizing::Intrinsic,
    1 => ImageSizing::FitWidth,
    2 => {
      let width_px = read_u32(cursor)?;
      let height_px = if read_u8(cursor)? == 1 { Some(read_u32(cursor)?) } else { None };
      ImageSizing::Fixed { width_px, height_px }
    },
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid image sizing")),
  };
  let alignment = decode_block_alignment(read_u8(cursor)?)?;
  Ok(ImageBlock {
    asset_id,
    alt_text,
    caption,
    sizing,
    alignment,
    version: 0,
  })
}

fn write_image_payload(bytes: &mut Vec<u8>, image: &ImageBlock) {
  write_u128(bytes, image.asset_id.0);
  write_string(bytes, image.alt_text.as_ref());
  match &image.caption {
    Some(caption) => {
      bytes.push(1);
      write_paragraph_payload(bytes, caption, caption.byte_range.clone());
    },
    None => bytes.push(0),
  }
  match image.sizing {
    ImageSizing::Intrinsic => bytes.push(0),
    ImageSizing::FitWidth => bytes.push(1),
    ImageSizing::Fixed { width_px, height_px } => {
      bytes.push(2);
      bytes.extend_from_slice(&width_px.to_le_bytes());
      match height_px {
        Some(height_px) => {
          bytes.push(1);
          bytes.extend_from_slice(&height_px.to_le_bytes());
        },
        None => bytes.push(0),
      }
    },
  }
  bytes.push(encode_block_alignment(image.alignment));
}

fn read_equation_payload(cursor: &mut Cursor<&[u8]>) -> io::Result<EquationBlock> {
  let syntax = match read_u8(cursor)? {
    0 => EquationSyntax::Latex,
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid equation syntax")),
  };
  let display = match read_u8(cursor)? {
    0 => EquationDisplay::Display,
    1 => EquationDisplay::InlineLikeParagraph,
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid equation display")),
  };
  Ok(EquationBlock {
    source: read_string(cursor)?.into(),
    syntax,
    display,
    version: 0,
  })
}

fn write_equation_payload(bytes: &mut Vec<u8>, equation: &EquationBlock) {
  bytes.push(match equation.syntax {
    EquationSyntax::Latex => 0,
  });
  bytes.push(match equation.display {
    EquationDisplay::Display => 0,
    EquationDisplay::InlineLikeParagraph => 1,
  });
  write_string(bytes, equation.source.as_ref());
}

fn read_table_payload(cursor: &mut Cursor<&[u8]>) -> io::Result<TableBlock> {
  let column_count = read_len(cursor, "DB8 table column count")?;
  let mut column_widths = Vec::with_capacity(column_count.min(64));
  for _ in 0..column_count {
    column_widths.push(match read_u8(cursor)? {
      0 => TableColumnWidth::Auto,
      1 => TableColumnWidth::FixedPx(read_u32(cursor)?),
      2 => TableColumnWidth::Fraction(read_u32(cursor)?),
      _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid table column width")),
    });
  }
  let header_row = read_u8(cursor)? != 0;
  let row_count = read_len(cursor, "DB8 table row count")?;
  let mut rows = Vec::with_capacity(row_count.min(4096));
  for _ in 0..row_count {
    let cell_count = read_len(cursor, "DB8 table cell count")?;
    let mut cells = Vec::with_capacity(cell_count.min(128));
    for _ in 0..cell_count {
      let row_span = read_u16(cursor)?;
      let col_span = read_u16(cursor)?;
      let block_count = read_len(cursor, "DB8 table cell block count")?;
      let mut blocks = Vec::with_capacity(block_count.min(64));
      for _ in 0..block_count {
        blocks.push(read_table_cell_block(cursor)?);
      }
      cells.push(TableCell { blocks, row_span, col_span });
    }
    rows.push(TableRow { cells });
  }
  Ok(TableBlock {
    rows,
    column_widths,
    style: TableStyle { header_row },
    version: 0,
  })
}

fn write_table_payload(bytes: &mut Vec<u8>, table: &TableBlock) {
  write_u64(bytes, table.column_widths.len() as u64);
  for width in &table.column_widths {
    match *width {
      TableColumnWidth::Auto => bytes.push(0),
      TableColumnWidth::FixedPx(px) => {
        bytes.push(1);
        bytes.extend_from_slice(&px.to_le_bytes());
      },
      TableColumnWidth::Fraction(fraction) => {
        bytes.push(2);
        bytes.extend_from_slice(&fraction.to_le_bytes());
      },
    }
  }
  bytes.push(u8::from(table.style.header_row));
  write_u64(bytes, table.rows.len() as u64);
  for row in &table.rows {
    write_u64(bytes, row.cells.len() as u64);
    for cell in &row.cells {
      bytes.extend_from_slice(&cell.row_span.to_le_bytes());
      bytes.extend_from_slice(&cell.col_span.to_le_bytes());
      write_u64(bytes, cell.blocks.len() as u64);
      for block in &cell.blocks {
        write_table_cell_block(bytes, block);
      }
    }
  }
}

fn read_table_cell_block(cursor: &mut Cursor<&[u8]>) -> io::Result<TableCellBlock> {
  match read_u8(cursor)? {
    TABLE_CELL_PARAGRAPH => {
      let text = read_string(cursor)?;
      let paragraph = read_paragraph_payload(cursor)?;
      Ok(TableCellBlock::Paragraph(TableCellParagraph { paragraph, text }))
    },
    TABLE_CELL_TABLE => read_table_payload(cursor).map(TableCellBlock::Table),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid table cell block kind")),
  }
}

fn write_table_cell_block(bytes: &mut Vec<u8>, block: &TableCellBlock) {
  match block {
    TableCellBlock::Paragraph(paragraph) => {
      bytes.push(TABLE_CELL_PARAGRAPH);
      write_string(bytes, &paragraph.text);
      write_paragraph_payload(bytes, &paragraph.paragraph, 0..paragraph.text.len());
    },
    TableCellBlock::Table(table) => {
      bytes.push(TABLE_CELL_TABLE);
      write_table_payload(bytes, table);
    },
  }
}

fn read_asset_record(cursor: &mut Cursor<&[u8]>) -> io::Result<AssetRecord> {
  let id = AssetId(read_u128(cursor)?);
  let mime_type = read_string(cursor)?.into();
  let original_name = if read_u8(cursor)? == 1 {
    Some(read_string(cursor)?.into())
  } else {
    None
  };
  let content_hash = read_u64(cursor)?;
  let byte_len = read_len(cursor, "DB8 asset byte length")?;
  let mut bytes = vec![0; byte_len];
  cursor.read_exact(&mut bytes)?;
  Ok(AssetRecord {
    id,
    mime_type,
    original_name,
    content_hash,
    bytes: Arc::new(bytes),
  })
}

fn write_asset_record(bytes: &mut Vec<u8>, asset: &AssetRecord) {
  write_u128(bytes, asset.id.0);
  write_string(bytes, asset.mime_type.as_ref());
  match &asset.original_name {
    Some(name) => {
      bytes.push(1);
      write_string(bytes, name.as_ref());
    },
    None => bytes.push(0),
  }
  write_u64(bytes, asset.content_hash);
  write_u64(bytes, asset.bytes.len() as u64);
  bytes.extend_from_slice(&asset.bytes);
}

pub(super) fn recovery_path_for_document(path: &PathBuf) -> PathBuf {
  let mut recovery_path = path.clone();
  let file_name = path
    .file_name()
    .and_then(|name| name.to_str())
    .map(|name| format!("{name}.recovery"))
    .unwrap_or_else(|| "untitled.db8.recovery".to_string());
  recovery_path.set_file_name(file_name);
  recovery_path
}

#[allow(dead_code)]
fn document_fingerprint(document: &Document) -> u64 {
  let mut hasher = DefaultHasher::new();
  document_text_slice(document, 0..document.text.byte_len()).hash(&mut hasher);
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let range = paragraph_byte_range(document, paragraph_ix);
    paragraph.style.hash(&mut hasher);
    range.start.hash(&mut hasher);
    range.end.hash(&mut hasher);
    paragraph.runs.hash(&mut hasher);
  }
  hasher.finish()
}

fn validate_document(document: &Document) -> io::Result<()> {
  let text_len = document.text.byte_len();
  if document.paragraphs.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 document has no paragraphs"));
  }
  for (ix, paragraph) in document.paragraphs.iter().enumerate() {
    let range = paragraph_byte_range(document, ix);
    if range.start > range.end || range.end > text_len {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "paragraph range is outside document text"));
    }
    if ix == 0 && range.start != 0 {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "first paragraph does not start at byte 0"));
    }
    if paragraph_runs_len(paragraph) != paragraph_text_len(paragraph) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "paragraph run lengths do not match paragraph text",
      ));
    }
    // Verify every run boundary falls on a valid UTF-8 char boundary. A
    // corrupt DB8 could declare correct total run lengths but split a
    // multibyte character mid-codepoint, which would panic when layout
    // later slices the rope at those offsets.
    {
      let p_text = document_text_slice(document, range.clone());
      let mut run_end = 0;
      for run in &paragraph.runs {
        run_end += run.len;
        if run_end < p_text.len() && !p_text.is_char_boundary(run_end) {
          return Err(io::Error::new(io::ErrorKind::InvalidData, "run boundary splits a UTF-8 character"));
        }
      }
    }
    if ix > 0 {
      let previous_range = paragraph_byte_range(document, ix - 1);
      if previous_range.end + 1 != range.start || document.text.byte(previous_range.end) != b'\n' {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "paragraph ranges are not newline separated"));
      }
    }
  }
  if document
    .paragraphs
    .last()
    .is_some_and(|_| paragraph_byte_range(document, document.paragraphs.len() - 1).end != text_len)
  {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "last paragraph does not end at text length"));
  }
  validate_paragraph_block_projection(document)?;
  for block in document.blocks.iter() {
    validate_block_payload(block, document, 0)?;
  }
  Ok(())
}

fn validate_paragraph_block_projection(document: &Document) -> io::Result<()> {
  let paragraph_blocks = document
    .blocks
    .iter()
    .filter_map(|block| match block {
      Block::Paragraph(paragraph) => Some(paragraph),
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => None,
    })
    .collect::<Vec<_>>();
  if paragraph_blocks.len() != document.paragraphs.len() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "paragraph block count does not match paragraph metadata",
    ));
  }
  for (block_paragraph, paragraph) in paragraph_blocks.iter().zip(document.paragraphs.iter()) {
    if *block_paragraph != paragraph {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "paragraph block payload does not match paragraph metadata",
      ));
    }
  }
  Ok(())
}

fn validate_block_payload(block: &Block, document: &Document, table_depth: usize) -> io::Result<()> {
  match block {
    // Missing assets are allowed so a partially damaged document can still
    // open and show a visible missing-image block instead of failing load.
    Block::Image(image) => validate_image_payload(image, document)?,
    Block::Equation(equation) => validate_equation_payload(equation)?,
    Block::Table(table) => validate_table_payload(table, table_depth)?,
    Block::Paragraph(paragraph) => {
      if paragraph_runs_len(paragraph) != paragraph_text_len(paragraph) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "paragraph block run lengths are invalid"));
      }
    },
  }
  Ok(())
}

fn validate_image_payload(image: &ImageBlock, document: &Document) -> io::Result<()> {
  match image.sizing {
    ImageSizing::Fixed { width_px, height_px } => {
      if width_px == 0 || height_px == Some(0) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "fixed image dimensions must be nonzero"));
      }
    },
    ImageSizing::Intrinsic | ImageSizing::FitWidth => {},
  }
  if let Some(caption) = &image.caption {
    if paragraph_runs_len(caption) != paragraph_text_len(caption) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "image caption run lengths are invalid"));
    }
  }
  if let Some(asset) = document.assets.assets.get(&image.asset_id) {
    let mut hasher = DefaultHasher::new();
    asset.bytes.hash(&mut hasher);
    if asset.content_hash != hasher.finish() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "image asset content hash mismatch"));
    }
  }
  Ok(())
}

fn validate_equation_payload(equation: &EquationBlock) -> io::Result<()> {
  if equation.source.len() > 64 * 1024 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "equation source is too large"));
  }
  Ok(())
}

fn validate_table_payload(table: &TableBlock, depth: usize) -> io::Result<()> {
  if depth > 8 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "nested tables are too deep"));
  }
  if table.rows.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table has no rows"));
  }
  let expected_columns = table.column_widths.len().max(1);
  for row in &table.rows {
    if row.cells.is_empty() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "table row has no cells"));
    }
    let mut span_total = 0usize;
    for cell in &row.cells {
      if cell.row_span == 0 || cell.col_span == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell span cannot be zero"));
      }
      span_total = span_total.saturating_add(cell.col_span as usize);
      for block in &cell.blocks {
        match block {
          TableCellBlock::Paragraph(paragraph) => {
            if paragraph.paragraph.byte_range != (0..paragraph.text.len()) {
              return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell paragraph byte range is invalid"));
            }
            if paragraph_runs_len(&paragraph.paragraph) != paragraph.text.len() {
              return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "table cell paragraph run lengths do not match text",
              ));
            }
            let mut run_end = 0;
            for run in &paragraph.paragraph.runs {
              run_end += run.len;
              if run_end < paragraph.text.len() && !paragraph.text.is_char_boundary(run_end) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell run boundary splits UTF-8"));
              }
            }
          },
          TableCellBlock::Table(nested) => validate_table_payload(nested, depth + 1)?,
        }
      }
    }
    if span_total != expected_columns {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "table row shape does not match column count"));
    }
  }
  Ok(())
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
  let mut bytes = [0; 1];
  cursor.read_exact(&mut bytes)?;
  Ok(bytes[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> io::Result<u16> {
  let mut bytes = [0; 2];
  cursor.read_exact(&mut bytes)?;
  Ok(u16::from_le_bytes(bytes))
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

fn read_u128(cursor: &mut Cursor<&[u8]>) -> io::Result<u128> {
  let mut bytes = [0; 16];
  cursor.read_exact(&mut bytes)?;
  Ok(u128::from_le_bytes(bytes))
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u128(bytes: &mut Vec<u8>, value: u128) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_len(cursor: &mut Cursor<&[u8]>, label: &'static str) -> io::Result<usize> {
  let raw = read_u64(cursor)?;
  usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{label} overflows usize")))
}

fn read_string(cursor: &mut Cursor<&[u8]>) -> io::Result<String> {
  let len = read_len(cursor, "DB8 string length")?;
  let mut bytes = vec![0; len];
  cursor.read_exact(&mut bytes)?;
  String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 string is not UTF-8"))
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
  write_u64(bytes, value.len() as u64);
  bytes.extend_from_slice(value.as_bytes());
}

fn encode_block_alignment(alignment: BlockAlignment) -> u8 {
  match alignment {
    BlockAlignment::Left => 0,
    BlockAlignment::Center => 1,
    BlockAlignment::Right => 2,
  }
}

fn decode_block_alignment(value: u8) -> io::Result<BlockAlignment> {
  match value {
    0 => Ok(BlockAlignment::Left),
    1 => Ok(BlockAlignment::Center),
    2 => Ok(BlockAlignment::Right),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid block alignment")),
  }
}

fn encode_paragraph_style(style: ParagraphStyle) -> u8 {
  match style {
    ParagraphStyle::Pocket => 0,
    ParagraphStyle::Hat => 1,
    ParagraphStyle::Block => 2,
    ParagraphStyle::Tag => 3,
    ParagraphStyle::Analytic => 4,
    ParagraphStyle::Normal => 5,
    ParagraphStyle::Undertag => 6,
  }
}

fn decode_paragraph_style(value: u8) -> io::Result<ParagraphStyle> {
  match value {
    0 => Ok(ParagraphStyle::Pocket),
    1 => Ok(ParagraphStyle::Hat),
    2 => Ok(ParagraphStyle::Block),
    3 => Ok(ParagraphStyle::Tag),
    4 => Ok(ParagraphStyle::Analytic),
    5 => Ok(ParagraphStyle::Normal),
    6 => Ok(ParagraphStyle::Undertag),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid paragraph style")),
  }
}

fn write_run_styles(bytes: &mut Vec<u8>, styles: RunStyles) {
  bytes.push(encode_run_semantic_style(styles.semantic));
  let mut flags = 0u8;
  if styles.direct_underline {
    flags |= 1 << 0;
  }
  if styles.strikethrough {
    flags |= 1 << 1;
  }
  bytes.push(flags);
  bytes.push(encode_highlight_style(styles.highlight));
}

fn read_run_styles(cursor: &mut Cursor<&[u8]>) -> io::Result<RunStyles> {
  let semantic = decode_run_semantic_style(read_u8(cursor)?)?;
  let flags = read_u8(cursor)?;
  if flags & !0b0000_0011 != 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid run style flags"));
  }
  Ok(RunStyles {
    semantic,
    direct_underline: flags & (1 << 0) != 0,
    strikethrough: flags & (1 << 1) != 0,
    highlight: decode_highlight_style(read_u8(cursor)?)?,
  })
}

fn encode_run_semantic_style(style: RunSemanticStyle) -> u8 {
  match style {
    RunSemanticStyle::Plain => 0,
    RunSemanticStyle::Cite => 1,
    RunSemanticStyle::Emphasis => 2,
    RunSemanticStyle::Underline => 3,
    RunSemanticStyle::Condensed => 4,
    RunSemanticStyle::Ultracondensed => 5,
  }
}

fn decode_run_semantic_style(value: u8) -> io::Result<RunSemanticStyle> {
  match value {
    0 => Ok(RunSemanticStyle::Plain),
    1 => Ok(RunSemanticStyle::Cite),
    2 => Ok(RunSemanticStyle::Emphasis),
    3 => Ok(RunSemanticStyle::Underline),
    4 => Ok(RunSemanticStyle::Condensed),
    5 => Ok(RunSemanticStyle::Ultracondensed),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid run semantic style")),
  }
}

fn encode_highlight_style(style: Option<HighlightStyle>) -> u8 {
  match style {
    None => 0,
    Some(HighlightStyle::Spoken) => 1,
    Some(HighlightStyle::Insert) => 2,
    Some(HighlightStyle::Alternative) => 3,
  }
}

fn decode_highlight_style(value: u8) -> io::Result<Option<HighlightStyle>> {
  if value > 31 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid highlight style slot"));
  }
  Ok(match value {
    0 => None,
    1 => Some(HighlightStyle::Spoken),
    2 => Some(HighlightStyle::Insert),
    3 => Some(HighlightStyle::Alternative),
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "highlight slot is reserved but has no app style yet",
      ));
    },
  })
}
