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

  match fs::read(path) {
    Ok(bytes) => {
      let (document, flow_snapshot) = flowstate_document::read_db8_file_bytes_with_snapshot(&bytes)?;
      Ok(LoadedDocumentForOpen {
        document,
        flow_snapshot,
        path: Some(path.to_path_buf()),
        title: None,
      })
    },
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      let document = crate::workspace::file_management::new_blank_document();
      // Best-effort write: if the path is in a read-only directory we still
      // open the document in memory rather than crashing.
      let _ = flowstate_document::write_db8(path, &document);
      Ok(LoadedDocumentForOpen {
        document,
        flow_snapshot: None,
        path: Some(path.to_path_buf()),
        title: None,
      })
    },
    Err(error) => Err(error),
  }
}
