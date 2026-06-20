struct LoadedDocumentForOpen {
  document: DocumentProjection,
  runtime: flowstate_collab::crdt_runtime::CrdtRuntime,
  path: Option<PathBuf>,
  title: Option<String>,
}

#[hotpath::measure]
fn load_document_for_open(path: &PathBuf) -> std::io::Result<LoadedDocumentForOpen> {
  if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
    if extension.eq_ignore_ascii_case("docx") {
      let title = path
        .with_extension("db8")
        .file_name()
        .map(|name| name.to_string_lossy().to_string());
      let (imported, _) = import_docx_to_loro(path, title.as_deref().unwrap_or("Imported Document"))?;
      let runtime = flowstate_collab::crdt_runtime::CrdtRuntime::from_imported_document(imported).map_err(runtime_io_error)?;
      // Runtime startup records replica metadata and therefore advances the
      // canonical frontier. The editor must start from that exact frontier or
      // its first command is correctly rejected as stale.
      let document = runtime.projection_snapshot().map_err(runtime_io_error)?;
      return Ok(LoadedDocumentForOpen {
        document,
        runtime,
        path: None,
        title,
      });
    }
    if extension.eq_ignore_ascii_case("pdf") {
      let db8_bytes = crate::docx_conversion::extract_db8_bytes_from_pdf(path)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "PDF does not contain embedded Flowstate DB8 data"))?;
      let package = flowstate_document::DocumentPackage::from_bytes(&db8_bytes)?;
      let runtime = flowstate_collab::crdt_runtime::CrdtRuntime::from_package(package, None).map_err(runtime_io_error)?;
      let document = runtime.projection_snapshot().map_err(runtime_io_error)?;
      return Ok(LoadedDocumentForOpen {
        document,
        runtime,
        path: None,
        title: path
          .with_extension("db8")
          .file_name()
          .map(|name| name.to_string_lossy().to_string()),
      });
    }
  }

  let (document, runtime) = match flowstate_collab::crdt_runtime::CrdtRuntime::open_package(path) {
    Ok(runtime) => {
      let document = runtime.projection_snapshot().map_err(runtime_io_error)?;
      (document, runtime)
    },
    Err(error) if runtime_root_io_kind(&error) == Some(std::io::ErrorKind::NotFound) => {
      let imported = flowstate_document::import_document_projection(new_blank_document(), "Flowstate Document")?;
      let runtime = flowstate_collab::crdt_runtime::CrdtRuntime::from_imported_document(imported).map_err(runtime_io_error)?;
      let document = runtime.projection_snapshot().map_err(runtime_io_error)?;
      (document, runtime)
    },
    Err(error) => return Err(runtime_io_error(error)),
  };
  Ok(LoadedDocumentForOpen {
    document,
    runtime,
    path: Some(path.clone()),
    title: None,
  })
}

fn runtime_io_error(error: anyhow::Error) -> std::io::Error {
  if let Some(io_error) = error.root_cause().downcast_ref::<std::io::Error>() {
    return std::io::Error::new(io_error.kind(), error.to_string());
  }
  std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

fn runtime_root_io_kind(error: &anyhow::Error) -> Option<std::io::ErrorKind> {
  error.root_cause().downcast_ref::<std::io::Error>().map(std::io::Error::kind)
}

#[hotpath::measure]
fn load_document_preview(path: &Path) -> std::io::Result<DocumentProjection> {
  if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
    if extension.eq_ignore_ascii_case("docx") {
      return convert_docx_to_document(path).map(|(document, _)| document);
    }
    if extension.eq_ignore_ascii_case("pdf") {
      let db8_bytes = crate::docx_conversion::extract_db8_bytes_from_pdf(path)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "PDF does not contain embedded Flowstate DB8 data"))?;
      return flowstate_document::read_db8_bytes(&db8_bytes);
    }
  }
  flowstate_document::read_db8(path)
}
