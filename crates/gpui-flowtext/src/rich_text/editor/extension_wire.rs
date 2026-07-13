#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtensionWireOffset {
  pub paragraph: usize,
  pub byte: usize,
}

impl From<ExtensionWireOffset> for DocumentOffset {
  fn from(offset: ExtensionWireOffset) -> Self {
    Self {
      paragraph: offset.paragraph,
      byte: offset.byte,
    }
  }
}

impl From<DocumentOffset> for ExtensionWireOffset {
  fn from(offset: DocumentOffset) -> Self {
    Self {
      paragraph: offset.paragraph,
      byte: offset.byte,
    }
  }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionWireSelection {
  Text {
    anchor: ExtensionWireOffset,
    head: ExtensionWireOffset,
  },
  Object {
    block_ix: usize,
  },
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

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExtensionWireDocument {
  pub blocks: Vec<InputBlock>,
  pub assets: Vec<InputAsset>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExtensionWireSnapshot {
  pub generation: u64,
  pub document: ExtensionWireDocument,
  pub selection: ExtensionWireSelection,
  pub selected_text: String,
  pub selected_fragment: RichClipboardFragment,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ExtensionWireEditRequest {
  pub expected_generation: u64,
  pub edits: Vec<ExtensionWireEdit>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionWireEdit {
  ReplaceText {
    start: ExtensionWireOffset,
    end: ExtensionWireOffset,
    fragment: RichClipboardFragment,
  },
  SpliceBlocks {
    start: usize,
    end: usize,
    blocks: Vec<InputBlock>,
    #[serde(default)]
    assets: Vec<InputAsset>,
  },
  ReplaceTableCell {
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    blocks: Vec<InputTableCellBlock>,
  },
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ExtensionWireEditResponse {
  pub generation: u64,
}

#[derive(Debug)]
pub enum ExtensionWireError {
  Json(serde_json::Error),
  Edit(ExtensionEditError),
}

impl std::fmt::Display for ExtensionWireError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Json(error) => write!(f, "invalid extension JSON: {error}"),
      Self::Edit(error) => error.fmt(f),
    }
  }
}

impl std::error::Error for ExtensionWireError {}

impl From<serde_json::Error> for ExtensionWireError {
  fn from(error: serde_json::Error) -> Self {
    Self::Json(error)
  }
}

impl From<ExtensionEditError> for ExtensionWireError {
  fn from(error: ExtensionEditError) -> Self {
    Self::Edit(error)
  }
}

impl RichTextEditor {
  pub fn extension_snapshot_json(&self) -> Result<String, serde_json::Error> {
    let snapshot = self.extension_snapshot();
    let selection = match snapshot.selection {
      ExtensionSelection::Text(selection) => ExtensionWireSelection::Text {
        anchor: selection.anchor.into(),
        head: selection.head.into(),
      },
      ExtensionSelection::Object { block_ix } => ExtensionWireSelection::Object { block_ix },
      ExtensionSelection::EquationSource { block_ix, anchor, head } => {
        ExtensionWireSelection::EquationSource { block_ix, anchor, head }
      },
      ExtensionSelection::TableCell {
        block_ix,
        row_ix,
        cell_ix,
        anchor,
        head,
      } => ExtensionWireSelection::TableCell {
        block_ix,
        row_ix,
        cell_ix,
        anchor,
        head,
      },
    };
    let assets = snapshot
      .document
      .assets
      .assets
      .values()
      .map(|asset| InputAsset {
        id: asset.id,
        mime_type: asset.mime_type.to_string(),
        original_name: asset.original_name.as_ref().map(ToString::to_string),
        content_hash: asset.content_hash,
        bytes: asset.bytes.as_ref().clone(),
      })
      .collect();
    serde_json::to_string(&ExtensionWireSnapshot {
      generation: snapshot.generation,
      document: ExtensionWireDocument {
        blocks: extension_input_blocks(&snapshot.document),
        assets,
      },
      selection,
      selected_text: snapshot.selected_text,
      selected_fragment: snapshot.selected_fragment,
    })
  }

  pub fn apply_extension_edits_json(
    &mut self,
    request: &str,
    cx: &mut Context<Self>,
  ) -> Result<String, ExtensionWireError> {
    let request: ExtensionWireEditRequest = serde_json::from_str(request)?;
    let edits = request.edits.iter().map(extension_edit_from_wire).collect::<Vec<_>>();
    let generation = self.apply_extension_edits(request.expected_generation, &edits, cx)?;
    Ok(serde_json::to_string(&ExtensionWireEditResponse { generation })?)
  }
}

fn extension_edit_from_wire(edit: &ExtensionWireEdit) -> ExtensionDocumentEdit {
  match edit {
    ExtensionWireEdit::ReplaceText { start, end, fragment } => ExtensionDocumentEdit::ReplaceText {
      range: (*start).into()..(*end).into(),
      fragment: fragment.clone(),
    },
    ExtensionWireEdit::SpliceBlocks { start, end, blocks, assets } => ExtensionDocumentEdit::SpliceBlocks {
      range: *start..*end,
      blocks: blocks.clone(),
      assets: assets.clone(),
    },
    ExtensionWireEdit::ReplaceTableCell {
      block_ix,
      row_ix,
      cell_ix,
      blocks,
    } => ExtensionDocumentEdit::ReplaceTableCell {
      block_ix: *block_ix,
      row_ix: *row_ix,
      cell_ix: *cell_ix,
      blocks: blocks.iter().map(table_cell_block_from_wire).collect(),
    },
  }
}

fn table_cell_block_from_wire(block: &InputTableCellBlock) -> TableCellBlock {
  match block {
    InputTableCellBlock::Paragraph(paragraph) => {
      TableCellBlock::Paragraph(table_cell_paragraph_from_input_paragraph(paragraph))
    },
    InputTableCellBlock::Table(table) => TableCellBlock::Table(table_from_input_table(table)),
  }
}
