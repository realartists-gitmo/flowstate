#[hotpath::measure]
fn first_window_range(document: &DocumentProjection, paragraph_count: usize) -> Range<DocumentOffset> {
  let end_paragraph = document
    .paragraphs
    .len()
    .saturating_sub(1)
    .min(paragraph_count.saturating_sub(1));
  DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
    paragraph: end_paragraph,
    byte: paragraph_text_len(&document.paragraphs[end_paragraph]),
  }
}

#[hotpath::measure]
fn top_selection(document: &DocumentProjection) -> Option<EditorSelection> {
  if document.paragraphs.is_empty() {
    return None;
  }
  Some(EditorSelection {
    anchor: DocumentOffset { paragraph: 0, byte: 0 },
    head: first_window_range(document, 3).end,
  })
}

#[hotpath::measure]
fn first_char_range(document: &DocumentProjection, paragraph_ix: usize) -> Option<Range<usize>> {
  let text = paragraph_text(document, paragraph_ix);
  let ch = text.chars().next()?;
  Some(0..ch.len_utf8())
}

#[hotpath::measure]
fn safe_mid_byte(document: &DocumentProjection, paragraph_ix: usize) -> usize {
  let text = paragraph_text(document, paragraph_ix);
  if text.is_empty() {
    return 0;
  }
  let target = text.len() / 2;
  text
    .char_indices()
    .map(|(ix, _)| ix)
    .take_while(|ix| *ix <= target)
    .last()
    .unwrap_or(0)
}

#[hotpath::measure]
fn fingerprint_document(document: &DocumentProjection) -> u64 {
  let mut hasher = std::collections::hash_map::DefaultHasher::new();
  for chunk in document.text.chunks() {
    chunk.hash(&mut hasher);
  }
  document.paragraphs.len().hash(&mut hasher);
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    paragraph.style.hash(&mut hasher);
    paragraph_byte_range(document, paragraph_ix).hash(&mut hasher);
    paragraph.runs.hash(&mut hasher);
  }
  document.blocks.len().hash(&mut hasher);
  for block in document.blocks.iter() {
    hash_block(block, &mut hasher);
  }
  let mut assets = document.assets.assets.values().collect::<Vec<_>>();
  assets.sort_by_key(|asset| asset.id.0);
  for asset in assets {
    asset.id.hash(&mut hasher);
    asset.mime_type.as_ref().hash(&mut hasher);
    asset
      .original_name
      .as_ref()
      .map(|name| name.as_ref())
      .hash(&mut hasher);
    asset.content_hash.hash(&mut hasher);
    asset.bytes.len().hash(&mut hasher);
    asset.bytes.hash(&mut hasher);
  }
  hasher.finish()
}

#[hotpath::measure]
fn hash_block(block: &Block, hasher: &mut impl Hasher) {
  match block {
    Block::Paragraph(paragraph) => {
      0u8.hash(hasher);
      hash_paragraph(paragraph, hasher);
    },
    Block::Image(image) => {
      1u8.hash(hasher);
      image.asset_id.hash(hasher);
      image.alt_text.as_ref().hash(hasher);
      hash_optional_paragraph(image.caption.as_ref(), hasher);
      hash_image_sizing(&image.sizing, hasher);
      hash_block_alignment(image.alignment, hasher);
      image.version.hash(hasher);
    },
    Block::Equation(equation) => {
      2u8.hash(hasher);
      equation.source.as_ref().hash(hasher);
      hash_equation_syntax(equation.syntax, hasher);
      hash_equation_display(equation.display, hasher);
      equation.version.hash(hasher);
    },
    Block::Table(table) => {
      3u8.hash(hasher);
      hash_table(table, hasher);
    },
  }
}

#[hotpath::measure]
fn hash_optional_paragraph(paragraph: Option<&Paragraph>, hasher: &mut impl Hasher) {
  match paragraph {
    Some(paragraph) => {
      true.hash(hasher);
      hash_paragraph(paragraph, hasher);
    },
    None => false.hash(hasher),
  }
}

#[hotpath::measure]
fn hash_paragraph(paragraph: &Paragraph, hasher: &mut impl Hasher) {
  paragraph.style.hash(hasher);
  paragraph.runs.hash(hasher);
  paragraph.version.hash(hasher);
}

