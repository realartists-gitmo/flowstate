use std::{
  fs,
  path::Path,
};

pub const CLEANING_RULES: &[CleanAction] = &[
  CleanAction::ReadWithRdocx,
  CleanAction::RecognizeKnownParagraphAndRunStyles,
  CleanAction::ResolveRunProperties,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanAction {
  ReadWithRdocx,
  RecognizeKnownParagraphAndRunStyles,
  ResolveRunProperties,
}

#[derive(Clone, Debug)]
pub struct CleanedDocx {
  pub bytes: Vec<u8>,
  pub report: DocxCleanReport,
}

#[derive(Clone, Debug)]
pub struct DocxCleanReport {
  pub stats: DocxCleanStats,
  pub actions: &'static [CleanAction],
}

#[derive(Default, Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocxCleanStats {
  pub styles_normalized: usize,
  pub styles_removed: usize,
  pub paragraphs_restyled: usize,
  pub runs_restyled: usize,
  pub hyperlinks_flattened: usize,
}

pub fn clean_docx_path(path: impl AsRef<Path>) -> std::io::Result<CleanedDocx> {
  clean_docx_bytes(&fs::read(path)?)
}

pub fn clean_docx_bytes(bytes: &[u8]) -> std::io::Result<CleanedDocx> {
  Ok(CleanedDocx {
    bytes: bytes.to_vec(),
    report: DocxCleanReport {
      stats: DocxCleanStats::default(),
      actions: CLEANING_RULES,
    },
  })
}
