use std::path::{Path, PathBuf};

pub const DEFAULT_DOCUMENT_EXTENSION: &str = "db8";

#[hotpath::measure]
#[must_use]
pub fn recovery_path_for_document(path: &Path) -> PathBuf {
  let mut recovery_path = path.to_path_buf();
  let file_name = path
    .file_name()
    .and_then(|name| name.to_str())
    .map_or_else(|| "untitled.db8.recovery".to_owned(), |name| format!("{name}.recovery"));
  recovery_path.set_file_name(file_name);
  recovery_path
}
