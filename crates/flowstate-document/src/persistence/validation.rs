
#[allow(dead_code)]
#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
fn validate_image_payload(image: &ImageBlock, document: &Document) -> io::Result<()> {
  match image.sizing {
    ImageSizing::Fixed { width_px, height_px } => {
      if width_px == 0 || height_px == Some(0) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "fixed image dimensions must be nonzero"));
      }
    },
    ImageSizing::Intrinsic | ImageSizing::FitWidth => {},
  }
  if let Some(caption) = &image.caption
    && paragraph_runs_len(caption) != paragraph_text_len(caption)
  {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "image caption run lengths are invalid"));
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

#[hotpath::measure]
fn validate_equation_payload(equation: &EquationBlock) -> io::Result<()> {
  if equation.source.len() > 64 * 1024 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "equation source is too large"));
  }
  Ok(())
}

#[hotpath::measure]
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
