#[hotpath::measure_all]
impl RichTextEditor {
  pub fn send_document(&mut self, format: DocumentExportFormat, cx: &mut Context<Self>) -> Task<io::Result<PathBuf>> {
    if self.disposed {
      return cx
        .background_executor()
        .spawn(async { Err(io::Error::new(io::ErrorKind::NotFound, "editor is closed")) });
    }
    let output_path = match send_output_path(self.document_path.as_deref(), self.recovery_path.as_deref(), self.document_display_name.as_ref(), format) {
      Ok(path) => path,
      Err(error) => return cx.background_executor().spawn(async move { Err(error) }),
    };
    let generation = self.edit_generation;
    let document = send_document_without_analytic_styles(&self.document);
    cx.spawn(async move |editor, cx| {
      let result = cx
        .background_executor()
        .spawn(async move {
          write_document_export(&output_path, &document, format)?;
          Ok(output_path)
        })
        .await;
      if result.is_ok() {
        let _ = editor.update(cx, |editor, cx| {
          editor.last_send_db8_generation = Some(generation);
          cx.notify();
        });
      }
      result
    })
  }

  pub fn export_document_format(&mut self, format: DocumentExportFormat, cx: &mut Context<Self>) -> Task<io::Result<PathBuf>> {
    if self.disposed {
      return cx
        .background_executor()
        .spawn(async { Err(io::Error::new(io::ErrorKind::NotFound, "editor is closed")) });
    }
    let output_path = match format_output_path(self.document_path.as_deref(), self.recovery_path.as_deref(), self.document_display_name.as_ref(), format) {
      Ok(path) => path,
      Err(error) => return cx.background_executor().spawn(async move { Err(error) }),
    };
    let generation = self.edit_generation;
    let document = self.document.clone();
    cx.spawn(async move |editor, cx| {
      let result = cx
        .background_executor()
        .spawn(async move {
          write_document_export(&output_path, &document, format)?;
          Ok(output_path)
        })
        .await;
      if result.is_ok() {
        let _ = editor.update(cx, |editor, cx| {
          editor.last_format_export_generation = Some(generation);
          cx.notify();
        });
      }
      result
    })
  }

  pub fn send_db8_created_since_last_saved_edit(&self) -> bool {
    self.last_send_db8_generation.is_some()
  }

  pub fn format_export_created_since_last_saved_edit(&self) -> bool {
    self.last_format_export_generation.is_some()
  }
}

#[hotpath::measure]
fn send_output_path(
  source_path: Option<&Path>,
  recovery_path: Option<&Path>,
  display_name: Option<&SharedString>,
  format: DocumentExportFormat,
) -> io::Result<PathBuf> {
  let output_dir = if crate::app_settings::load_send_to_document_directory() {
    source_path
      .and_then(Path::parent)
      .or_else(|| recovery_path.and_then(Path::parent))
      .map(Path::to_path_buf)
      .unwrap_or_else(default_send_directory)
  } else {
    crate::app_settings::load_send_custom_directory().unwrap_or_else(default_send_directory)
  };
  let stem = document_export_stem(source_path, recovery_path, display_name);
  unique_sibling_path(output_dir.join(format!("SEND_{stem}.{}", format.extension())))
}

#[hotpath::measure]
fn format_output_path(
  source_path: Option<&Path>,
  recovery_path: Option<&Path>,
  display_name: Option<&SharedString>,
  format: DocumentExportFormat,
) -> io::Result<PathBuf> {
  let output_dir = source_path
    .and_then(Path::parent)
    .or_else(|| recovery_path.and_then(Path::parent))
    .map(Path::to_path_buf)
    .unwrap_or_else(default_send_directory);
  let stem = document_export_stem(source_path, recovery_path, display_name);
  unique_sibling_path(output_dir.join(format!("{stem}.{}", format.extension())))
}

#[hotpath::measure]
fn document_export_stem(source_path: Option<&Path>, recovery_path: Option<&Path>, display_name: Option<&SharedString>) -> String {
  display_name
    .map(|name| name.as_ref())
    .and_then(stem_from_name)
    .or_else(|| source_path.and_then(path_stem))
    .or_else(|| recovery_path.and_then(path_stem))
    .unwrap_or_else(|| "Untitled".to_string())
}

#[hotpath::measure]
fn path_stem(path: &Path) -> Option<String> {
  path.file_stem().and_then(|name| name.to_str()).and_then(stem_from_name)
}

