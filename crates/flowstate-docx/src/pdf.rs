use std::{fs, io, path::Path};

#[hotpath::measure]
pub fn convert_docx_to_pdf(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let input = input.as_ref();
  let output = output.as_ref();
  if let Some(parent) = output.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }

  docxide_pdf::convert_docx_to_pdf(input, output).map_err(|error| io::Error::other(format!("failed to convert DOCX to PDF: {error}")))
}
