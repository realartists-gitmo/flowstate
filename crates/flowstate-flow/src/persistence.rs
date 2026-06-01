use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use flowstate_collab::{
  ActorId, CollabDocument, DocumentId as CollabDocumentId, Fl0CollabDocument, FormatKind, GranularBinaryRecord, GranularOrderRecord,
  GranularSource, GranularTextRecord, NativeFileInput, decode_native_file, encode_native_file, granular_record_id_with_suffix,
};
use serde::{Deserialize, Serialize};

use crate::document::{BoxNode, Flow, FlowDocument, Node, NodeId, NodeValue, Nodes};

pub const CURRENT_SAVE_VERSION: u32 = 1;

const FL0_ROOT_FLOW_ORDER: &str = "root_flow_order";
const FL0_CHILD_ORDER_PREFIX: &str = "children";
const FL0_CONTENT_SUFFIX: &str = "content";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SaveableFlowDocument {
  pub document_id: u128,
  pub nodes: Vec<SaveableFlowNode>,
  pub version: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SaveableFlowNode {
  pub id: NodeId,
  pub node: SaveableNode,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SaveableNode {
  pub value: SaveableNodeValue,
  pub level: i32,
  pub parent: Option<NodeId>,
  pub children: Vec<NodeId>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SaveableNodeValue {
  Root,
  Flow {
    content: String,
    invert: bool,
    columns: Vec<String>,
  },
  Box {
    content: String,
    flow_id: NodeId,
    placeholder: Option<String>,
    empty: bool,
    crossed: bool,
    bold: bool,
    is_extension: bool,
  },
}

#[hotpath::measure]
pub fn get_json(document: &FlowDocument) -> Result<String> {
  let saveable = saveable_flow_document(document);
  serde_json::to_string(&saveable).context("failed to serialize .fl0 debug projection JSON")
}

#[hotpath::measure]
pub fn flow_projection_bytes(document: &FlowDocument) -> Result<Vec<u8>> {
  let saveable = saveable_flow_document(document);
  postcard::to_stdvec(&saveable).context("failed to serialize .fl0 projection")
}

#[hotpath::measure]
pub fn load_projection(bytes: &[u8]) -> Result<SaveableFlowDocument> {
  let saveable: SaveableFlowDocument = postcard::from_bytes(bytes).context("invalid .fl0 projection")?;
  if saveable.version != CURRENT_SAVE_VERSION {
    anyhow::bail!("unsupported .fl0 projection version {}", saveable.version);
  }
  Ok(saveable)
}

#[hotpath::measure]
pub fn load_nodes_from_projection(bytes: &[u8]) -> Result<Nodes> {
  nodes_from_saveable(load_projection(bytes)?)
}

#[hotpath::measure]
pub fn fl0_bytes(document: &FlowDocument) -> Result<Vec<u8>> {
  let projection_cache = flow_projection_bytes(document)?;
  let mut input = NativeFileInput::new(FormatKind::Fl0, projection_cache.clone());
  input.document_id = CollabDocumentId(uuid::Uuid::from_u128(document.document_id));
  input.source_snapshot = Some(fl0_source_snapshot_for_input(document, &input, &projection_cache)?);
  encode_native_file(input).context("failed to write .fl0 collaboration envelope")
}

#[hotpath::measure]
pub fn fl0_collab_document(document: &FlowDocument, created_by_actor: ActorId) -> Result<Fl0CollabDocument> {
  let projection_cache = flow_projection_bytes(document)?;
  let source = fl0_granular_source(document, projection_cache.clone())?;
  Fl0CollabDocument::from_granular_source(
    CollabDocumentId(uuid::Uuid::from_u128(document.document_id)),
    created_by_actor,
    &source,
    &projection_cache,
    &[],
  )
  .context("failed to create .fl0 collaboration source")
}

#[hotpath::measure]
pub fn flow_document_from_collab_source(source: &CollabDocument) -> Result<FlowDocument> {
  if source.format_kind() != FormatKind::Fl0 {
    anyhow::bail!("collaboration source is not FL0");
  }
  if let Some(granular) = source.materialize_granular_source()? {
    return flow_document_from_granular_source(&granular);
  }
  let projection = load_projection(&source.materialize_projection_cache()?)?;
  Ok(FlowDocument::from_nodes_with_document_id(
    projection.document_id,
    nodes_from_saveable(projection)?,
  ))
}

#[hotpath::measure]
pub fn load_flow_document(path: impl AsRef<Path>) -> Result<FlowDocument> {
  let path = path.as_ref();
  let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
  let decoded = decode_native_file(&bytes, FormatKind::Fl0).with_context(|| format!("invalid .fl0 envelope {}", path.display()))?;
  let projection = load_projection(&decoded.projection_cache)?;
  if decoded.manifest.document_id.0.as_u128() != projection.document_id {
    anyhow::bail!("FL0 document ID mismatch");
  }
  let document_id = projection.document_id;
  Ok(FlowDocument::from_nodes_with_document_id(document_id, nodes_from_saveable(projection)?))
}

#[hotpath::measure]
pub fn load_flow_document_or_new(path: impl AsRef<Path>) -> FlowDocument {
  load_flow_document(path).unwrap_or_else(|_| FlowDocument::new())
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
  fs::write(path, fl0_bytes(document)?).with_context(|| format!("failed to write {}", path.display()))
}

#[hotpath::measure]
fn fl0_source_snapshot_for_input(document: &FlowDocument, input: &NativeFileInput, projection_cache: &[u8]) -> Result<Vec<u8>> {
  let source = fl0_granular_source(document, projection_cache.to_vec())?;
  Fl0CollabDocument::from_granular_source(input.document_id, input.created_by_actor, &source, projection_cache, &[])
    .context("failed to create .fl0 granular source")?
    .export_snapshot()
    .context("failed to export .fl0 granular source snapshot")
}

#[hotpath::measure]
fn fl0_granular_source(document: &FlowDocument, projection_cache: Vec<u8>) -> Result<GranularSource> {
  let mut orders = Vec::new();
  if let Some(root) = document.nodes.get(crate::document::ROOT_ID) {
    orders.push(GranularOrderRecord {
      name: FL0_ROOT_FLOW_ORDER.to_string(),
      ids: root.children.clone(),
    });
  }
  let mut texts = Vec::new();
  let mut binaries = Vec::with_capacity(document.nodes.len());
  for (id, node) in &document.nodes {
    if !node.children.is_empty() {
      orders.push(GranularOrderRecord {
        name: fl0_child_order_name(id),
        ids: node.children.clone(),
      });
    }
    match &node.value {
      NodeValue::Flow(flow) => texts.push(GranularTextRecord {
        id: fl0_content_text_id(id),
        text: flow.content.clone(),
        metadata: Vec::new(),
        marks: Vec::new(),
      }),
      NodeValue::Box(box_node) => texts.push(GranularTextRecord {
        id: fl0_content_text_id(id),
        text: box_node.content.clone(),
        metadata: Vec::new(),
        marks: Vec::new(),
      }),
      NodeValue::Root => {},
    }
    binaries.push(GranularBinaryRecord {
      id: id.clone(),
      metadata: postcard::to_stdvec(&saveable_node(node)).context("failed to serialize .fl0 granular node metadata")?,
    });
  }
  Ok(GranularSource {
    metadata: projection_cache,
    orders,
    texts,
    binaries,
  })
}

#[hotpath::measure]
fn flow_document_from_granular_source(source: &GranularSource) -> Result<FlowDocument> {
  let projection = load_projection(&source.metadata)?;
  let mut nodes = if source.binaries.is_empty() {
    nodes_from_saveable(projection.clone())?
  } else {
    fl0_nodes_from_granular_binaries(source)?
  };
  apply_fl0_granular_orders(&mut nodes, source)?;
  apply_fl0_granular_texts(&mut nodes, source)?;
  Ok(FlowDocument::from_nodes_with_document_id(projection.document_id, nodes))
}

fn fl0_nodes_from_granular_binaries(source: &GranularSource) -> Result<Nodes> {
  let mut nodes = Nodes::default();
  for record in &source.binaries {
    let node = postcard::from_bytes::<SaveableNode>(&record.metadata).context("invalid .fl0 granular node metadata")?;
    if nodes
      .insert(record.id.clone(), node_from_saveable(node))
      .is_some()
    {
      anyhow::bail!("duplicate .fl0 granular node ID");
    }
  }
  Ok(nodes)
}

fn apply_fl0_granular_orders(nodes: &mut Nodes, source: &GranularSource) -> Result<()> {
  for order in &source.orders {
    if order.name == FL0_ROOT_FLOW_ORDER {
      let root = nodes
        .get_mut(crate::document::ROOT_ID)
        .context("missing .fl0 granular root node")?;
      root.children.clone_from(&order.ids);
      for child in &order.ids {
        nodes
          .get_mut(child)
          .with_context(|| format!("missing .fl0 granular root child {child}"))?
          .parent = Some(crate::document::ROOT_ID.to_string());
      }
      continue;
    }
    let Some(parent_id) = fl0_parent_id_from_order_name(&order.name) else {
      anyhow::bail!("unknown .fl0 granular order {}", order.name);
    };
    let parent = nodes
      .get_mut(parent_id)
      .with_context(|| format!("missing .fl0 granular parent {parent_id}"))?;
    parent.children.clone_from(&order.ids);
    for child in &order.ids {
      nodes
        .get_mut(child)
        .with_context(|| format!("missing .fl0 granular child {child}"))?
        .parent = Some(parent_id.to_string());
    }
  }
  Ok(())
}

fn apply_fl0_granular_texts(nodes: &mut Nodes, source: &GranularSource) -> Result<()> {
  for text in &source.texts {
    let Some(node_id) = text.id.strip_suffix(":content") else {
      anyhow::bail!("unknown .fl0 granular text {}", text.id);
    };
    let node = nodes
      .get_mut(node_id)
      .with_context(|| format!("missing .fl0 granular text node {node_id}"))?;
    match &mut node.value {
      NodeValue::Flow(flow) => flow.content.clone_from(&text.text),
      NodeValue::Box(box_node) => box_node.content.clone_from(&text.text),
      NodeValue::Root => anyhow::bail!("root node cannot have .fl0 granular text"),
    }
  }
  Ok(())
}

fn fl0_child_order_name(id: &str) -> String {
  granular_record_id_with_suffix(granular_record_id_with_suffix(FL0_CHILD_ORDER_PREFIX, id), "order")
}

fn fl0_parent_id_from_order_name(name: &str) -> Option<&str> {
  name.strip_prefix("children:")?.strip_suffix(":order")
}

fn fl0_content_text_id(id: &str) -> String {
  granular_record_id_with_suffix(id, FL0_CONTENT_SUFFIX)
}

#[hotpath::measure]
fn saveable_flow_document(document: &FlowDocument) -> SaveableFlowDocument {
  let mut nodes = document
    .nodes
    .iter()
    .map(|(id, node)| SaveableFlowNode {
      id: id.clone(),
      node: saveable_node(node),
    })
    .collect::<Vec<_>>();
  nodes.sort_by(|left, right| left.id.cmp(&right.id));
  SaveableFlowDocument {
    document_id: document.document_id,
    nodes,
    version: CURRENT_SAVE_VERSION,
  }
}

#[hotpath::measure]
fn nodes_from_saveable(saveable: SaveableFlowDocument) -> Result<Nodes> {
  let mut nodes = Nodes::default();
  for entry in saveable.nodes {
    if nodes
      .insert(entry.id, node_from_saveable(entry.node))
      .is_some()
    {
      anyhow::bail!("duplicate .fl0 node ID in projection");
    }
  }
  Ok(nodes)
}

#[hotpath::measure]
fn saveable_node(node: &Node) -> SaveableNode {
  SaveableNode {
    value: saveable_node_value(&node.value),
    level: node.level,
    parent: node.parent.clone(),
    children: node.children.clone(),
  }
}

#[hotpath::measure]
fn saveable_node_value(value: &NodeValue) -> SaveableNodeValue {
  match value {
    NodeValue::Root => SaveableNodeValue::Root,
    NodeValue::Flow(flow) => SaveableNodeValue::Flow {
      content: flow.content.clone(),
      invert: flow.invert,
      columns: flow.columns.clone(),
    },
    NodeValue::Box(box_node) => SaveableNodeValue::Box {
      content: box_node.content.clone(),
      flow_id: box_node.flow_id.clone(),
      placeholder: box_node.placeholder.clone(),
      empty: box_node.empty,
      crossed: box_node.crossed,
      bold: box_node.bold,
      is_extension: box_node.is_extension,
    },
  }
}

#[hotpath::measure]
fn node_from_saveable(node: SaveableNode) -> Node {
  Node {
    value: node_value_from_saveable(node.value),
    level: node.level,
    parent: node.parent,
    children: node.children,
  }
}

#[hotpath::measure]
fn node_value_from_saveable(value: SaveableNodeValue) -> NodeValue {
  match value {
    SaveableNodeValue::Root => NodeValue::Root,
    SaveableNodeValue::Flow { content, invert, columns } => NodeValue::Flow(Flow { content, invert, columns }),
    SaveableNodeValue::Box {
      content,
      flow_id,
      placeholder,
      empty,
      crossed,
      bold,
      is_extension,
    } => NodeValue::Box(BoxNode {
      content,
      flow_id,
      placeholder,
      empty,
      crossed,
      bold,
      is_extension,
    }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::document::{ROOT_ID, new_box_id, new_flow_id};

  #[test]
  #[hotpath::measure]
  fn projection_round_trips() {
    let document = FlowDocument::new();
    let bytes = flow_projection_bytes(&document).unwrap();
    let nodes = load_nodes_from_projection(&bytes).unwrap();
    assert!(nodes.contains_key(ROOT_ID));
  }

  #[test]
  #[hotpath::measure]
  fn native_envelope_round_trips() {
    let document = FlowDocument::new();
    let bytes = fl0_bytes(&document).unwrap();
    let path = std::env::temp_dir().join(format!("flowstate-test-{}.fl0", uuid::Uuid::new_v4()));
    fs::write(&path, bytes).unwrap();
    let loaded = load_flow_document(&path).unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(loaded.document_id, document.document_id);
    assert_eq!(loaded.nodes, document.nodes);
  }

  #[test]
  #[hotpath::measure]
  fn collab_source_materializes_projection() {
    let document = FlowDocument::new();
    let source = fl0_collab_document(&document, ActorId::new()).unwrap();
    let materialized = flow_document_from_collab_source(source.inner()).unwrap();
    assert_eq!(materialized.document_id, document.document_id);
    assert_eq!(materialized.nodes, document.nodes);
  }

  #[test]
  #[hotpath::measure]
  fn granular_box_text_update_materializes_projection() {
    let flow_id = new_flow_id();
    let box_id = new_box_id();
    let mut document = FlowDocument::new();
    document.root_mut().children.push(flow_id.clone());
    document.nodes.insert(
      flow_id.clone(),
      Node {
        value: NodeValue::Flow(Flow {
          content: "flow".to_string(),
          invert: false,
          columns: Vec::new(),
        }),
        level: 0,
        parent: Some(ROOT_ID.to_string()),
        children: vec![box_id.clone()],
      },
    );
    document.nodes.insert(
      box_id.clone(),
      Node {
        value: NodeValue::Box(BoxNode {
          content: "box".to_string(),
          flow_id,
          placeholder: None,
          empty: false,
          crossed: false,
          bold: false,
          is_extension: false,
        }),
        level: 1,
        parent: document.root().children.first().cloned(),
        children: Vec::new(),
      },
    );

    let source = fl0_collab_document(&document, ActorId::new()).unwrap();
    source
      .inner()
      .insert_granular_text_utf8(flowstate_collab::Role::Owner, &fl0_content_text_id(&box_id), 0, "SYNC ")
      .unwrap();
    let materialized = flow_document_from_collab_source(source.inner()).unwrap();
    assert!(
      materialized
        .box_node(&box_id)
        .unwrap()
        .content
        .starts_with("SYNC ")
    );
  }
}
