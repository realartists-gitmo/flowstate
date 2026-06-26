use serde::{Deserialize, Serialize};

use crate::document::{BoxNode, Flow, FlowDocument, Node, NodeId, NodeValue, Nodes, ROOT_ID, constrain_index, new_box_id, new_flow_id};
use crate::styles::DebateStyleFlow;

pub type ActionBundle = Vec<Action>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "tag")]
pub enum Action {
  #[serde(rename = "add")]
  Add {
    parent: NodeId,
    id: NodeId,
    index: usize,
    value: NodeValue,
  },
  #[serde(rename = "delete")]
  Delete { id: NodeId },
  #[serde(rename = "update")]
  Update {
    id: NodeId,
    #[serde(rename = "newValue")]
    new_value: NodeValue,
  },
  #[serde(rename = "move")]
  Move {
    id: NodeId,
    #[serde(rename = "newIndex")]
    new_index: usize,
  },
  #[serde(rename = "replace")]
  Replace {
    #[serde(rename = "newNodes")]
    new_nodes: Nodes,
  },
  #[serde(rename = "identity")]
  Identity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
  pub actions: ActionBundle,
  pub owner: NodeId,
  pub focus: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatKind {
  Bold,
  Crossed,
}

#[hotpath::measure_all]
impl FlowDocument {
  pub fn apply_action(&mut self, action: Action) -> Action {
    match action {
      Action::Add { parent, id, index, value } => {
        if self.nodes.contains_key(&id) {
          return Action::Identity;
        }
        let Some(parent_node) = self.nodes.get(&parent) else {
          return Action::Identity;
        };
        let child = Node {
          value,
          level: parent_node.level + 1,
          parent: Some(parent.clone()),
          children: Vec::new(),
        };
        self.nodes.insert(id.clone(), child);
        let Some(parent_node) = self.nodes.get_mut(&parent) else {
          return Action::Identity;
        };
        let index = constrain_index(index, parent_node.children.len());
        parent_node.children.insert(index, id.clone());
        Action::Delete { id }
      },
      Action::Delete { id } => {
        let inverse_replace = self
          .nodes
          .get(&id)
          .is_some_and(|node| !node.children.is_empty())
          .then(|| Action::Replace {
            new_nodes: self.nodes.clone(),
          });
        let Some(node) = self.nodes.get(&id).cloned() else {
          return Action::Identity;
        };
        let Some(parent_id) = node.parent.clone() else {
          return Action::Identity;
        };
        let Some(parent) = self.nodes.get_mut(&parent_id) else {
          return Action::Identity;
        };
        let Some(index) = parent.children.iter().position(|child| child == &id) else {
          return Action::Identity;
        };
        parent.children.remove(index);
        remove_subtree(&mut self.nodes, &id);
        inverse_replace.unwrap_or(Action::Add {
          parent: parent_id,
          id,
          index,
          value: node.value,
        })
      },
      Action::Update { id, new_value } => {
        let Some(node) = self.nodes.get_mut(&id) else {
          return Action::Identity;
        };
        let inverse = Action::Update {
          id,
          new_value: node.value.clone(),
        };
        node.value = new_value;
        inverse
      },
      Action::Move { id, new_index } => {
        let Some(node) = self.nodes.get(&id) else {
          return Action::Identity;
        };
        let Some(parent_id) = node.parent.clone() else {
          return Action::Identity;
        };
        let Some(parent) = self.nodes.get_mut(&parent_id) else {
          return Action::Identity;
        };
        let Some(index) = parent.children.iter().position(|child| child == &id) else {
          return Action::Identity;
        };
        parent.children.remove(index);
        let index_after_remove = constrain_index(new_index, parent.children.len());
        parent.children.insert(index_after_remove, id.clone());
        Action::Move { id, new_index: index }
      },
      Action::Replace { new_nodes } => {
        let inverse = Action::Replace {
          new_nodes: self.nodes.clone(),
        };
        self.nodes = new_nodes;
        inverse
      },
      Action::Identity => Action::Identity,
    }
  }

