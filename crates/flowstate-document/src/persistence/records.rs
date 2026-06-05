#[derive(Clone, Copy)]
struct Db8Chunk {
  kind: u8,
  offset: usize,
  len: usize,
}

#[hotpath::measure]
fn read_db8_vnext(mut cursor: Cursor<&[u8]>, timing: Instant) -> io::Result<Document> {
  let chunk_count = read_u32(&mut cursor)? as usize;
  let mut chunks = Vec::with_capacity(chunk_count.min(32));
  for _ in 0..chunk_count {
    let kind = read_u8(&mut cursor)?;
    let flags = read_u8(&mut cursor)?;
    let _reserved = read_u16(&mut cursor)?;
    if flags != 0 {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported DB8 chunk flags"));
    }
    let offset = read_len(&mut cursor, "DB8 chunk offset")?;
    let len = read_len(&mut cursor, "DB8 chunk length")?;
    chunks.push(Db8Chunk { kind, offset, len });
  }

  let text_bytes = required_chunk(cursor.get_ref(), &chunks, CHUNK_TEXT, "DB8 text chunk")?;
  let text = std::str::from_utf8(text_bytes)
    .map(std::borrow::ToOwned::to_owned)
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 text chunk is not UTF-8"))?;
  let assets = read_assets_chunk(required_chunk(cursor.get_ref(), &chunks, CHUNK_ASSETS, "DB8 assets chunk")?)?;
  let (blocks, paragraphs) = read_blocks_chunk(required_chunk(cursor.get_ref(), &chunks, CHUNK_BLOCKS, "DB8 blocks chunk")?, &text)?;
  let paragraph_ids = read_paragraph_ids_chunk(required_chunk(cursor.get_ref(), &chunks, CHUNK_PARAGRAPH_IDS, "DB8 paragraph IDs chunk")?)?;
  let block_ids = read_block_ids_chunk(required_chunk(cursor.get_ref(), &chunks, CHUNK_BLOCK_IDS, "DB8 block IDs chunk")?)?;
  let sections = read_sections_chunk(required_chunk(cursor.get_ref(), &chunks, CHUNK_SECTIONS, "DB8 sections chunk")?)?;
  let document_id = read_document_meta_chunk(required_chunk(
    cursor.get_ref(),
    &chunks,
    CHUNK_DOCUMENT_META,
    "DB8 document metadata chunk",
  )?)?;

  let offset_index = ParagraphOffsetIndex::new(&paragraphs);
  let mut document = Document {
    text: Rope::from(text),
    paragraphs: Arc::new(paragraphs),
    blocks: Arc::new(blocks),
    assets,
    ids: DocumentIds {
      document_id,
      paragraph_ids,
      block_ids,
    },
    sections: Arc::new(sections),
    offset_index,
    theme: DocumentTheme::default(),
  };
  reconcile_document_ids(&mut document);
  validate_or_rebuild_sections(&mut document);
  validate_document(&document)?;
  log_timing_lazy("db8 vnext read", timing, || {
    format!(
      "bytes={} blocks={} paragraphs={} sections={}",
      document.text.byte_len(),
      document.blocks.len(),
      document.paragraphs.len(),
      document.sections.len()
    )
  });
  Ok(document)
}

#[hotpath::measure]
fn required_chunk<'bytes>(bytes: &'bytes [u8], chunks: &[Db8Chunk], kind: u8, label: &'static str) -> io::Result<&'bytes [u8]> {
  let chunk = chunks
    .iter()
    .find(|chunk| chunk.kind == kind)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{label} is missing")))?;
  let end = chunk
    .offset
    .checked_add(chunk.len)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{label} range overflows")))?;
  if end > bytes.len() {
    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{label} is truncated")));
  }
  Ok(&bytes[chunk.offset..end])
}

#[hotpath::measure]
fn read_assets_chunk(bytes: &[u8]) -> io::Result<AssetStore> {
  let mut cursor = Cursor::new(bytes);
  let asset_count = read_len(&mut cursor, "DB8 asset count")?;
  let mut assets = AssetStore::default();
  assets.assets.reserve(asset_count);
  for _ in 0..asset_count {
    let asset = read_asset_record(&mut cursor)?;
    assets.assets.insert(asset.id, asset);
  }
  Ok(assets)
}

