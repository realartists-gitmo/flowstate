use std::fmt;

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
  EquationSource {
    block_ix: usize,
    anchor: usize,
    head: usize,
  },
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
  SpliceBlocks {
    range: Range<usize>,
    blocks: Vec<InputBlock>,
    assets: Vec<InputAsset>,
  },
  ReplaceTableCell {
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    blocks: Vec<TableCellBlock>,
  },
  ReplaceDocument(Box<Document>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionEditError {
  StaleGeneration { expected: u64, actual: u64 },
  ReadOnly,
  InvalidRange,
  InvalidBlock(usize),
  InvalidBlockRange,
  NotATable(usize),
  InvalidTableCell { block_ix: usize, row_ix: usize, cell_ix: usize },
  InvalidDocument,
}

impl fmt::Display for ExtensionEditError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::StaleGeneration { expected, actual } => write!(f, "document generation changed (expected {expected}, found {actual})"),
      Self::ReadOnly => f.write_str("document is read-only"),
      Self::InvalidRange => f.write_str("text range is outside the document or not on UTF-8 boundaries"),
      Self::InvalidBlock(block_ix) => write!(f, "block index {block_ix} is outside the document"),
      Self::InvalidBlockRange => f.write_str("block splice range is outside the document"),
      Self::NotATable(block_ix) => write!(f, "block {block_ix} is not a table"),
      Self::InvalidTableCell { block_ix, row_ix, cell_ix } => {
        write!(f, "table cell {block_ix}:{row_ix}:{cell_ix} does not exist")
      },
      Self::InvalidDocument => f.write_str("replacement would leave an invalid document"),
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
      insert_extension_assets(&mut document.assets, &fragment.assets);
      insert_rich_fragment_at(document, range.start, fragment);
    },
    ExtensionDocumentEdit::ReplaceBlock { block_ix, block } => {
      let Some(slot) = Arc::make_mut(&mut document.blocks).get_mut(*block_ix) else {
        return Err(ExtensionEditError::InvalidBlock(*block_ix));
      };
      *slot = block.clone();
    },
    ExtensionDocumentEdit::SpliceBlocks { range, blocks, assets } => {
      if range.start > range.end || range.end > document.blocks.len() {
        return Err(ExtensionEditError::InvalidBlockRange);
      }
      let mut input_blocks = extension_input_blocks(document);
      input_blocks.splice(range.clone(), blocks.iter().cloned());
      let mut replacement_assets = document.assets.clone();
      insert_extension_assets(&mut replacement_assets, assets);
      *document = document_from_extension_blocks(document, input_blocks, replacement_assets);
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
    ExtensionDocumentEdit::ReplaceDocument(replacement) => *document = (**replacement).clone(),
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

fn insert_extension_assets(store: &mut AssetStore, assets: &[InputAsset]) {
  for asset in assets {
    store.assets.insert(
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
}

fn extension_input_blocks(document: &Document) -> Vec<InputBlock> {
  let mut paragraph_ix = 0;
  document
    .blocks
    .iter()
    .map(|block| match block {
      Block::Paragraph(_) => {
        let paragraph = &document.paragraphs[paragraph_ix];
        let input = input_paragraph_from_document_range(document, paragraph_ix, 0..paragraph_text_len(paragraph));
        paragraph_ix += 1;
        InputBlock::Paragraph(input)
      },
      _ => input_block_from_block(block),
    })
    .collect()
}

fn document_from_extension_blocks(document: &Document, mut blocks: Vec<InputBlock>, assets: AssetStore) -> Document {
  if !blocks.iter().any(|block| matches!(block, InputBlock::Paragraph(_))) {
    blocks.push(InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    }));
  }
  let paragraphs = blocks
    .iter()
    .filter_map(|block| match block {
      InputBlock::Paragraph(paragraph) => Some(paragraph.clone()),
      _ => None,
    })
    .collect();
  let mut replacement = document_from_input(document.theme.clone(), paragraphs);
  let mut paragraph_ix = 0;
  replacement.blocks = Arc::new(
    blocks
      .iter()
      .map(|block| match block {
        InputBlock::Paragraph(_) => {
          let block = Block::Paragraph(replacement.paragraphs[paragraph_ix].clone());
          paragraph_ix += 1;
          block
        },
        _ => block_from_input_block(block),
      })
      .collect(),
  );
  replacement.assets = assets;
  replacement.ids = document.ids.clone();
  reconcile_document_ids(&mut replacement);
  rebuild_document_sections(&mut replacement);
  replacement
}

#[cfg(test)]
mod extension_api_tests {
  use super::*;

  fn text_document(text: &str) -> Document {
    document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: text.to_owned(),
          styles: RunStyles::default(),
        }],
      }],
    )
  }

  #[test]
  fn rich_text_replacement_is_applied_to_a_candidate_document() {
    let mut document = text_document("alpha omega");
    let styles = RunStyles::default().with(RunStyle::Semantic(2));
    let edit = ExtensionDocumentEdit::ReplaceText {
      range: DocumentOffset { paragraph: 0, byte: 6 }..DocumentOffset { paragraph: 0, byte: 11 },
      fragment: RichClipboardFragment {
        format: RICH_TEXT_CLIPBOARD_FORMAT.to_owned(),
        paragraphs: vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "beta".to_owned(),
            styles,
          }],
        }],
        blocks: Vec::new(),
        assets: Vec::new(),
      },
    };

    apply_extension_edit(&mut document, &edit).unwrap();
    assert_eq!(paragraph_text(&document, 0), "alpha beta");
    assert_eq!(document.paragraphs[0].runs.last().unwrap().styles, styles);
  }

  #[test]
  fn text_replacement_rejects_non_utf8_boundary() {
    let mut document = text_document("café");
    let edit = ExtensionDocumentEdit::ReplaceText {
      range: DocumentOffset { paragraph: 0, byte: 4 }..DocumentOffset { paragraph: 0, byte: 5 },
      fragment: RichClipboardFragment {
        format: RICH_TEXT_CLIPBOARD_FORMAT.to_owned(),
        paragraphs: Vec::new(),
        blocks: Vec::new(),
        assets: Vec::new(),
      },
    };

    assert_eq!(apply_extension_edit(&mut document, &edit), Err(ExtensionEditError::InvalidRange));
    assert_eq!(paragraph_text(&document, 0), "café");
  }

  #[test]
  fn block_splice_preserves_ordered_projection() {
    let mut document = text_document("before");
    let equation = InputBlock::Equation(InputEquationBlock {
      source: "x^2".to_owned(),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    });
    let paragraph = InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Custom(2),
      runs: vec![InputRun {
        text: "after".to_owned(),
        styles: RunStyles::default(),
      }],
    });

    apply_extension_edit(
      &mut document,
      &ExtensionDocumentEdit::SpliceBlocks {
        range: 0..1,
        blocks: vec![equation, paragraph],
        assets: Vec::new(),
      },
    )
    .unwrap();

    assert!(matches!(&document.blocks[0], Block::Equation(equation) if equation.source.as_ref() == "x^2"));
    assert!(matches!(&document.blocks[1], Block::Paragraph(_)));
    assert_eq!(paragraph_text(&document, 0), "after");
    assert!(document_bytes(&document).is_ok());
  }

}
