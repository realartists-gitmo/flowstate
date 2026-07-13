use serde::Deserialize;
use serde_json::json;

use super::host_error;
use crate::bindings::flowstate::extension::host;

#[derive(Deserialize)]
struct Snapshot {
  generation: u64,
  selection: Selection,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Selection {
  TableCell {
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
  },
  #[serde(other)]
  Other,
}

pub fn replace_selected_cell() -> Result<(), String> {
  let source = host::snapshot().map_err(host_error)?;
  let snapshot: Snapshot = serde_json::from_str(&source).map_err(|error| error.to_string())?;
  let Selection::TableCell { block_ix, row_ix, cell_ix } = snapshot.selection else {
    return Err("select a table cell before running this action".into());
  };
  let request = json!({ "expected_generation": snapshot.generation, "edits": [{
    "kind": "replace_table_cell", "block_ix": block_ix,
    "row_ix": row_ix, "cell_ix": cell_ix,
    "blocks": [{ "Paragraph": {
      "style": "Normal", "runs": [{
        "text": "Replaced table cell", "styles": {
          "semantic": "Plain", "direct_underline": true,
          "strikethrough": false, "highlight": null
        }
      }]
    }}]
  }]});
  host::apply_edits(&request.to_string()).map_err(host_error)?;
  Ok(())
}
