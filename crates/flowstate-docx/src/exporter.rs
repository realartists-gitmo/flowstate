mod blocks;
mod formatting;
mod omml_export;
mod package;
mod sections;
mod styles;
mod xml_postprocess;

#[cfg(test)]
mod fidelity_tests;

use std::{
  io::{self, Cursor},
  path::Path,
};

use docx_rs::Docx;
use flowstate_document::{Block, DocumentProjection};
use flowstate_fidelity::FidelityClass;

use self::{
  blocks::add_block, formatting::docx_fonts, package::write_recompressed_docx, sections::SectionContext, styles::add_flowstate_styles,
  xml_postprocess::SideChannel,
};

/// A non-fatal export degradation. Currently records image assets that could not
/// be embedded and were exported as descriptive text instead (FS-129).
pub type ExportWarning = String;

#[hotpath::measure]
pub fn write_docx(path: impl AsRef<Path>, document: &DocumentProjection) -> io::Result<()> {
  write_docx_with_report(path, document).map(|_warnings| ())
}

/// Like [`write_docx`] but returns the structured warnings gathered while
/// building the package (e.g. un-embeddable image assets).
#[hotpath::measure]
pub fn write_docx_with_report(path: impl AsRef<Path>, document: &DocumentProjection) -> io::Result<Vec<ExportWarning>> {
  // Import/export fidelity: record the shape of the projection entering DOCX
  // export. The closure only runs when tracing is enabled, so counting is free
  // when off.
  flowstate_fidelity::event(FidelityClass::ImportExport, "write-docx", || {
    let (mut paragraphs, mut tables, mut images, mut equations) = (0_usize, 0_usize, 0_usize, 0_usize);
    for block in document.blocks.iter() {
      match block {
        Block::Paragraph(_) => paragraphs += 1,
        Block::Table(_) => tables += 1,
        Block::Image(_) => images += 1,
        Block::Equation(_) => equations += 1,
      }
    }
    format!(
      "paragraphs={paragraphs} tables={tables} images={images} equations={equations} sections={}",
      document.sections.len()
    )
  });

  let path = path.as_ref();
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    std::fs::create_dir_all(parent)?;
  }

  // FS-126: resolve page sections before emitting blocks so section boundaries
  // and the document-level page properties can be placed correctly.
  let context = SectionContext::resolve(document);
  let mut docx = add_flowstate_styles(Docx::new().default_fonts(docx_fonts(&document.theme)), &document.theme);
  docx = context.apply_document_section(docx);

  let mut side = SideChannel::default();
  for (block_ix, block) in document.blocks.iter().enumerate() {
    let boundary = context.boundary_section_property(block_ix);
    docx = add_block(docx, document, block, &document.theme, &context, &mut side, boundary);
  }

  let mut uncompressed_package = Cursor::new(Vec::new());
  docx
    .build()
    .pack(&mut uncompressed_package)
    .map_err(|error| io::Error::other(format!("failed to write docx package: {error}")))?;
  write_recompressed_docx(path, uncompressed_package.into_inner(), &side)?;
  Ok(side.into_warnings())
}

#[hotpath::measure]
pub fn convert_db8_to_docx(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let document = flowstate_document::read_db8(input)?;
  write_docx(output, &document)
}