#[hotpath::measure]
fn read_blocks_chunk(bytes: &[u8], text: &str) -> io::Result<(Vec<Block>, Vec<Paragraph>)> {
  let mut cursor = Cursor::new(bytes);
  let block_count = read_len(&mut cursor, "DB8 block count")?;
  let mut blocks = Vec::with_capacity(block_count.min(4096));
  let mut paragraphs = Vec::new();
  for _ in 0..block_count {
    let mut block = read_block_record(&mut cursor)?;
    normalize_block_text_runs(&mut block, text)?;
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
  Ok((blocks, paragraphs))
}

#[hotpath::measure]
fn read_paragraph_ids_chunk(bytes: &[u8]) -> io::Result<Vec<ParagraphId>> {
  let mut cursor = Cursor::new(bytes);
  let count = read_len(&mut cursor, "DB8 paragraph ID count")?;
  let mut ids = Vec::with_capacity(count);
  for _ in 0..count {
    ids.push(ParagraphId(read_u128(&mut cursor)?));
  }
  Ok(ids)
}

#[hotpath::measure]
fn read_block_ids_chunk(bytes: &[u8]) -> io::Result<Vec<BlockId>> {
  let mut cursor = Cursor::new(bytes);
  let count = read_len(&mut cursor, "DB8 block ID count")?;
  let mut ids = Vec::with_capacity(count);
  for _ in 0..count {
    ids.push(BlockId(read_u128(&mut cursor)?));
  }
  Ok(ids)
}

#[hotpath::measure]
fn read_sections_chunk(bytes: &[u8]) -> io::Result<Vec<DocumentSection>> {
  let mut cursor = Cursor::new(bytes);
  let count = read_len(&mut cursor, "DB8 section count")?;
  let mut sections = Vec::with_capacity(count);
  for _ in 0..count {
    sections.push(read_section_record(&mut cursor)?);
  }
  Ok(sections)
}

#[hotpath::measure]
fn read_document_meta_chunk(bytes: &[u8]) -> io::Result<u128> {
  let mut cursor = Cursor::new(bytes);
  let document_id = read_u128(&mut cursor)?;
  if document_id == 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 document ID is empty"));
  }
  Ok(document_id)
}

#[hotpath::measure]
fn validate_or_rebuild_sections(document: &mut Document) {
  if document.sections.is_empty()
    || document
      .sections
      .iter()
      .any(|section| paragraph_index_for_id(document, section.start_paragraph).is_none())
  {
    rebuild_document_sections(document);
  }
}

#[hotpath::measure]
fn normalize_block_text_runs(block: &mut Block, document_text: &str) -> io::Result<()> {
  match block {
    Block::Paragraph(paragraph) => normalize_paragraph_text_runs(paragraph, document_text),
    Block::Image(_) | Block::Equation(_) => Ok(()),
    Block::Table(table) => normalize_table_text_runs(table),
  }
}

#[hotpath::measure]
fn normalize_table_text_runs(table: &mut TableBlock) -> io::Result<()> {
  for row in &mut table.rows {
    for cell in &mut row.cells {
      for block in &mut cell.blocks {
        match block {
          TableCellBlock::Paragraph(paragraph) => normalize_runs_for_text(&mut paragraph.paragraph.runs, &paragraph.text)?,
          TableCellBlock::Table(table) => normalize_table_text_runs(table)?,
        }
      }
    }
  }
  Ok(())
}

#[hotpath::measure]
fn normalize_paragraph_text_runs(paragraph: &mut Paragraph, document_text: &str) -> io::Result<()> {
  if paragraph.byte_range.start > paragraph.byte_range.end
    || paragraph.byte_range.end > document_text.len()
    || !document_text.is_char_boundary(paragraph.byte_range.start)
    || !document_text.is_char_boundary(paragraph.byte_range.end)
  {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "paragraph byte range is invalid"));
  }
  normalize_runs_for_text(&mut paragraph.runs, &document_text[paragraph.byte_range.clone()])
}

#[hotpath::measure]
fn normalize_runs_for_text(runs: &mut Vec<TextRun>, text: &str) -> io::Result<()> {
  let run_len = runs.iter().map(|run| run.len).sum::<usize>();
  if run_len == text.len() && run_boundaries_are_char_boundaries(runs, text) {
    return Ok(());
  }

  if run_len == text.chars().count() {
    let byte_offsets = char_count_boundaries_to_byte_offsets(runs, text)?;
    for (run, range) in runs.iter_mut().zip(byte_offsets.windows(2)) {
      run.len = range[1] - range[0];
    }
    *runs = merge_adjacent_runs(std::mem::take(runs));
    return Ok(());
  }

  if runs.len() == 1 {
    runs[0].len = text.len();
    return Ok(());
  }

  Err(io::Error::new(
    io::ErrorKind::InvalidData,
    "paragraph run lengths do not match text bytes or characters",
  ))
}

#[hotpath::measure]
fn run_boundaries_are_char_boundaries(runs: &[TextRun], text: &str) -> bool {
  let mut byte = 0_usize;
  for run in runs {
    byte = byte.saturating_add(run.len);
    if byte < text.len() && !text.is_char_boundary(byte) {
      return false;
    }
  }
  byte == text.len()
}