#[hotpath::measure]
fn hash_table(table: &TableBlock, hasher: &mut impl Hasher) {
  table.version.hash(hasher);
  table.style.header_row.hash(hasher);
  table.column_widths.len().hash(hasher);
  for width in &table.column_widths {
    match width {
      TableColumnWidth::Auto => 0u8.hash(hasher),
      TableColumnWidth::FixedPx(value) => {
        1u8.hash(hasher);
        value.hash(hasher);
      },
      TableColumnWidth::Fraction(value) => {
        2u8.hash(hasher);
        value.hash(hasher);
      },
    }
  }
  table.rows.len().hash(hasher);
  for row in &table.rows {
    row.cells.len().hash(hasher);
    for cell in &row.cells {
      cell.row_span.hash(hasher);
      cell.col_span.hash(hasher);
      for block in &cell.blocks {
        match block {
          TableCellBlock::Paragraph(paragraph) => {
            0u8.hash(hasher);
            hash_paragraph(&paragraph.paragraph, hasher);
            paragraph.text.hash(hasher);
          },
          TableCellBlock::Table(table) => {
            1u8.hash(hasher);
            hash_table(table, hasher);
          },
        }
      }
    }
  }
}

#[hotpath::measure]
fn hash_image_sizing(sizing: &ImageSizing, hasher: &mut impl Hasher) {
  match sizing {
    ImageSizing::Intrinsic => 0u8.hash(hasher),
    ImageSizing::FitWidth => 1u8.hash(hasher),
    ImageSizing::Fixed { width_px, height_px } => {
      2u8.hash(hasher);
      width_px.hash(hasher);
      height_px.hash(hasher);
    },
  }
}

#[hotpath::measure]
fn hash_block_alignment(alignment: BlockAlignment, hasher: &mut impl Hasher) {
  match alignment {
    BlockAlignment::Left => 0u8.hash(hasher),
    BlockAlignment::Center => 1u8.hash(hasher),
    BlockAlignment::Right => 2u8.hash(hasher),
  }
}

#[hotpath::measure]
fn hash_equation_syntax(syntax: EquationSyntax, hasher: &mut impl Hasher) {
  match syntax {
    EquationSyntax::Latex => 0u8.hash(hasher),
  }
}

#[hotpath::measure]
fn hash_equation_display(display: EquationDisplay, hasher: &mut impl Hasher) {
  match display {
    EquationDisplay::Display => 0u8.hash(hasher),
    EquationDisplay::InlineLikeParagraph => 1u8.hash(hasher),
  }
}

#[hotpath::measure]
fn paragraph_style_names() -> [(ParagraphStyle, &'static str); 7] {
  [
    (ParagraphStyle::Custom(0), "Custom heading"),
    (ParagraphStyle::Custom(1), "Custom subheading"),
    (ParagraphStyle::Custom(2), "Custom block"),
    (ParagraphStyle::Custom(3), "Custom label"),
    (ParagraphStyle::Custom(4), "Custom section"),
    (ParagraphStyle::Normal, "Normal"),
    (ParagraphStyle::Custom(6), "Custom trailing"),
  ]
}

#[hotpath::measure]
fn semantic_style_names() -> [(RunSemanticStyle, &'static str); 6] {
  [
    (RunSemanticStyle::Plain, "Plain"),
    (RunSemanticStyle::Custom(1), "Custom semantic"),
    (RunSemanticStyle::Custom(2), "Emphasis"),
    (RunSemanticStyle::Custom(3), "Underline"),
    (RunSemanticStyle::Custom(4), "Condensed"),
    (RunSemanticStyle::Custom(5), "Ultracondensed"),
  ]
}

#[hotpath::measure]
fn highlight_style_names() -> [(Option<HighlightStyle>, &'static str); 4] {
  [
    (None, "None"),
    (Some(HighlightStyle::Custom(1)), "Custom highlight"),
    (Some(HighlightStyle::Custom(2)), "Insert"),
    (Some(HighlightStyle::Custom(3)), "Alternative"),
  ]
}

#[hotpath::measure]
fn div_duration(duration: Duration, divisor: u32) -> Duration {
  if divisor == 0 {
    Duration::default()
  } else {
    Duration::from_secs_f64(duration.as_secs_f64() / divisor as f64)
  }
}

#[hotpath::measure]
fn ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1000.0
}

#[hotpath::measure]
fn px_to_f32(pixels: Pixels) -> f32 {
  let value: f32 = pixels.into();
  value
}

#[hotpath::measure]
fn md(value: &str) -> String {
  value.replace('|', "\\|").replace('\n', " ")
}

#[hotpath::measure]
fn build_profile() -> &'static str {
  if cfg!(debug_assertions) { "debug" } else { "release" }
}
