struct LoadedDocumentForOpen {
  document: Document,
  flow_snapshot: Option<Vec<u8>>,
  path: Option<PathBuf>,
  title: Option<String>,
}

#[hotpath::measure]
fn load_document_for_open(path: &PathBuf) -> std::io::Result<LoadedDocumentForOpen> {
  if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
    if extension.eq_ignore_ascii_case("docx") {
      let (document, _) = convert_docx_to_document(path)?;
      return Ok(LoadedDocumentForOpen {
        document,
        flow_snapshot: None,
        path: None,
        title: path
          .with_extension("db8")
          .file_name()
          .map(|name| name.to_string_lossy().to_string()),
      });
    }
    if extension.eq_ignore_ascii_case("pdf") {
      let document = if let Some(db8_bytes) = crate::docx_conversion::extract_db8_bytes_from_pdf(path)? {
        flowstate_document::read_db8_bytes(&db8_bytes)?
      } else {
        convert_pdf_to_document(path)?.0
      };
      return Ok(LoadedDocumentForOpen {
        document,
        flow_snapshot: None,
        path: None,
        title: path
          .with_extension("db8")
          .file_name()
          .map(|name| name.to_string_lossy().to_string()),
      });
    }
  }

  load_or_create_document(path).map(|document| {
    let flow_snapshot = fs::read(path)
      .ok()
      .and_then(|bytes| flowstate_document::db8_flow_snapshot_from_bytes(&bytes).ok().flatten());
    LoadedDocumentForOpen {
      document,
      flow_snapshot,
      path: Some(path.clone()),
      title: None,
    }
  })
}
