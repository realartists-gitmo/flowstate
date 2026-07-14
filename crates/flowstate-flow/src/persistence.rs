use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::actions::{Action, new_box_action};
use crate::document::{BoxNode, Flow, FlowDocument, NodeId, NodeValue, Nodes, ROOT_ID, new_box_id, new_flow_id};

pub const CURRENT_SAVE_VERSION: u32 = 1;

const MAX_FL0_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
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
  let version = data
    .get("version")
    .and_then(Value::as_u64)
    .and_then(|version| u32::try_from(version).ok())
    .unwrap_or(0);
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
  let metadata = fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
  if metadata.len() > MAX_FL0_BYTES {
    bail!(
      "refusing to read {}: file too large ({} bytes > {} bytes)",
      path.display(),
      metadata.len(),
      MAX_FL0_BYTES
    );
  }
  let text = fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
  let value: Value = serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
  let nodes = load_nodes(value)?;
  Ok(FlowDocument::from_nodes(nodes))
}

#[hotpath::measure]
pub fn load_flow_document_or_new(path: impl AsRef<Path>) -> FlowDocument {
  let path = path.as_ref();
  match load_flow_document(path) {
    Ok(document) => document,
    Err(error) => {
      if path.exists() {
        let quarantine = path.with_file_name(format!(
          "{}.corrupt.{}",
          path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("flowstate.fl0"),
          Uuid::new_v4()
        ));
        if let Err(rename_error) = fs::rename(path, &quarantine) {
          tracing::warn!(path = %path.display(), quarantine = %quarantine.display(), error = %format_args!("{error:#}"), rename_error = %rename_error, "failed to quarantine existing .fl0 document; using a new empty document");
        } else {
          tracing::warn!(path = %path.display(), quarantine = %quarantine.display(), error = %format_args!("{error:#}"), "failed to load existing .fl0 document; quarantined it and using a new empty document");
        }
      }
      FlowDocument::new()
    },
  }
}

#[hotpath::measure]
pub fn save_flow_document(path: impl AsRef<Path>, document: &FlowDocument) -> Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
  }
  let text = get_json(document)?;
  let temp_path = path.with_file_name(format!(
    ".{}.{}.tmp",
    path
      .file_name()
      .and_then(|name| name.to_str())
      .unwrap_or("flowstate"),
    Uuid::new_v4()
  ));
  fs::write(&temp_path, text).with_context(|| format!("failed to write temporary {}", temp_path.display()))?;
  fs::rename(&temp_path, path).with_context(|| format!("failed to atomically replace {} with {}", path.display(), temp_path.display()))?;
  Ok(())
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
  children: Vec<Self>,
  #[serde(default)]
  empty: bool,
  #[serde(default)]
  placeholder: Option<String>,
  #[serde(default)]
  crossed: bool,
}

#[hotpath::measure]
fn upgrade_0_1(saved: Value) -> Result<SaveableFlowDocument> {
  #[derive(Debug)]
  enum LegacyNode {
    Flow(OldFlow),
    Box(OldBox),
  }

  let old_flows: Vec<OldFlow> = serde_json::from_value(saved).context("invalid legacy debate-flow document")?;
  let mut document = FlowDocument::new();
  let mut pending = Vec::new();
  for (index, old_flow) in old_flows.into_iter().enumerate().rev() {
    pending.push((ROOT_ID.to_owned(), index, LegacyNode::Flow(old_flow)));
  }

  let mut processed_nodes = 0usize;
  const MAX_LEGACY_NODES: usize = 100_000;
  while let Some((parent_id, index, node)) = pending.pop() {
    processed_nodes += 1;
    if processed_nodes > MAX_LEGACY_NODES {
      bail!("legacy .fl0 document is too deeply nested or large to upgrade safely");
    }

    match node {
      LegacyNode::Flow(old_flow) => {
        let flow_id = new_flow_id();
        let add_flow = Action::Add {
          parent: parent_id,
          id: flow_id.clone(),
          index,
          value: NodeValue::Flow(Flow {
            content: old_flow.content,
            invert: old_flow.invert,
            columns: old_flow.columns,
          }),
        };
        document.apply_action(add_flow);
        for (box_index, child) in old_flow.children.into_iter().enumerate().rev() {
          pending.push((flow_id.clone(), box_index, LegacyNode::Box(child)));
        }
      },
      LegacyNode::Box(old_box) => {
        let id = new_box_id();
        let flow_id = document
          .parent_flow_id(&parent_id)
          .unwrap_or_else(|| parent_id.clone());
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
        for (child_index, child) in old_box.children.into_iter().enumerate().rev() {
          pending.push((id.clone(), child_index, LegacyNode::Box(child)));
        }
      },
    }
  }

  Ok(SaveableFlowDocument {
    nodes: document.nodes,
    version: CURRENT_SAVE_VERSION,
  })
}

#[allow(dead_code, reason = "Legacy save migration helper is retained for older persisted flow documents.")]
#[hotpath::measure]
fn _new_empty_box(parent_id: NodeId, flow_id: NodeId, index: usize) -> Action {
  new_box_action(parent_id, flow_id, index, None)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{path::PathBuf, time::SystemTime};

  fn test_dir(name: &str) -> PathBuf {
    let unique = format!(
      "{name}-{}-{}",
      std::process::id(),
      SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    fs::create_dir_all(&path).unwrap();
    path
  }

  #[test]
  fn rejects_oversized_documents_before_reading() {
    let dir = test_dir("flowstate-fl0-size");
    let path = dir.join("oversized.fl0");
    fs::write(&path, vec![b'{'; (MAX_FL0_BYTES as usize) + 1]).unwrap();

    let error = load_flow_document(&path).unwrap_err();
    assert!(error.to_string().contains("file too large"));
  }

  #[test]
  fn quarantines_bad_existing_document_on_fallback() {
    let dir = test_dir("flowstate-fl0-corrupt");
    let path = dir.join("broken.fl0");
    fs::write(&path, b"{not json").unwrap();

    let document = load_flow_document_or_new(&path);
    assert!(document.flow_ids().is_empty());
    assert!(!path.exists());
    assert!(fs::read_dir(&dir).unwrap().any(|entry| {
      entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("broken.fl0.corrupt.")
    }));
  }

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
