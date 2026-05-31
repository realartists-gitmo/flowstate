use std::{
  fs::File,
  io::{self, Cursor},
  path::Path,
};

use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

#[hotpath::measure]
pub(super) fn write_recompressed_docx(path: &Path, package: Vec<u8>) -> io::Result<()> {
  let mut archive =
    ZipArchive::new(Cursor::new(package)).map_err(|error| io::Error::other(format!("failed to read generated docx package: {error}")))?;
  let file = File::create(path)?;
  let mut writer = ZipWriter::new(file);
  for index in 0..archive.len() {
    let mut entry = archive
      .by_index(index)
      .map_err(|error| io::Error::other(format!("failed to read generated docx entry: {error}")))?;
    let name = entry.name().to_owned();
    let mut options = FileOptions::default()
      .compression_method(CompressionMethod::Deflated)
      .last_modified_time(entry.last_modified());
    if let Some(mode) = entry.unix_mode() {
      options = options.unix_permissions(mode);
    }
    if entry.is_dir() {
      writer
        .add_directory(name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx directory: {error}")))?;
    } else {
      writer
        .start_file(name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx entry: {error}")))?;
      io::copy(&mut entry, &mut writer)?;
    }
  }
  writer
    .finish()
    .map_err(|error| io::Error::other(format!("failed to finish docx package: {error}")))?;
  Ok(())
}
