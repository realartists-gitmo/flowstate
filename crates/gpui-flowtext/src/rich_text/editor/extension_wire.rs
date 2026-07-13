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
