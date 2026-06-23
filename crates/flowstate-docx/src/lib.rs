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
  import_cleaned_docx_to_loro, import_docx_bytes_to_loro, import_docx_to_loro,
};
pub use pdf::{convert_db8_to_pdf, convert_docx_to_pdf, write_pdf, write_pdf_with_db8_bytes};
pub use pdf_recovery::{FlowstatePdfPayloadInfo, convert_pdf_to_db8, embed_db8_bytes_in_pdf, embed_db8_file_in_pdf, extract_db8_bytes_from_pdf};

#[hotpath::measure]
pub fn convert_docx_to_db8(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<DocxConversionReport> {
  let (imported, report) = import_docx_to_loro(input, "Imported DOCX")?;
  flowstate_document::DocumentPackage::from_loro_snapshot_with_assets(
    &imported.doc,
    "Imported DOCX",
    flowstate_document::loro_import::assets_from_document(&imported.projection),
  )?
  .write(output)?;
  Ok(report)
}
