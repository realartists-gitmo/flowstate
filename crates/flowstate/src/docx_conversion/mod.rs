use std::{io, path::Path};

pub use flowstate_docx::{
  CleanAction, CleanedDocx, DocxCleanReport, DocxCleanStats, DocxConversionReport, PdfConversionReport, PdfImportDecision, PdfRecognitionRule,
  RecognitionRule, analyze_pdf_import, clean_docx_bytes, convert_cleaned_docx_to_document, convert_docx_bytes_to_document, convert_docx_to_db8,
  convert_docx_to_document, convert_docx_to_pdf, convert_pdf_to_db8, convert_pdf_to_document, convert_recognized_pdf_to_db8,
  embed_db8_file_in_pdf, extract_db8_bytes_from_pdf, write_docx, write_pdf, write_pdf_with_db8_bytes,
};

use crate::app_settings::load_document_theme;

#[hotpath::measure]
pub fn convert_db8_to_docx(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let mut document = flowstate_document::read_db8(input)?;
  document.theme = load_document_theme();
  flowstate_docx::write_docx(output, &document)
}

#[hotpath::measure]
pub fn convert_db8_to_pdf(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let input = input.as_ref();
  let mut document = flowstate_document::read_db8(input)?;
  document.theme = load_document_theme();
  let db8_bytes = std::fs::read(input)?;
  flowstate_docx::write_pdf_with_db8_bytes(output, &document, &db8_bytes)
}
