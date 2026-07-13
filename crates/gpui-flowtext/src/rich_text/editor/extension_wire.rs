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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtensionWireEditRequest {
  pub expected_generation: u64,
  pub edits: Vec<ExtensionWireEdit>,
}

#[derive(Clone, Debug, serde::Serialize)]
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

impl<'de> serde::Deserialize<'de> for ExtensionWireEdit {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    use serde::de::Error as _;

    #[derive(serde::Deserialize)]
    struct ReplaceText {
      start: ExtensionWireOffset,
      end: ExtensionWireOffset,
      fragment: RichClipboardFragment,
    }
    #[derive(serde::Deserialize)]
    struct SpliceBlocks {
      start: usize,
      end: usize,
      blocks: Vec<InputBlock>,
      #[serde(default)]
      assets: Vec<InputAsset>,
    }
    #[derive(serde::Deserialize)]
    struct ReplaceTableCell {
      block_ix: usize,
      row_ix: usize,
      cell_ix: usize,
      blocks: Vec<InputTableCellBlock>,
    }

    let mut value = serde_json::Value::deserialize(deserializer)?;
    normalize_asset_ids(&mut value).map_err(D::Error::custom)?;
    let kind = value
      .as_object_mut()
      .and_then(|object| object.remove("kind"))
      .and_then(|kind| kind.as_str().map(str::to_owned))
      .ok_or_else(|| D::Error::custom("extension edit is missing a string kind"))?;
    match kind.as_str() {
      "replace_text" => {
        let edit: ReplaceText = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self::ReplaceText {
          start: edit.start,
          end: edit.end,
          fragment: edit.fragment,
        })
      },
      "splice_blocks" => {
        let edit: SpliceBlocks = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self::SpliceBlocks {
          start: edit.start,
          end: edit.end,
          blocks: edit.blocks,
          assets: edit.assets,
        })
      },
      "replace_table_cell" => {
        let edit: ReplaceTableCell = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self::ReplaceTableCell {
          block_ix: edit.block_ix,
          row_ix: edit.row_ix,
          cell_ix: edit.cell_ix,
          blocks: edit.blocks,
        })
      },
      _ => Err(D::Error::custom(format!("unknown extension edit kind `{kind}`"))),
    }
  }
}

fn normalize_asset_ids(value: &mut serde_json::Value) -> Result<(), String> {
  match value {
    serde_json::Value::Object(object) => {
      let is_asset = object.contains_key("mime_type") && object.contains_key("bytes");
      for (key, value) in object {
        if (key == "asset_id" || (key == "id" && is_asset)) && value.is_string() {
          let text = value.as_str().expect("checked string");
          *value = serde_json::Value::Number(text.parse().map_err(|_| format!("invalid asset ID `{text}`"))?);
        } else {
          normalize_asset_ids(value)?;
        }
      }
    },
    serde_json::Value::Array(values) => {
      for value in values {
        normalize_asset_ids(value)?;
      }
    },
    _ => {},
  }
  Ok(())
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

#[cfg(test)]
mod extension_wire_tests {
  use super::*;

  #[test]
  fn block_splice_request_round_trips_and_converts() {
    let request = ExtensionWireEditRequest {
      expected_generation: 7,
      edits: vec![ExtensionWireEdit::SpliceBlocks {
        start: 1,
        end: 3,
        blocks: vec![InputBlock::Equation(InputEquationBlock {
          source: "x".to_owned(),
          syntax: InputEquationSyntax::Latex,
          display: InputEquationDisplay::Display,
        })],
        assets: Vec::new(),
      }],
    };
    let json = serde_json::to_string(&request).unwrap();
    let decoded: ExtensionWireEditRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.expected_generation, 7);
    assert!(matches!(
      extension_edit_from_wire(&decoded.edits[0]),
      ExtensionDocumentEdit::SpliceBlocks { range, blocks, .. }
        if range == (1..3) && matches!(&blocks[0], InputBlock::Equation(equation) if equation.source == "x")
    ));
  }

  #[test]
  fn block_splice_request_accepts_string_u128_asset_ids() {
    let asset_id = AssetId(u128::from(u64::MAX) + 1);
    let request = ExtensionWireEditRequest {
      expected_generation: 7,
      edits: vec![ExtensionWireEdit::SpliceBlocks {
        start: 0,
        end: 0,
        blocks: Vec::new(),
        assets: vec![InputAsset {
          id: asset_id,
          mime_type: "image/png".to_owned(),
          original_name: Some("pixel.png".to_owned()),
          content_hash: 42,
          bytes: vec![137, 80, 78, 71],
        }],
      }],
    };

    let json = serde_json::to_string(&request)
      .unwrap()
      .replace(&asset_id.0.to_string(), &format!("\"{}\"", asset_id.0));
    let decoded: ExtensionWireEditRequest = serde_json::from_str(&json).unwrap();

    assert!(matches!(
      &decoded.edits[0],
      ExtensionWireEdit::SpliceBlocks { assets, .. } if assets[0].id == asset_id
    ));
  }

  #[test]
  fn example_image_splice_accepts_legacy_numeric_asset_ids() {
    let json = r#"{
      "expected_generation": 7,
      "edits": [{
        "kind": "splice_blocks", "start": 0, "end": 0,
        "blocks": [{"Image": {
          "asset_id": 42, "alt_text": "example", "caption": null,
          "sizing": "Intrinsic", "alignment": "Center"
        }}],
        "assets": [{
          "id": 42, "mime_type": "image/svg+xml", "original_name": "wasm.svg",
          "content_hash": 42, "bytes": [60, 115, 118, 103, 47, 62]
        }]
      }]
    }"#;

    let decoded: ExtensionWireEditRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(
      &decoded.edits[0],
      ExtensionWireEdit::SpliceBlocks { blocks, assets, .. }
        if assets[0].id == AssetId(42)
          && matches!(&blocks[0], InputBlock::Image(image) if image.asset_id == AssetId(42))
    ));
  }
}
