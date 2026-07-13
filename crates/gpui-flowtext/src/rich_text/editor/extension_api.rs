use std::{fmt, ops::Range};

use super::*;

#[derive(Clone, Debug)]
pub struct ExtensionDocumentSnapshot {
  pub generation: u64,
  pub document: Document,
  pub selection: ExtensionSelection,
  pub selected_text: String,
  pub selected_fragment: RichClipboardFragment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionSelection {
  Text(EditorSelection),
  Object { block_ix: usize },
  TableCell {
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    anchor: usize,
    head: usize,
  },
}

#[derive(Clone, Debug)]
pub enum ExtensionDocumentEdit {
  ReplaceText {
    range: Range<DocumentOffset>,
    fragment: RichClipboardFragment,
  },
  ReplaceBlock {
    block_ix: usize,
    block: Block,
  },
  ReplaceTableCell {
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    blocks: Vec<TableCellBlock>,
  },
  ReplaceDocument(Document),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionEditError {
  StaleGeneration { expected: u64, actual: u64 },
  ReadOnly,
  InvalidRange,
  InvalidBlock(usize),
  NotATable(usize),
  InvalidTableCell { block_ix: usize, row_ix: usize, cell_ix: usize },
  InvalidDocument,
}

impl fmt::Display for ExtensionEditError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::StaleGeneration { expected, actual } => write!(formatter, "document generation changed (expected {expected}, found {actual})"),
      Self::ReadOnly => formatter.write_str("document is read-only"),
      Self::InvalidRange => formatter.write_str("text range is outside the document or not on UTF-8 boundaries"),
      Self::InvalidBlock(block_ix) => write!(formatter, "block index {block_ix} is outside the document"),
      Self::NotATable(block_ix) => write!(formatter, "block {block_ix} is not a table"),
      Self::InvalidTableCell { block_ix, row_ix, cell_ix } => {
        write!(formatter, "table cell {block_ix}:{row_ix}:{cell_ix} does not exist")
      },
      Self::InvalidDocument => formatter.write_str("replacement would leave an invalid document"),
    }
  }
}

impl std::error::Error for ExtensionEditError {}
