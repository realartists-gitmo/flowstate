use std::{
  fs::File,
  io::{self, Cursor, Read},
  path::Path,
};

use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

use super::xml_postprocess::{SideChannel, rewrite_document_xml};

/// The main document part rewritten by the post-process seam.
const DOCUMENT_PART: &str = "word/document.xml";

#[hotpath::measure]
pub(super) fn write_recompressed_docx(path: &Path, package: Vec<u8>, side: &SideChannel) -> io::Result<()> {
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
    } else if name == DOCUMENT_PART {
      // FS-124/125/127: route the main document part through the OOXML rewrite
      // seam before recompressing it back into the package.
      let mut bytes = Vec::with_capacity(entry.size() as usize);
      entry.read_to_end(&mut bytes)?;
      let rewritten = rewrite_document_xml(bytes, side);
      writer
        .start_file(name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx entry: {error}")))?;
      io::Write::write_all(&mut writer, &rewritten)?;
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