  pub fn apply_action_bundle(&mut self, actions: ActionBundle) -> ActionBundle {
    let mut inverse = Vec::with_capacity(actions.len());
    for action in actions {
      inverse.push(self.apply_action(action));
    }
    inverse.reverse();
    inverse
  }
}

/// Removes `id` and all of its descendants from `nodes`.
///
/// Removing each node before recursing into its (former) children keeps the
/// walk cycle-safe: a malformed child edge pointing back at an already-removed
/// ancestor short-circuits on the `remove` miss instead of recursing forever.
#[hotpath::measure]
fn remove_subtree(nodes: &mut Nodes, id: &NodeId) {
  if let Some(node) = nodes.remove(id) {
    for child in &node.children {
      remove_subtree(nodes, child);
    }
  }
}

#[hotpath::measure]
pub fn new_box_action(parent: NodeId, parent_flow_id: NodeId, index: usize, placeholder: Option<String>) -> Action {
  Action::Add {
    parent,
    id: new_box_id(),
    index,
    value: NodeValue::Box(BoxNode {
      content: String::new(),
      flow_id: parent_flow_id,
      placeholder,
      empty: false,
      crossed: false,
      bold: false,
      is_extension: false,
    }),
  }
}

#[hotpath::measure]
pub const fn new_extension_action(parent: NodeId, parent_flow_id: NodeId, id: NodeId) -> Action {
  Action::Add {
    parent,
    id,
    index: 0,
    value: NodeValue::Box(BoxNode {
      content: String::new(),
      flow_id: parent_flow_id,
      placeholder: None,
      empty: false,
      crossed: false,
      bold: false,
      is_extension: true,
    }),
  }
}

#[hotpath::measure]
#[must_use]
pub const fn new_update_action(id: NodeId, new_value: NodeValue) -> Action {
  Action::Update { id, new_value }
}

#[hotpath::measure]
#[must_use]
pub fn add_new_box_actions(document: &FlowDocument, parent: NodeId, index: usize, placeholder: Option<String>) -> Option<CommandResult> {
  let flow_id = document.parent_flow_id(&parent)?;
  let action = new_box_action(parent, flow_id.clone(), index, placeholder);
  let focus = match &action {
    Action::Add { id, .. } => Some(id.clone()),
    _ => None,
  };
  Some(CommandResult {
    actions: vec![action],
    owner: flow_id,
    focus,
  })
}

#[hotpath::measure]
#[must_use]
pub fn add_new_extension_actions(document: &FlowDocument, parent: NodeId) -> Option<CommandResult> {
  let flow_id = document.parent_flow_id(&parent)?;
  let extension_id = new_box_id();
  let child = new_box_action(extension_id.clone(), flow_id.clone(), 0, None);
  let focus = match &child {
    Action::Add { id, .. } => Some(id.clone()),
    _ => None,
  };
  Some(CommandResult {
    actions: vec![new_extension_action(parent, flow_id.clone(), extension_id), child],
    owner: flow_id,
    focus,
  })
}

#[hotpath::measure]
#[must_use]
pub fn add_new_flow_actions(index: usize, style: &DebateStyleFlow, switch_speakers: bool) -> CommandResult {
  let starter_boxes = style.starter_boxes.as_deref();
  let columns = if switch_speakers {
    style
      .columns_switch
      .clone()
      .unwrap_or_else(|| style.columns.clone())
  } else {
    style.columns.clone()
  };
  let flow_id = new_flow_id();
  let mut actions = vec![Action::Add {
    parent: ROOT_ID.to_owned(),
    id: flow_id.clone(),
    index,
    value: NodeValue::Flow(Flow {
      content: String::new(),
      invert: style.invert,
      columns,
    }),
  }];

  if let Some(starter_boxes) = starter_boxes {
    for (index, placeholder) in starter_boxes.iter().enumerate() {
      actions.push(new_box_action(flow_id.clone(), flow_id.clone(), index, Some((*placeholder).clone())));
    }
  } else {
    actions.push(new_box_action(flow_id.clone(), flow_id.clone(), 0, None));
  }

  CommandResult {
    actions,
    owner: ROOT_ID.to_owned(),
    focus: Some(flow_id),
  }
}

#[hotpath::measure]
#[must_use]
pub fn toggle_box_format_actions(document: &FlowDocument, id: NodeId, format: FormatKind) -> Option<CommandResult> {
  let mut box_node = document.box_node(&id)?.clone();
  match format {
    FormatKind::Bold => box_node.bold = !box_node.bold,
    FormatKind::Crossed => box_node.crossed = !box_node.crossed,
  }
  let owner = document.parent_flow_id(&id)?;
  Some(CommandResult {
    actions: vec![new_update_action(id.clone(), NodeValue::Box(box_node))],
    owner,
    focus: Some(id),
  })
}

#[hotpath::measure]
#[must_use]
pub fn move_node_actions(document: &FlowDocument, id: NodeId, new_index: usize) -> Option<CommandResult> {
  let owner = match document.node(&id)?.value {
    NodeValue::Flow(_) => ROOT_ID.to_owned(),
    NodeValue::Box(_) => document.parent_flow_id(&id)?,
    NodeValue::Root => return None,
  };
  Some(CommandResult {
    actions: vec![Action::Move { id: id.clone(), new_index }],
    owner,
    focus: Some(id),
  })
}

#[hotpath::measure]
#[must_use]
pub fn delete_node_actions(document: &FlowDocument, id: NodeId) -> Option<CommandResult> {
  let owner = match document.node(&id)?.value {
    NodeValue::Flow(_) => ROOT_ID.to_owned(),
    NodeValue::Box(_) => document.parent_flow_id(&id)?,
    NodeValue::Root => return None,
  };
  let mut actions = Vec::new();
  collect_delete_actions(document, &id, &mut actions)?;
  Some(CommandResult { actions, owner, focus: None })
}

#[hotpath::measure]
fn collect_delete_actions(document: &FlowDocument, id: &str, actions: &mut ActionBundle) -> Option<()> {
  let node = document.node(id)?;
  for child in node.children.iter().rev() {
    collect_delete_actions(document, child, actions)?;
  }
  actions.push(Action::Delete { id: id.to_owned() });
  Some(())
}

#[hotpath::measure]
#[must_use]
pub fn add_new_empty_actions(document: &FlowDocument, flow_id: NodeId, level: usize) -> Option<CommandResult> {
  document.flow(&flow_id)?;
  let mut actions = Vec::with_capacity(level + 1);
  let mut parent_id = flow_id.clone();
  for _ in 0..level {
    let id = new_box_id();
    actions.push(Action::Add {
      parent: parent_id,
      id: id.clone(),
      index: 0,
      value: NodeValue::Box(BoxNode {
        content: String::new(),
        flow_id: flow_id.clone(),
        placeholder: None,
        empty: true,
        crossed: false,
        bold: false,
        is_extension: false,
      }),
    });
    parent_id = id;
  }
  let final_box = new_box_action(parent_id, flow_id.clone(), 0, None);
  let focus = match &final_box {
    Action::Add { id, .. } => Some(id.clone()),
    _ => None,
  };
  actions.push(final_box);
  Some(CommandResult {
    actions,
    owner: flow_id,
    focus,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::history::HistoryHolder;
  use crate::styles::{DebateStyleKey, debate_style_templates};

  #[test]
  #[hotpath::measure]
  fn add_update_delete_and_undo_round_trip() {
    let mut document = FlowDocument::new();
    let template = debate_style_templates(DebateStyleKey::Policy, false).remove(0);
    let add_flow = add_new_flow_actions(0, &template, false);
    let flow_id = add_flow.focus.clone().unwrap();
    let inverse = document.apply_action_bundle(add_flow.actions);
    let mut history = HistoryHolder::new();
    history.add(add_flow.owner, inverse, None, Some(flow_id.clone()));

    let first_box = document.node(&flow_id).unwrap().children[0].clone();
    let mut box_node = document.box_node(&first_box).unwrap().clone();
    box_node.content = "plan flaw".to_string();
    let inverse = document.apply_action_bundle(vec![new_update_action(first_box.clone(), NodeValue::Box(box_node))]);
    history.add(flow_id.clone(), inverse, Some(first_box.clone()), Some(first_box.clone()));
    assert_eq!(document.box_node(&first_box).unwrap().content, "plan flaw");

    history.undo(&flow_id, &mut document);
    assert_eq!(document.box_node(&first_box).unwrap().content, "");

    history.redo(&flow_id, &mut document);
    assert_eq!(document.box_node(&first_box).unwrap().content, "plan flaw");

    let delete = delete_node_actions(&document, first_box.clone()).unwrap();
    let inverse = document.apply_action_bundle(delete.actions);
    history.add(flow_id.clone(), inverse, Some(first_box.clone()), Some(flow_id.clone()));
    assert!(document.box_node(&first_box).is_none());

    history.undo(&flow_id, &mut document);
    assert_eq!(document.box_node(&first_box).unwrap().content, "plan flaw");
  }

  #[test]
  #[hotpath::measure]
  fn move_flow_actions_reorders_root_children_and_undoes() {
    let mut document = FlowDocument::new();
    let template = debate_style_templates(DebateStyleKey::Policy, false).remove(0);
    let first = add_new_flow_actions(0, &template, false);
    let first_id = first.focus.clone().unwrap();
    document.apply_action_bundle(first.actions);
    let second = add_new_flow_actions(1, &template, false);
    let second_id = second.focus.clone().unwrap();
    document.apply_action_bundle(second.actions);

    let command = move_node_actions(&document, first_id.clone(), 1).unwrap();
    let inverse = document.apply_action_bundle(command.actions);
    assert_eq!(document.flow_ids(), &[second_id.clone(), first_id.clone()]);

    document.apply_action_bundle(inverse);
    assert_eq!(document.flow_ids(), &[first_id, second_id]);
  }
}
