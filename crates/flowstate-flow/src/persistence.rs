use std::fs;
use std::path::Path;

use crate::document::FlowDocument;

pub fn load_flow_document(_path: impl AsRef<Path>) -> anyhow::Result<FlowDocument> {
  Ok(FlowDocument::new())
}

pub fn load_flow_document_or_new(_path: impl AsRef<Path>) -> FlowDocument {
  FlowDocument::new()
}

pub fn save_flow_document(_path: impl AsRef<Path>, _document: &FlowDocument) -> anyhow::Result<()> {
  let path = _path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }
  fs::write(path, b"{}")?;
  Ok(())
}
