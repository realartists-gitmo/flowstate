mod cleaner;
mod exporter;
mod interpreter;

use std::{io, path::Path};

pub use cleaner::{CleanAction, CleanedDocx, DocxCleanReport, DocxCleanStats, clean_docx_bytes};
pub use exporter::{convert_db8_to_docx, write_docx};
pub use interpreter::{
  DocxConversionReport, RecognitionRule, convert_cleaned_docx_to_document, convert_docx_bytes_to_document, convert_docx_to_document,
};

use flowstate_document::write_db8;

#[hotpath::measure]
pub fn convert_docx_to_db8(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<DocxConversionReport> {
  let (document, report) = convert_docx_to_document(input)?;
  write_db8(output, &document)?;
  Ok(report)
}
