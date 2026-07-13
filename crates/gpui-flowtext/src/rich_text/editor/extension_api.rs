use std::{fmt, ops::Range, sync::Arc};

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

impl RichTextEditor {
  pub fn apply_extension_edits(
    &mut self,
    expected_generation: u64,
    edits: &[ExtensionDocumentEdit],
    cx: &mut Context<Self>,
  ) -> Result<u64, ExtensionEditError> {
    if expected_generation != self.edit_generation {
      return Err(ExtensionEditError::StaleGeneration {
        expected: expected_generation,
        actual: self.edit_generation,
      });
    }
    if !self.can_write_collaboration() {
      return Err(ExtensionEditError::ReadOnly);
    }
    if edits.is_empty() {
      return Ok(self.edit_generation);
    }

    let mut replacement = self.document.clone();
    for edit in edits {
      apply_extension_edit(&mut replacement, edit)?;
    }
    rebuild_document_sections(&mut replacement);
    reconcile_document_ids(&mut replacement);
    if document_bytes(&replacement).is_err() {
      return Err(ExtensionEditError::InvalidDocument);
    }

    let before_selection = self.selection.clone();
    let before = std::mem::replace(&mut self.document, replacement);
    self.selection.anchor = clamped_extension_offset(&self.document, self.selection.anchor);
    self.selection.head = clamped_extension_offset(&self.document, self.selection.head);
    self.selected_block = None;
    self.push_replace_document_history(before, before_selection, cx);
    Ok(self.edit_generation)
  }
}

fn apply_extension_edit(document: &mut Document, edit: &ExtensionDocumentEdit) -> Result<(), ExtensionEditError> {
  match edit {
    ExtensionDocumentEdit::ReplaceText { range, fragment } => {
      if range.start > range.end
        || !paragraph_offset_in_bounds(document, range.start)
        || !paragraph_offset_in_bounds(document, range.end)
      {
        return Err(ExtensionEditError::InvalidRange);
      }
      let start_text = paragraph_text(document, range.start.paragraph);
      let end_text = paragraph_text(document, range.end.paragraph);
      if !start_text.is_char_boundary(range.start.byte) || !end_text.is_char_boundary(range.end.byte) {
        return Err(ExtensionEditError::InvalidRange);
      }
      delete_cross_paragraph_range(document, range.clone());
      for asset in &fragment.assets {
        document.assets.assets.insert(
          asset.id,
          AssetRecord {
            id: asset.id,
            mime_type: asset.mime_type.clone().into(),
            original_name: asset.original_name.clone().map(Into::into),
            content_hash: asset.content_hash,
            bytes: Arc::new(asset.bytes.clone()),
          },
        );
      }
      insert_rich_fragment_at(document, range.start, fragment);
    },
    ExtensionDocumentEdit::ReplaceBlock { block_ix, block } => {
      let Some(slot) = Arc::make_mut(&mut document.blocks).get_mut(*block_ix) else {
        return Err(ExtensionEditError::InvalidBlock(*block_ix));
      };
      *slot = block.clone();
    },
    ExtensionDocumentEdit::ReplaceTableCell {
      block_ix,
      row_ix,
      cell_ix,
      blocks,
    } => {
      let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(*block_ix) else {
        return Err(ExtensionEditError::InvalidBlock(*block_ix));
      };
      let Block::Table(table) = block else {
        return Err(ExtensionEditError::NotATable(*block_ix));
      };
      let Some(cell) = table.rows.get_mut(*row_ix).and_then(|row| row.cells.get_mut(*cell_ix)) else {
        return Err(ExtensionEditError::InvalidTableCell {
          block_ix: *block_ix,
          row_ix: *row_ix,
          cell_ix: *cell_ix,
        });
      };
      cell.blocks.clone_from(blocks);
      table.version = table.version.wrapping_add(1);
    },
    ExtensionDocumentEdit::ReplaceDocument(replacement) => *document = replacement.clone(),
  }
  Ok(())
}

fn clamped_extension_offset(document: &Document, offset: DocumentOffset) -> DocumentOffset {
  let paragraph = offset.paragraph.min(document.paragraphs.len().saturating_sub(1));
  DocumentOffset {
    paragraph,
    byte: offset.byte.min(document.paragraphs.get(paragraph).map_or(0, paragraph_text_len)),
  }
}