#[hotpath::measure]
fn char_count_boundaries_to_byte_offsets(runs: &[TextRun], text: &str) -> io::Result<Vec<usize>> {
  let mut offsets = Vec::with_capacity(runs.len() + 1);
  offsets.push(0);
  let mut char_count = 0_usize;
  for run in runs {
    char_count = char_count.saturating_add(run.len);
    if char_count == text.chars().count() {
      offsets.push(text.len());
      continue;
    }
    let Some((byte, _)) = text.char_indices().nth(char_count) else {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "run character offset is outside paragraph text",
      ));
    };
    offsets.push(byte);
  }
  Ok(offsets)
}

#[hotpath::measure]
fn read_block_record(cursor: &mut Cursor<&[u8]>) -> io::Result<Block> {
  let kind = read_u8(cursor)?;
  let payload_len = {
    let raw = read_u64(cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 block payload length overflows usize"))?
  };
  let payload = read_bytes(cursor, payload_len, "DB8 block payload")?;
  let mut payload = Cursor::new(payload);
  match kind {
    BLOCK_PARAGRAPH => read_paragraph_payload(&mut payload).map(Block::Paragraph),
    BLOCK_IMAGE => read_image_payload(&mut payload).map(Block::Image),
    BLOCK_EQUATION => read_equation_payload(&mut payload).map(Block::Equation),
    BLOCK_TABLE => read_table_payload(&mut payload).map(Block::Table),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 block kind")),
  }
}

#[hotpath::measure]
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

#[hotpath::measure]
fn write_section_record(bytes: &mut Vec<u8>, section: &DocumentSection) {
  write_u128(bytes, section.id.0);
  write_u128(bytes, section.parent_id.map_or(0, |id| id.0));
  bytes.push(encode_section_kind(section.kind));
  bytes.push(u8::from(section.heading_paragraph.is_some()));
  bytes.push(u8::from(section.end_paragraph_exclusive.is_some()));
  bytes.push(0);
  write_u128(bytes, section.heading_paragraph.map_or(0, |id| id.0));
  write_u128(bytes, section.start_paragraph.0);
  write_u128(bytes, section.end_paragraph_exclusive.map_or(0, |id| id.0));
}

#[hotpath::measure]
fn read_section_record(cursor: &mut Cursor<&[u8]>) -> io::Result<DocumentSection> {
  let id = SectionId(read_u128(cursor)?);
  let parent = read_u128(cursor)?;
  let kind = decode_section_kind(read_u8(cursor)?)?;
  let has_heading = read_u8(cursor)? != 0;
  let has_end = read_u8(cursor)? != 0;
  let reserved = read_u8(cursor)?;
  if reserved != 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 section reserved byte"));
  }
  let heading = read_u128(cursor)?;
  let start = read_u128(cursor)?;
  let end = read_u128(cursor)?;
  Ok(DocumentSection {
    id,
    parent_id: (parent != 0).then_some(SectionId(parent)),
    kind,
    heading_paragraph: has_heading.then_some(ParagraphId(heading)),
    start_paragraph: ParagraphId(start),
    end_paragraph_exclusive: has_end.then_some(ParagraphId(end)),
  })
}

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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
  let bytes = read_bytes(cursor, byte_len, "DB8 asset bytes")?.to_vec();
  Ok(AssetRecord {
    id,
    mime_type,
    original_name,
    content_hash,
    bytes: Arc::new(bytes),
  })
}

#[hotpath::measure]
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

#[hotpath::measure]
#[expect(dead_code, reason = "kept as public recovery API for external callers and tests")]
#[must_use]
pub fn recovery_path_for_document(path: &Path) -> PathBuf {
  let mut recovery_path = path.to_path_buf();
  let file_name = path
    .file_name()
    .and_then(|name| name.to_str())
    .map_or_else(|| "untitled.db8.recovery".to_owned(), |name| format!("{name}.recovery"));
  recovery_path.set_file_name(file_name);
  recovery_path
}

#[cfg(test)]
#[allow(
  clippy::items_after_test_module,
  reason = "persistence files are include!-assembled; these local tests sit beside the codec helpers they cover."
)]
mod records_tests {
  use super::*;

  #[test]
  fn normalizes_legacy_character_count_run_lengths() {
    let mut runs = vec![TextRun {
      len: "Kepe et al. ‘23".chars().count(),
      styles: RunStyles::default(),
    }];

    normalize_runs_for_text(&mut runs, "Kepe et al. ‘23").expect("normalize runs");

    assert_eq!(runs[0].len, "Kepe et al. ‘23".len());
  }

  #[test]
  fn normalizes_legacy_character_count_run_boundaries() {
    let cite = RunStyles {
      semantic: SEMANTIC_CITE,
      ..RunStyles::default()
    };
    let mut runs = vec![
      TextRun {
        len: "Kepe et al. ".chars().count(),
        styles: RunStyles::default(),
      },
      TextRun {
        len: "‘23".chars().count(),
        styles: cite,
      },
    ];

    normalize_runs_for_text(&mut runs, "Kepe et al. ‘23").expect("normalize runs");

    assert_eq!(runs[0].len, "Kepe et al. ".len());
    assert_eq!(runs[1].len, "‘23".len());
  }
}
