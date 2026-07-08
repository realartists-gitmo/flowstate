struct LoadedDocumentForOpen {
  document: DocumentProjection,
  runtime: flowstate_collab::crdt_runtime::CrdtRuntime,
  path: Option<PathBuf>,
  title: Option<String>,
}

/// §P3 preview→open bridge: the opening screen's docx preview runs the ENTIRE
/// clean/interpret/structured pipeline to materialize a projection, and the
/// subsequent open re-ran the identical pipeline from scratch (~2.4s on the
/// reference doc) before the Loro import. Both produce the same
/// `DocumentProjection` (`convert_docx_to_document` and
/// `import_cleaned_docx_to_loro` share `build_structured_document`), so the
/// preview's result feeds the open directly when the file is unchanged
/// (mtime + length fingerprint). Small LRU: the open screen previews several
/// recents; entries are dropped once used or displaced.
static DOCX_PREVIEW_BRIDGE: std::sync::Mutex<Vec<(PathBuf, std::time::SystemTime, u64, DocumentProjection)>> =
  std::sync::Mutex::new(Vec::new());
const DOCX_PREVIEW_BRIDGE_CAP: usize = 4;

fn docx_fingerprint(path: &Path) -> Option<(std::time::SystemTime, u64)> {
  let metadata = std::fs::metadata(path).ok()?;
  Some((metadata.modified().ok()?, metadata.len()))
}

fn store_docx_preview_bridge(path: &Path, document: &DocumentProjection) {
  let Some((modified, len)) = docx_fingerprint(path) else {
    return;
  };
  let Ok(mut bridge) = DOCX_PREVIEW_BRIDGE.lock() else {
    return;
  };
  bridge.retain(|(existing, ..)| existing != path);
  bridge.push((path.to_path_buf(), modified, len, document.clone()));
  if bridge.len() > DOCX_PREVIEW_BRIDGE_CAP {
    bridge.remove(0);
  }
}

fn take_docx_preview_bridge(path: &Path) -> Option<DocumentProjection> {
  let (modified, len) = docx_fingerprint(path)?;
  let mut bridge = DOCX_PREVIEW_BRIDGE.lock().ok()?;
  let ix = bridge
    .iter()
    .position(|(existing, existing_modified, existing_len, _)| existing == path && *existing_modified == modified && *existing_len == len)?;
  Some(bridge.remove(ix).3)
}

#[hotpath::measure]
fn load_document_for_open(path: &PathBuf) -> std::io::Result<LoadedDocumentForOpen> {
  if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
    if extension.eq_ignore_ascii_case("docx") {
      let title = path
        .with_extension("db8")
        .file_name()
        .map(|name| name.to_string_lossy().to_string());
      let imported = match take_docx_preview_bridge(path) {
        // The preview already ran the whole docx pipeline on this exact file
        // content; only the Loro import remains.
        Some(document) => flowstate_document::import_document_projection(document, title.as_deref().unwrap_or("Imported Document"))?,
        None => import_docx_to_loro(path, title.as_deref().unwrap_or("Imported Document"))?.0,
      };
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

/// §act-three C (phase V): the cheapest renderable projection for `path`,
/// WITHOUT constructing the authority runtime — the frontier-current `.db8`
/// projection cache (~0.3s, no snapshot/segment decode or Loro import) or a
/// still-warm docx preview-bridge projection. Returns `None` when no fast
/// projection exists (the caller then just waits for the full phase-G load).
/// Read-only: this projection paints immediately; editing enables when the
/// authority attaches (phase G). Never fabricates or mutates state.
pub(crate) fn read_open_projection_fast(path: &Path) -> Option<DocumentProjection> {
  if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
    if extension.eq_ignore_ascii_case("docx") {
      // The open screen's preview already materialized this exact file; reuse
      // it for an instant paint (the Loro import still runs in phase G). Peek
      // WITHOUT consuming — `load_document_for_open` takes the bridge entry.
      return peek_docx_preview_bridge(path);
    }
    if extension.eq_ignore_ascii_case("pdf") {
      // PDFs carry an embedded .db8 but need extraction; no cheap cache path.
      return None;
    }
  }
  // .db8 (and unknown extensions): the frontier-current projection cache.
  flowstate_document::DocumentPackage::read_cached_projection(path).ok().flatten()
}

fn peek_docx_preview_bridge(path: &Path) -> Option<DocumentProjection> {
  let (modified, len) = docx_fingerprint(path)?;
  let bridge = DOCX_PREVIEW_BRIDGE.lock().ok()?;
  bridge
    .iter()
    .find(|(existing, existing_modified, existing_len, _)| existing == path && *existing_modified == modified && *existing_len == len)
    .map(|(.., document)| document.clone())
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
      let (document, _) = convert_docx_to_document(path)?;
      store_docx_preview_bridge(path, &document);
      return Ok(document);
    }
    if extension.eq_ignore_ascii_case("pdf") {
      let db8_bytes = crate::docx_conversion::extract_db8_bytes_from_pdf(path)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "PDF does not contain embedded Flowstate DB8 data"))?;
      return flowstate_document::read_db8_bytes(&db8_bytes);
    }
  }
  // §P3 preview fast path: a frontier-current projection cache renders the
  // preview without decoding, hashing, or validating the package's snapshot
  // and segment chunks (the ".db8 preview slower than the equivalent .docx"
  // field report). Stale/absent cache falls back to the full read below.
  if let Some(document) = flowstate_document::DocumentPackage::read_cached_projection(path)? {
    return Ok(document);
  }
  flowstate_document::read_db8(path)
}
