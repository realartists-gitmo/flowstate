mod cleaner;
mod exporter;
mod interpreter;
mod pdf;
mod pdf_recovery;

use std::{io, path::Path};

pub use cleaner::{CleanAction, CleanedDocx, DocxCleanReport, DocxCleanStats, clean_docx_bytes};
pub use exporter::{convert_db8_to_docx, write_docx};
pub use interpreter::{
  DocxConversionReport, RecognitionRule, convert_cleaned_docx_to_document, convert_docx_bytes_to_document, convert_docx_to_document,
};
pub use pdf::{convert_db8_to_pdf, convert_docx_to_pdf, write_pdf, write_pdf_with_db8_bytes};
pub use pdf_recovery::{FlowstatePdfPayloadInfo, convert_pdf_to_db8, embed_db8_bytes_in_pdf, embed_db8_file_in_pdf, extract_db8_bytes_from_pdf};

use flowstate_document::write_imported_document_as_loro_db8;

#[hotpath::measure]
pub fn convert_docx_to_db8(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<DocxConversionReport> {
  let (document, report) = convert_docx_to_document(input)?;
  write_imported_document_as_loro_db8(output, &document, "Imported DOCX")?;
  Ok(report)
}
