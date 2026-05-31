use std::{
  fs, io,
  path::{Path, PathBuf},
  time::{SystemTime, UNIX_EPOCH},
};

use flowstate_document::{Document, db8_bytes, read_db8};

use crate::{embed_db8_bytes_in_pdf, write_docx};

#[hotpath::measure]
pub fn convert_docx_to_pdf(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let input = input.as_ref();
  let output = output.as_ref();
  if let Some(parent) = output.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }

  docxide_pdf::convert_docx_to_pdf(input, output).map_err(|error| io::Error::other(format!("failed to convert DOCX to PDF: {error}")))
}

#[hotpath::measure]
pub fn write_pdf(path: impl AsRef<Path>, document: &Document) -> io::Result<()> {
  let db8 = db8_bytes(document)?;
  write_pdf_with_db8_bytes(path, document, &db8)
}

#[hotpath::measure]
pub fn write_pdf_with_db8_bytes(path: impl AsRef<Path>, document: &Document, db8_bytes: &[u8]) -> io::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }

  let temp_docx = temp_sibling_path(path, "docx");
  let temp_pdf = temp_sibling_path(path, "pdf");
  let result = (|| {
    write_docx(&temp_docx, document)?;
    convert_docx_to_pdf(&temp_docx, &temp_pdf)?;
    embed_db8_bytes_in_pdf(&temp_pdf, db8_bytes, path)?;
    Ok(())
  })();
  let _ = fs::remove_file(&temp_docx);
  let _ = fs::remove_file(&temp_pdf);
  result
}

#[hotpath::measure]
pub fn convert_db8_to_pdf(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let input = input.as_ref();
  let document = read_db8(input)?;
  let db8 = fs::read(input)?;
  write_pdf_with_db8_bytes(output, &document, &db8)
}

#[hotpath::measure]
fn temp_sibling_path(target: &Path, extension: &str) -> PathBuf {
  let dir = target.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
  let stem = target.file_stem().and_then(|stem| stem.to_str()).unwrap_or("flowstate-export");
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| duration.as_nanos())
    .unwrap_or_default();
  dir.join(format!(".{stem}.{}.tmp.{extension}", nanos))
}
