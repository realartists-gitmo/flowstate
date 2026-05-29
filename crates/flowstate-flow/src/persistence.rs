use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actions::{Action, new_box_action};
use crate::document::{BoxNode, Flow, FlowDocument, NodeId, NodeValue, Nodes, ROOT_ID, new_box_id, new_flow_id};

pub const CURRENT_SAVE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SaveableFlowDocument {
  pub nodes: Nodes,
  pub version: u32,
}

#[hotpath::measure]
pub fn get_json(document: &FlowDocument) -> Result<String> {
  let saveable = SaveableFlowDocument {
    nodes: document.nodes.clone(),
    version: CURRENT_SAVE_VERSION,
  };
  serde_json::to_string(&saveable).context("failed to serialize .fl0 document")
}

#[hotpath::measure]
pub fn load_nodes(data: Value) -> Result<Nodes> {
  let version = data.get("version").and_then(Value::as_u64).unwrap_or(0) as u32;
  if version == CURRENT_SAVE_VERSION {
    let saveable: SaveableFlowDocument = serde_json::from_value(data).context("invalid .fl0 document")?;
    return Ok(saveable.nodes);
  }
  if version == 0 {
    return upgrade_0_1(data).map(|saveable| saveable.nodes);
  }
  bail!("unsupported .fl0 save version {version}");
}

#[hotpath::measure]
pub fn load_flow_document(path: impl AsRef<Path>) -> Result<FlowDocument> {
  let path = path.as_ref();
  let text = fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
  let value: Value = serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
  let nodes = load_nodes(value)?;
  Ok(FlowDocument::from_nodes(nodes))
}

#[hotpath::measure]
pub fn load_flow_document_or_new(path: impl AsRef<Path>) -> FlowDocument {
  load_flow_document(path).unwrap_or_else(|_| FlowDocument::new())
}

#[hotpath::measure]
pub fn save_flow_document(path: impl AsRef<Path>, document: &FlowDocument) -> Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
  }
  let text = get_json(document)?;
  fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

#[derive(Clone, Debug, Deserialize)]
struct OldFlow {
  #[serde(default)]
  content: String,
  #[serde(default)]
  columns: Vec<String>,
  #[serde(default)]
  invert: bool,
  #[serde(default)]
  children: Vec<OldBox>,
}

#[derive(Clone, Debug, Deserialize)]
struct OldBox {
  #[serde(default)]
  content: String,
  #[serde(default)]
  children: Vec<OldBox>,
  #[serde(default)]
  empty: bool,
  #[serde(default)]
  placeholder: Option<String>,
  #[serde(default)]
  crossed: bool,
}

#[hotpath::measure]
fn upgrade_0_1(saved: Value) -> Result<SaveableFlowDocument> {
  let old_flows: Vec<OldFlow> = serde_json::from_value(saved).context("invalid legacy debate-flow document")?;
  let mut document = FlowDocument::new();
  for (index, old_flow) in old_flows.into_iter().enumerate() {
    let flow_id = new_flow_id();
    let add_flow = Action::Add {
      parent: ROOT_ID.to_string(),
      id: flow_id.clone(),
      index,
      value: NodeValue::Flow(Flow {
        content: old_flow.content,
        invert: old_flow.invert,
        columns: old_flow.columns,
      }),
    };
    document.apply_action(add_flow);
    for (box_index, old_box) in old_flow.children.into_iter().enumerate() {
      upgrade_0_1_add_boxes_rec(&mut document, flow_id.clone(), flow_id.clone(), old_box, box_index);
    }
  }
  Ok(SaveableFlowDocument {
    nodes: document.nodes,
    version: CURRENT_SAVE_VERSION,
  })
}

#[hotpath::measure]
fn upgrade_0_1_add_boxes_rec(document: &mut FlowDocument, flow_id: NodeId, parent_id: NodeId, old_box: OldBox, index: usize) {
  let id = new_box_id();
  let add = Action::Add {
    parent: parent_id,
    id: id.clone(),
    index,
    value: NodeValue::Box(BoxNode {
      content: old_box.content,
      flow_id: flow_id.clone(),
      placeholder: old_box.placeholder,
      empty: old_box.empty,
      crossed: old_box.crossed,
      bold: false,
      is_extension: false,
    }),
  };
  document.apply_action(add);
  for (child_index, child) in old_box.children.into_iter().enumerate() {
    upgrade_0_1_add_boxes_rec(document, flow_id.clone(), id.clone(), child, child_index);
  }
}

#[allow(dead_code)]
#[hotpath::measure]
fn _new_empty_box(parent_id: NodeId, flow_id: NodeId, index: usize) -> Action {
  new_box_action(parent_id, flow_id, index, None)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  #[hotpath::measure]
  fn loads_current_save_shape() {
    let document = FlowDocument::new();
    let json = get_json(&document).unwrap();
    let value: Value = serde_json::from_str(&json).unwrap();
    let nodes = load_nodes(value).unwrap();
    assert!(nodes.contains_key(ROOT_ID));
  }

  #[test]
  #[hotpath::measure]
  fn upgrades_legacy_flow_array() {
    let legacy = serde_json::json!([
      {
        "content": "aff",
        "level": 0,
        "columns": ["1AC", "1NC"],
        "invert": false,
        "focus": false,
        "index": 0,
        "lastFocus": [],
        "children": [
          {
            "content": "advantage",
            "children": [],
            "index": 0,
            "level": 1,
            "focus": false,
            "placeholder": "type here",
            "crossed": true
          }
        ],
        "history": { "index": -1, "data": [], "lastFocus": null },
        "id": 1
      }
    ]);
    let document = FlowDocument::from_nodes(load_nodes(legacy).unwrap());
    let flow_id = document.flow_ids()[0].clone();
    let box_id = document.node(&flow_id).unwrap().children[0].clone();
    assert_eq!(document.flow(&flow_id).unwrap().content, "aff");
    assert_eq!(document.box_node(&box_id).unwrap().content, "advantage");
    assert!(document.box_node(&box_id).unwrap().crossed);
  }
}
