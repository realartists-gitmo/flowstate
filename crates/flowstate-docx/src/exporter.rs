mod blocks;
mod formatting;
mod package;
mod styles;

use std::{
  io::{self, Cursor},
  path::Path,
};

use docx_rs::Docx;
use flowstate_document::Document;

use self::{blocks::add_block, formatting::docx_fonts, package::write_recompressed_docx, styles::add_flowstate_styles};

#[hotpath::measure]
pub fn write_docx(path: impl AsRef<Path>, document: &Document) -> io::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    std::fs::create_dir_all(parent)?;
  }
  let mut docx = add_flowstate_styles(Docx::new().default_fonts(docx_fonts(&document.theme)), &document.theme);
  for block in document.blocks.iter() {
    docx = add_block(docx, document, block, &document.theme);
  }
  let mut uncompressed_package = Cursor::new(Vec::new());
  docx
    .build()
    .pack(&mut uncompressed_package)
    .map_err(|error| io::Error::other(format!("failed to write docx package: {error}")))?;
  write_recompressed_docx(path, uncompressed_package.into_inner())
}

#[hotpath::measure]
pub fn convert_db8_to_docx(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let document = flowstate_document::read_db8(input)?;
  write_docx(output, &document)
}
