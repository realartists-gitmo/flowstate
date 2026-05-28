
#[hotpath::measure]
fn read_db8_current(mut cursor: Cursor<&[u8]>, timing: Instant) -> io::Result<Document> {
  let text_len = {
    let raw = read_u64(&mut cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 text length overflows usize"))?
  };
  let text_bytes = read_bytes(&mut cursor, text_len, "DB8 text")?;
  let text = std::str::from_utf8(text_bytes)
    .map(|text| text.to_owned())
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 text is not UTF-8"))?;

  let asset_count = {
    let raw = read_u64(&mut cursor)?;
    usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 asset count overflows usize"))?
  };
  let mut assets = AssetStore::default();
  assets.assets.reserve(asset_count);
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
  log_timing_lazy("db8 read", timing, || {
    format!(
      "bytes={} blocks={} paragraphs={}",
      document.text.byte_len(),
      document.blocks.len(),
      document.paragraphs.len()
    )
  });
  Ok(document)
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
pub fn recovery_path_for_document(path: &Path) -> PathBuf {
  let mut recovery_path = path.to_path_buf();
  let file_name = path
    .file_name()
    .and_then(|name| name.to_str())
    .map(|name| format!("{name}.recovery"))
    .unwrap_or_else(|| "untitled.db8.recovery".to_string());
  recovery_path.set_file_name(file_name);
  recovery_path
}
