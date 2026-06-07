pub trait DocumentExportAdapter: Send + Sync + 'static {
  fn send_output_directory(&self, source_path: Option<&Path>, recovery_path: Option<&Path>) -> Option<PathBuf> {
    source_path
      .and_then(Path::parent)
      .or_else(|| recovery_path.and_then(Path::parent))
      .map(Path::to_path_buf)
  }

  fn write_document_export(&self, output_path: &Path, document: &Document, format: DocumentExportFormat) -> io::Result<()>;
}

static DOCUMENT_EXPORT_ADAPTER: OnceLock<Arc<dyn DocumentExportAdapter>> = OnceLock::new();

pub fn set_document_export_adapter(adapter: Arc<dyn DocumentExportAdapter>) -> Result<(), Arc<dyn DocumentExportAdapter>> {
  DOCUMENT_EXPORT_ADAPTER.set(adapter)
}

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
          editor.last_send_document_generation = Some(generation);
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

  pub fn send_document_created_since_last_saved_edit(&self) -> bool {
    self.last_send_document_generation.is_some()
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
  let output_dir = DOCUMENT_EXPORT_ADAPTER
    .get()
    .and_then(|adapter| adapter.send_output_directory(source_path, recovery_path))
    .or_else(|| {
      source_path
        .and_then(Path::parent)
        .or_else(|| recovery_path.and_then(Path::parent))
        .map(Path::to_path_buf)
    })
    .unwrap_or_else(default_send_directory);
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
  let extension = path
    .extension()
    .and_then(|extension| extension.to_str())
    .unwrap_or(DEFAULT_DOCUMENT_EXTENSION);
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
  Native,
  NativeWithExtension(&'static str),
  Docx,
  Pdf,
}

impl DocumentExportFormat {
  #[hotpath::measure]
  pub fn extension(self) -> &'static str {
    match self {
      DocumentExportFormat::Native => DEFAULT_DOCUMENT_EXTENSION,
      DocumentExportFormat::NativeWithExtension(extension) => extension,
      DocumentExportFormat::Docx => "docx",
      DocumentExportFormat::Pdf => "pdf",
    }
  }
}

#[hotpath::measure]
fn write_document_export(output_path: &Path, document: &Document, format: DocumentExportFormat) -> io::Result<()> {
  if let Some(adapter) = DOCUMENT_EXPORT_ADAPTER.get() {
    return adapter.write_document_export(output_path, document, format);
  }
  match format {
    DocumentExportFormat::Native | DocumentExportFormat::NativeWithExtension(_) => write_document(output_path, document),
    DocumentExportFormat::Docx | DocumentExportFormat::Pdf => Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "DOCX and PDF export are host-application adapters; gpui-flowtext only writes its native binary format directly",
    )),
  }
}