#[hotpath::measure]
fn stem_from_name(name: &str) -> Option<String> {
  let name = name.trim().trim_start_matches('*').trim().strip_suffix(" *").unwrap_or(name.trim());
  let stem = Path::new(name)
    .file_stem()
    .and_then(|stem| stem.to_str())
    .unwrap_or(name)
    .trim();
  (!stem.is_empty()).then(|| stem.to_string())
}

#[hotpath::measure]
fn unique_sibling_path(path: PathBuf) -> io::Result<PathBuf> {
  if !path.exists() {
    return Ok(path);
  }
  let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
  let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("Untitled");
  let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or("db8");
  for index in 1.. {
    let candidate = parent.join(format!("{stem}_{index}.{extension}"));
    if !candidate.exists() {
      return Ok(candidate);
    }
  }
  unreachable!("unbounded unique path search should return")
}

#[hotpath::measure]
fn default_send_directory() -> PathBuf {
  std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentExportFormat {
  Db8,
  Docx,
  Pdf,
}

impl DocumentExportFormat {
  #[hotpath::measure]
  pub fn extension(self) -> &'static str {
    match self {
      DocumentExportFormat::Db8 => "db8",
      DocumentExportFormat::Docx => "docx",
      DocumentExportFormat::Pdf => "pdf",
    }
  }
}

#[hotpath::measure]
fn write_document_export(output_path: &Path, document: &Document, format: DocumentExportFormat) -> io::Result<()> {
  match format {
    DocumentExportFormat::Db8 => write_db8(output_path, document),
    DocumentExportFormat::Docx => crate::docx_conversion::write_docx(output_path, document),
    DocumentExportFormat::Pdf => crate::docx_conversion::write_pdf(output_path, document),
  }
}

#[hotpath::measure]
fn send_document_without_analytic_styles(document: &Document) -> Document {
  let mut blocks = Vec::with_capacity(document.blocks.len());
  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(paragraph) if paragraph.style == ParagraphStyle::Analytic => {},
      Block::Paragraph(paragraph) => blocks.push(Block::Paragraph(paragraph.clone())),
      Block::Table(table) => blocks.push(Block::Table(send_table_without_analytic_styles(table))),
      Block::Image(_) | Block::Equation(_) => blocks.push(block.clone()),
    }
  }

  let mut text = String::new();
  let mut paragraphs = Vec::new();
  let mut byte = 0usize;
  for block in &mut blocks {
    let Block::Paragraph(paragraph) = block else {
      continue;
    };
    if !paragraphs.is_empty() {
      text.push('\n');
      byte += 1;
    }
    let paragraph_text = document
      .text
      .byte_slice(paragraph.byte_range.clone())
      .to_string();
    let start = byte;
    text.push_str(&paragraph_text);
    byte += paragraph_text.len();
    paragraph.byte_range = start..byte;
    paragraphs.push(paragraph.clone());
  }
  if paragraphs.is_empty() {
    let paragraph = Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    };
    blocks.push(Block::Paragraph(paragraph.clone()));
    paragraphs.push(paragraph);
  }

  let block_count = blocks.len();
  let mut filtered_document = Document {
    text: Rope::from(text),
    paragraphs: Arc::new(paragraphs.clone()),
    blocks: Arc::new(blocks),
    assets: document.assets.clone(),
    ids: document_ids_for_shape(paragraphs.len(), block_count),
    sections: Arc::new(Vec::new()),
    offset_index: ParagraphOffsetIndex::new(&paragraphs),
    theme: document.theme.clone(),
  };
  rebuild_document_sections(&mut filtered_document);
  filtered_document
}

#[hotpath::measure]
fn send_table_without_analytic_styles(table: &TableBlock) -> TableBlock {
  let mut table = table.clone();
  for row in &mut table.rows {
    for cell in &mut row.cells {
      cell.blocks = send_table_cell_blocks_without_analytic_styles(std::mem::take(&mut cell.blocks));
    }
  }
  table
}

#[hotpath::measure]
fn send_table_cell_blocks_without_analytic_styles(blocks: Vec<TableCellBlock>) -> Vec<TableCellBlock> {
  blocks
    .into_iter()
    .filter_map(|block| match block {
      TableCellBlock::Paragraph(paragraph) if paragraph.paragraph.style == ParagraphStyle::Analytic => None,
      TableCellBlock::Paragraph(paragraph) => Some(TableCellBlock::Paragraph(paragraph)),
      TableCellBlock::Table(table) => Some(TableCellBlock::Table(send_table_without_analytic_styles(&table))),
    })
    .collect()
}
