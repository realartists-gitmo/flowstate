use std::{io, path::Path};

pub use flowstate_docx::{
  CleanAction, CleanedDocx, DocxCleanReport, DocxCleanStats, DocxConversionReport, RecognitionRule, clean_docx_bytes,
  convert_cleaned_docx_to_document, convert_docx_bytes_to_document, convert_docx_to_db8, convert_docx_to_document, convert_docx_to_pdf,
  write_docx,
};

use crate::app_settings::load_document_theme;

#[hotpath::measure]
pub fn convert_db8_to_docx(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let mut document = flowstate_document::read_db8(input)?;
  document.theme = load_document_theme();
  flowstate_docx::write_docx(output, &document)
}
