use std::{
  ffi::OsString,
  path::{Path, PathBuf},
};

use crate::rich_text_element::{Document, blank_document};

pub const UNTITLED_DOCUMENT_NAME: &str = "Untitled.db8";
pub const UNTITLED_FLOW_NAME: &str = "Untitled.fl0";

/// Create the document used by File > New and the empty-workspace New button.
#[hotpath::measure]
pub fn new_blank_document() -> Document {
  blank_document()
}

/// Use the process working directory as the first save location. If it cannot
/// be read, fall back to the user's home directory, then the filesystem root.
#[hotpath::measure]
pub fn default_save_directory() -> PathBuf {
  std::env::current_dir()
    .ok()
    .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    .unwrap_or_else(|| PathBuf::from("/"))
}

/// Keep untitled saves in the native `.db8` format even if the user types a
/// bare filename in the save dialog.
#[hotpath::measure]
pub fn normalize_db8_path(path: PathBuf) -> PathBuf {
  if path.extension().is_some() {
    return path;
  }

  let mut file_name = path
    .file_name()
    .map(|name| name.to_os_string())
    .unwrap_or_else(|| OsString::from(UNTITLED_DOCUMENT_NAME));
  file_name.push(".db8");

  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    parent.join(file_name)
  } else {
    Path::new(&file_name).to_path_buf()
  }
}

/// Keep flow saves in the native `.fl0` format when the user types a bare filename.
#[hotpath::measure]
pub fn normalize_fl0_path(path: PathBuf) -> PathBuf {
  normalize_path_with_extension(path, UNTITLED_FLOW_NAME, ".fl0")
}

#[hotpath::measure]
fn normalize_path_with_extension(path: PathBuf, fallback_name: &str, extension: &str) -> PathBuf {
  if path.extension().is_some() {
    return path;
  }

  let mut file_name = path
    .file_name()
    .map(|name| name.to_os_string())
    .unwrap_or_else(|| OsString::from(fallback_name));
  file_name.push(extension);

  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    parent.join(file_name)
  } else {
    Path::new(&file_name).to_path_buf()
  }
}
