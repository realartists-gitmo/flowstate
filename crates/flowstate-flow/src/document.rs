use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ROOT_ID: &str = "root";

pub type NodeId = String;
pub type Nodes = FxHashMap<NodeId, Node>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Node {
  pub value: NodeValue,
  pub level: i32,
  pub parent: Option<NodeId>,
  #[serde(default)]
  pub children: Vec<NodeId>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "tag")]
pub enum NodeValue {
  #[serde(rename = "root")]
  Root,
  #[serde(rename = "flow")]
  Flow(Flow),
  #[serde(rename = "box")]
  Box(BoxNode),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Flow {
  #[serde(default)]
  pub content: String,
  #[serde(default)]
  pub invert: bool,
  #[serde(default)]
  pub columns: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BoxNode {
  #[serde(default)]
  pub content: String,
  #[serde(rename = "flowId")]
  pub flow_id: NodeId,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub placeholder: Option<String>,
  #[serde(default, skip_serializing_if = "is_false")]
  pub empty: bool,
  #[serde(default, skip_serializing_if = "is_false")]
  pub crossed: bool,
  #[serde(default, skip_serializing_if = "is_false")]
  pub bold: bool,
  #[serde(rename = "isExtension", default, skip_serializing_if = "is_false")]
  pub is_extension: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowDocument {
  pub nodes: Nodes,
}

#[hotpath::measure_all]
impl Default for FlowDocument {
  fn default() -> Self {
    Self::new()
  }
}

#[hotpath::measure_all]
impl FlowDocument {
  pub fn new() -> Self {
    let mut nodes = Nodes::default();
    nodes.insert(
      ROOT_ID.to_string(),
      Node {
        value: NodeValue::Root,
        level: -1,
        parent: None,
        children: Vec::new(),
      },
    );
    Self { nodes }
  }

  pub fn from_nodes(mut nodes: Nodes) -> Self {
    nodes.entry(ROOT_ID.to_string()).or_insert_with(|| Node {
      value: NodeValue::Root,
      level: -1,
      parent: None,
      children: Vec::new(),
    });
    Self { nodes }
  }

  pub fn root(&self) -> &Node {
    self
      .nodes
      .get(ROOT_ID)
      .expect("FlowDocument invariant violated: root node missing")
  }

  pub fn root_mut(&mut self) -> &mut Node {
    self
      .nodes
      .get_mut(ROOT_ID)
      .expect("FlowDocument invariant violated: root node missing")
  }

  pub fn node(&self, id: impl AsRef<str>) -> Option<&Node> {
    self.nodes.get(id.as_ref())
  }

  pub fn node_mut(&mut self, id: impl AsRef<str>) -> Option<&mut Node> {
    self.nodes.get_mut(id.as_ref())
  }

  pub fn flow(&self, id: impl AsRef<str>) -> Option<&Flow> {
    match &self.node(id)?.value {
      NodeValue::Flow(flow) => Some(flow),
      _ => None,
    }
  }

  pub fn flow_mut(&mut self, id: impl AsRef<str>) -> Option<&mut Flow> {
    match &mut self.node_mut(id)?.value {
      NodeValue::Flow(flow) => Some(flow),
      _ => None,
    }
  }

  pub fn box_node(&self, id: impl AsRef<str>) -> Option<&BoxNode> {
    match &self.node(id)?.value {
      NodeValue::Box(box_node) => Some(box_node),
      _ => None,
    }
  }

  pub fn box_node_mut(&mut self, id: impl AsRef<str>) -> Option<&mut BoxNode> {
    match &mut self.node_mut(id)?.value {
      NodeValue::Box(box_node) => Some(box_node),
      _ => None,
    }
  }

  pub fn flow_ids(&self) -> &[NodeId] {
    &self.root().children
  }

  pub fn parent_flow_id(&self, id: impl AsRef<str>) -> Option<NodeId> {
    let id = id.as_ref();
    match &self.node(id)?.value {
      NodeValue::Flow(_) => Some(id.to_string()),
      NodeValue::Box(box_node) => Some(box_node.flow_id.clone()),
      NodeValue::Root => None,
    }
  }

  pub fn check_box_id(&self, id: impl AsRef<str>) -> Option<NodeId> {
    let id = id.as_ref();
    self.box_node(id).map(|_| id.to_string())
  }

  pub fn child_index(&self, parent_id: impl AsRef<str>, child_id: impl AsRef<str>) -> Option<usize> {
    self
      .node(parent_id)?
      .children
      .iter()
      .position(|id| id == child_id.as_ref())
  }

  pub fn adjacent_box(&self, id: impl AsRef<str>, direction: Direction) -> Option<NodeId> {
    let id = id.as_ref();
    let node = self.node(id)?;
    if !matches!(node.value, NodeValue::Box(_)) {
      return None;
    }
    let parent_id = node.parent.as_ref()?;
    let parent = self.node(parent_id)?;
    let index = parent.children.iter().position(|child| child == id)?;
    let next_index = match direction {
      Direction::Up => index.checked_sub(1),
      Direction::Down => Some(index + 1).filter(|ix| *ix < parent.children.len()),
    };
    if let Some(next_index) = next_index {
      return parent.children.get(next_index).cloned();
    }

    let parent_box_id = self.check_box_id(parent_id)?;
    let mut adjacent_parent = self.adjacent_box(parent_box_id, direction)?;
    loop {
      let adjacent_node = self.node(&adjacent_parent)?;
      if !adjacent_node.children.is_empty() {
        let child_index = match direction {
          Direction::Up => adjacent_node.children.len() - 1,
          Direction::Down => 0,
        };
        return adjacent_node.children.get(child_index).cloned();
      }
      adjacent_parent = self.adjacent_box(adjacent_parent, direction)?;
    }
  }

  pub fn is_worth_saving(&self, ignore_first_empty_flow: bool) -> bool {
    let root_children = &self.root().children;
    if root_children.is_empty() {
      return false;
    }
    if root_children.len() != 1 || !ignore_first_empty_flow {
      return true;
    }

    let Some(flow) = self.node(&root_children[0]) else {
      return false;
    };
    let NodeValue::Flow(flow_value) = &flow.value else {
      return false;
    };
    if !flow_value.content.is_empty() {
      return true;
    }
    if flow.children.is_empty() {
      return false;
    }
    if flow.children.len() > 1 {
      return true;
    }
    let Some(first_child) = self.node(&flow.children[0]) else {
      return false;
    };
    match &first_child.value {
      NodeValue::Box(box_node) => !(box_node.content.is_empty() && first_child.children.is_empty()),
      _ => true,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
  Up,
  Down,
}

#[hotpath::measure]
pub fn new_node_id() -> NodeId {
  Uuid::new_v4().to_string()
}

#[hotpath::measure]
pub fn new_box_id() -> NodeId {
  new_node_id()
}

#[hotpath::measure]
pub fn new_flow_id() -> NodeId {
  new_node_id()
}

#[hotpath::measure]
pub fn constrain_index(index: usize, len: usize) -> usize {
  index.min(len)
}

#[hotpath::measure]
fn is_false(value: &bool) -> bool {
  !*value
}
