pub trait DocumentExportAdapter: Send + Sync + 'static {
  fn send_output_directory(&self, source_path: Option<&Path>, recovery_path: Option<&Path>) -> Option<PathBuf> {
    source_path
      .and_then(Path::parent)
      .or_else(|| recovery_path.and_then(Path::parent))
      .map(Path::to_path_buf)
  }

  fn write_document_export(&self, output_path: &Path, document: &DocumentProjection, format: DocumentExportFormat) -> io::Result<()>;
}

pub trait DocumentRecoveryAdapter: Send + Sync + 'static {
  fn write_recovery_snapshot(&self, recovery_path: &Path, source_path: Option<&Path>, document: &DocumentProjection) -> io::Result<()>;
}

static DOCUMENT_EXPORT_ADAPTER: OnceLock<Arc<dyn DocumentExportAdapter>> = OnceLock::new();
static DOCUMENT_RECOVERY_ADAPTER: OnceLock<Arc<dyn DocumentRecoveryAdapter>> = OnceLock::new();

pub fn set_document_export_adapter(adapter: Arc<dyn DocumentExportAdapter>) -> Result<(), Arc<dyn DocumentExportAdapter>> {
  DOCUMENT_EXPORT_ADAPTER.set(adapter)
}

pub fn set_document_recovery_adapter(adapter: Arc<dyn DocumentRecoveryAdapter>) -> Result<(), Arc<dyn DocumentRecoveryAdapter>> {
  DOCUMENT_RECOVERY_ADAPTER.set(adapter)
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
    if let Some(export_hook) = self.native_export_hook.clone() {
      let pending_runtime_edits = std::mem::take(&mut self.pending_runtime_edits);
      let selection_after = pending_runtime_edits
        .iter()
        .rev()
        .find_map(|edit| edit.selection_after.clone());
      let assets = document.assets.assets.values().cloned().collect();
      return cx.spawn(async move |editor, cx| {
        let result = export_hook(output_path.clone(), format, pending_runtime_edits, assets).await;
        match result {
          Ok(document) => {
            let _ = editor.update(cx, |editor, cx| {
              editor.replace_document_from_collaboration(document, cx);
              editor.complete_runtime_edit(selection_after, cx);
              editor.last_send_document_generation = Some(generation);
              cx.notify();
            });
            Ok(output_path)
          },
          Err(error) => Err(error),
        }
      });
    }
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
    if let Some(export_hook) = self.native_export_hook.clone() {
      let pending_runtime_edits = std::mem::take(&mut self.pending_runtime_edits);
      let selection_after = pending_runtime_edits
        .iter()
        .rev()
        .find_map(|edit| edit.selection_after.clone());
      let assets = document.assets.assets.values().cloned().collect();
      return cx.spawn(async move |editor, cx| {
        let result = export_hook(output_path.clone(), format, pending_runtime_edits, assets).await;
        match result {
          Ok(document) => {
            let _ = editor.update(cx, |editor, cx| {
              editor.replace_document_from_collaboration(document, cx);
              editor.complete_runtime_edit(selection_after, cx);
              editor.last_format_export_generation = Some(generation);
              cx.notify();
            });
            Ok(output_path)
          },
          Err(error) => Err(error),
        }
      });
    }
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
fn write_document_export(output_path: &Path, document: &DocumentProjection, format: DocumentExportFormat) -> io::Result<()> {
  if let Some(adapter) = DOCUMENT_EXPORT_ADAPTER.get() {
    return adapter.write_document_export(output_path, document, format);
  }
  match format {
    DocumentExportFormat::Native => write_document(output_path, document),
    DocumentExportFormat::NativeWithExtension(extension) if extension.eq_ignore_ascii_case(DEFAULT_DOCUMENT_EXTENSION) => {
      write_document(output_path, document)
    },
    DocumentExportFormat::NativeWithExtension(extension) => Err(io::Error::new(
      io::ErrorKind::Unsupported,
      format!("native export .{extension} requires a host-application adapter"),
    )),
    DocumentExportFormat::Docx | DocumentExportFormat::Pdf => Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "DOCX and PDF export are host-application adapters; gpui-flowtext only writes its native binary format directly",
    )),
  }
}

#[hotpath::measure]
fn write_native_document(output_path: &Path, document: &DocumentProjection) -> io::Result<()> {
  if let Some(adapter) = DOCUMENT_EXPORT_ADAPTER.get() {
    return adapter.write_document_export(output_path, document, DocumentExportFormat::Native);
  }
  if path_extension_requires_host_adapter(output_path) {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "native save for a host-native document requires a host-application adapter",
    ));
  }
  write_document(output_path, document)
}

#[hotpath::measure]
fn write_recovery_snapshot(recovery_path: &Path, source_path: Option<&Path>, document: &DocumentProjection) -> io::Result<()> {
  if let Some(adapter) = DOCUMENT_RECOVERY_ADAPTER.get() {
    return adapter.write_recovery_snapshot(recovery_path, source_path, document);
  }
  if recovery_requires_host_adapter(recovery_path, source_path) {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "recovery snapshot for a host-native document requires a host-application adapter",
    ));
  }
  write_document(recovery_path, document)
}

#[hotpath::measure]
fn recovery_requires_host_adapter(recovery_path: &Path, source_path: Option<&Path>) -> bool {
  if source_path.is_some_and(path_extension_requires_host_adapter) {
    return true;
  }
  recovery_source_extension(recovery_path).is_some_and(|extension| extension.eq_ignore_ascii_case("db8"))
    || recovery_path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("db8"))
}

#[hotpath::measure]
fn path_extension_requires_host_adapter(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| !extension.eq_ignore_ascii_case(DEFAULT_DOCUMENT_EXTENSION))
}

#[hotpath::measure]
fn recovery_source_extension(path: &Path) -> Option<&str> {
  let file_name = path.file_name()?.to_str()?;
  let lower = file_name.to_ascii_lowercase();
  if !lower.ends_with(".recovery") {
    return None;
  }
  let source_name = &file_name[..file_name.len() - ".recovery".len()];
  Path::new(source_name).extension()?.to_str()
}

#[cfg(test)]
mod send_export_tests {
  use super::*;

  #[test]
  fn native_extension_without_adapter_does_not_write_db8_as_gptx() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("document.db8");

    let error = write_document_export(&path, &blank_document(), DocumentExportFormat::NativeWithExtension("db8")).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!path.exists());
  }

  #[test]
  fn native_save_without_adapter_does_not_write_db8_as_gptx() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("document.db8");

    let error = write_native_document(&path, &blank_document()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!path.exists());
  }

  #[test]
  fn recovery_for_db8_source_without_adapter_is_unsupported() {
    let error = write_recovery_snapshot(
      Path::new("document.db8.recovery"),
      Some(Path::new("document.db8")),
      &blank_document(),
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
  }

  #[test]
  fn recovery_for_db8_path_without_source_without_adapter_is_unsupported() {
    let error = write_recovery_snapshot(Path::new("collaboration.db8"), None, &blank_document()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
  }
}
