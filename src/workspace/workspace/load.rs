struct LoadedDocumentForOpen {
  document: Document,
  path: Option<PathBuf>,
  title: Option<String>,
}

fn load_document_for_open(path: &PathBuf) -> std::io::Result<LoadedDocumentForOpen> {
  if path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("docx"))
  {
    let (document, _) = convert_docx_to_document(path)?;
    return Ok(LoadedDocumentForOpen {
      document,
      path: None,
      title: path.file_name().map(|name| name.to_string_lossy().to_string()),
    });
  }

  load_or_create_document(path).map(|document| LoadedDocumentForOpen {
    document,
    path: Some(path.clone()),
    title: None,
  })
}

