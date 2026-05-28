fn load_document_for_open(path: &PathBuf) -> std::io::Result<(Document, Option<PathBuf>)> {
  if path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("docx"))
  {
    let db8_path = path.with_extension("db8");
    if converted_db8_is_fresh(path, &db8_path)
      && let Ok(document) = read_db8(&db8_path)
    {
      return Ok((document, Some(db8_path)));
    }

    let (document, _) = convert_docx_to_document(path)?;
    return Ok((document, Some(db8_path)));
  }

  load_or_create_document(path).map(|document| (document, Some(path.clone())))
}

fn converted_db8_is_fresh(source_path: &Path, db8_path: &Path) -> bool {
  let Ok(source_modified) = fs::metadata(source_path).and_then(|metadata| metadata.modified()) else {
    return false;
  };
  let Ok(db8_modified) = fs::metadata(db8_path).and_then(|metadata| metadata.modified()) else {
    return false;
  };
  db8_modified >= source_modified
}

