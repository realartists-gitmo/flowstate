use std::{
  collections::hash_map::DefaultHasher,
  hash::{Hash, Hasher},
  path::PathBuf,
};

use flowstate_flow::{
  Action, CommandResult, DebateStyleFlow, DebateStyleKey, FlowDocument, FormatKind, HistoryHolder, Node, NodeId, NodeValue, ROOT_ID,
  add_new_box_actions, add_new_empty_actions, add_new_extension_actions, add_new_flow_actions, all_debate_style_templates,
  debate_style_templates, delete_node_actions, get_json, load_flow_document_or_new, move_node_actions, save_flow_document,
  toggle_box_format_actions,
};
use gpui::{
  App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
  ParentElement, Render, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable, h_flex, v_flex};
use rustc_hash::{FxHashMap, FxHashSet};

const COLUMN_WIDTH: f32 = 172.0;

pub struct FlowEditor {
  document: FlowDocument,
  path: Option<PathBuf>,
  dirty: bool,
  history: HistoryHolder,
  focus_handle: FocusHandle,
  selected_style: DebateStyleKey,
  ld_toc_circuit: bool,
  switch_speakers: bool,
  selected_flow_id: Option<NodeId>,
  focus_id: Option<NodeId>,
  last_focus_ids: FxHashMap<NodeId, NodeId>,
  folded: FxHashSet<NodeId>,
  box_inputs: FxHashMap<NodeId, Entity<InputState>>,
  input_subscriptions: Vec<Subscription>,
  syncing_inputs: FxHashSet<NodeId>,
  pending_edit: Option<PendingEdit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowOutlineItem {
  pub id: NodeId,
  pub label: String,
  pub index: usize,
  pub selected: bool,
  pub invert: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowCommandState {
  pub has_flow: bool,
  pub has_selected_box: bool,
  pub can_format: bool,
  pub can_fold: bool,
  pub selected_bold: bool,
  pub selected_crossed: bool,
  pub selected_folded: bool,
  pub can_undo: bool,
  pub can_redo: bool,
}

#[derive(Clone, Debug)]
struct PendingEdit {
  id: NodeId,
  owner: NodeId,
  before_focus: Option<NodeId>,
  after_focus: Option<NodeId>,
  old_value: NodeValue,
}

impl FlowEditor {
  pub fn new_with_path(document: FlowDocument, path: Option<PathBuf>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
    let selected_flow_id = document.flow_ids().first().cloned();
    let focus_id = selected_flow_id.clone();

    Self {
      document,
      path,
      dirty: false,
      history: HistoryHolder::new(),
      focus_handle: cx.focus_handle(),
      selected_style: DebateStyleKey::Policy,
      ld_toc_circuit: false,
      switch_speakers: false,
      selected_flow_id,
      focus_id,
      last_focus_ids: FxHashMap::default(),
      folded: FxHashSet::default(),
      box_inputs: FxHashMap::default(),
      input_subscriptions: Vec::new(),
      syncing_inputs: FxHashSet::default(),
      pending_edit: None,
    }
  }

  pub fn blank(window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self::new_with_path(FlowDocument::new(), None, window, cx)
  }

  pub fn load_or_new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self::new_with_path(load_flow_document_or_new(&path), Some(path), window, cx)
  }

  pub fn document_path(&self) -> Option<&PathBuf> {
    self.path.as_ref()
  }

  pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.path = Some(path);
    cx.notify();
  }

  pub fn has_unsaved_changes(&self) -> bool {
    self.dirty || self.pending_edit.is_some()
  }

  pub fn save(&mut self, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    self.resolve_pending_edit(cx);
    let Some(path) = self.path.clone() else {
      return cx
        .background_executor()
        .spawn(async { Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "flow has no save path")) });
    };
    self.save_to_path(path, cx)
  }

  pub fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    self.resolve_pending_edit(cx);
    self.path = Some(path.clone());
    self.save_to_path(path, cx)
  }

  fn save_to_path(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    let saved_document = self.document.clone();
    cx.spawn(async move |editor, cx| {
      let write_result = cx
        .background_executor()
        .spawn({
          let saved_document = saved_document.clone();
          async move { save_flow_document(&path, &saved_document).map_err(std::io::Error::other) }
        })
        .await;
      match write_result {
        Ok(()) => {
          let _ = editor.update(cx, |editor, cx| {
            if editor.pending_edit.is_none() && editor.document == saved_document {
              editor.dirty = false;
            }
            cx.notify();
          });
          Ok(())
        },
        Err(error) => Err(error),
      }
    })
  }

  pub fn discard_recovery_file(&mut self) {}

  pub fn json_snapshot(&mut self, cx: &mut Context<Self>) -> Option<String> {
    self.resolve_pending_edit(cx);
    get_json(&self.document).ok()
  }

  pub fn resolve_pending(&mut self, cx: &mut Context<Self>) {
    self.resolve_pending_edit(cx);
  }

  pub fn selected_flow_id(&self) -> Option<&str> {
    self.selected_flow_id.as_deref()
  }

  pub fn selected_flow_title(&self) -> String {
    self
      .selected_flow_id
      .as_ref()
      .and_then(|id| self.document.flow(id))
      .map(|flow| flow.content.clone())
      .unwrap_or_default()
  }

  pub fn set_selected_flow_title(&mut self, value: String, cx: &mut Context<Self>) {
    let Some(flow_id) = self.selected_flow_id.clone() else {
      return;
    };
    self.on_title_change(&flow_id, value, cx);
  }

  pub fn outline_items(&self) -> Vec<FlowOutlineItem> {
    self
      .document
      .flow_ids()
      .iter()
      .enumerate()
      .map(|(index, id)| {
        let flow = self.document.flow(id);
        let label = flow
          .map(|flow| {
            if flow.content.is_empty() {
              format!("Flow {}", index + 1)
            } else {
              flow.content.clone()
            }
          })
          .unwrap_or_else(|| "Missing flow".to_string());
        FlowOutlineItem {
          id: id.clone(),
          label,
          index,
          selected: self.selected_flow_id.as_deref() == Some(id.as_str()),
          invert: flow.is_some_and(|flow| flow.invert),
        }
      })
      .collect()
  }

  pub fn command_state(&self) -> FlowCommandState {
    let selected_box = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id));
    let can_format = selected_box
      .as_ref()
      .and_then(|id| self.document.box_node(id))
      .is_some_and(|box_node| !box_node.is_extension);
    let can_fold = selected_box
      .as_ref()
      .and_then(|id| self.document.node(id))
      .is_some_and(|node| !node.children.is_empty());
    let selected_box_node = selected_box
      .as_ref()
      .and_then(|id| self.document.box_node(id));
    let owner = self.selected_flow_id.clone().unwrap_or_else(|| ROOT_ID.to_string());
    FlowCommandState {
      has_flow: self.selected_flow_id.is_some(),
      has_selected_box: selected_box.is_some(),
      can_format,
      can_fold,
      selected_bold: selected_box_node.is_some_and(|box_node| box_node.bold),
      selected_crossed: selected_box_node.is_some_and(|box_node| box_node.crossed),
      selected_folded: selected_box.as_ref().is_some_and(|id| self.folded.contains(id)),
      can_undo: self.history.can_undo(&owner),
      can_redo: self.history.can_redo(&owner),
    }
  }

  pub fn selected_style(&self) -> DebateStyleKey {
    self.selected_style
  }

  pub fn selected_style_label(&self) -> &'static str {
    all_debate_style_templates()
      .into_iter()
      .find(|template| template.key == self.selected_style)
      .map(|template| template.label)
      .unwrap_or("Policy")
  }

  pub fn set_selected_style(&mut self, style: DebateStyleKey, cx: &mut Context<Self>) {
    if self.selected_style != style {
      self.selected_style = style;
      self.switch_speakers = false;
      cx.notify();
    }
  }

  pub fn ld_toc_circuit(&self) -> bool {
    self.ld_toc_circuit
  }

  pub fn toggle_ld_toc_circuit(&mut self, cx: &mut Context<Self>) {
    self.ld_toc_circuit = !self.ld_toc_circuit;
    self.switch_speakers = false;
    cx.notify();
  }

  pub fn switch_speakers(&self) -> bool {
    self.switch_speakers
  }

  pub fn toggle_switch_speakers(&mut self, cx: &mut Context<Self>) {
    if self.has_switchable_templates() {
      self.switch_speakers = !self.switch_speakers;
      cx.notify();
    }
  }

  pub fn templates(&self) -> Vec<DebateStyleFlow> {
    let mut templates = debate_style_templates(self.selected_style, self.ld_toc_circuit);
    if self.switch_speakers {
      let mut flipped = Vec::with_capacity(templates.len());
      for pair in templates.chunks(2) {
        if pair.len() == 2 {
          flipped.push(pair[1].clone());
          flipped.push(pair[0].clone());
        } else if let Some(flow) = pair.first() {
          flipped.push(flow.clone());
        }
      }
      templates = flipped;
    }
    templates
  }

  pub fn has_switchable_templates(&self) -> bool {
    debate_style_templates(self.selected_style, self.ld_toc_circuit)
      .iter()
      .any(|template| template.columns_switch.is_some())
  }

  pub fn select_flow(&mut self, flow_id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
    self.resolve_pending_edit(cx);
    self.selected_flow_id = Some(flow_id.clone());
    let focus = self
      .last_focus_ids
      .get(&flow_id)
      .cloned()
      .unwrap_or(flow_id);
    self.set_focus(Some(focus), window, cx);
  }

  fn set_focus(&mut self, focus_id: Option<NodeId>, window: &mut Window, cx: &mut Context<Self>) {
    if self.focus_id != focus_id {
      self.resolve_pending_edit(cx);
    }
    self.focus_id = focus_id.clone();
    if let Some(focus_id) = focus_id {
      if let Some(flow_id) = self.document.parent_flow_id(&focus_id) {
        self.last_focus_ids.insert(flow_id, focus_id.clone());
      }
      self.focus_input(&focus_id, window, cx);
    }
    cx.notify();
  }

  fn focus_input(&self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(input) = self.box_inputs.get(id) {
      input.update(cx, |input, cx| input.focus(window, cx));
    }
  }

  fn perform_command(&mut self, command: CommandResult, before_focus: Option<NodeId>, window: &mut Window, cx: &mut Context<Self>) {
    self.resolve_pending_edit(cx);
    let after_focus = command.focus.clone().or(before_focus.clone());
    let inverse = self.document.apply_action_bundle(command.actions);
    self.history.add(command.owner, inverse, before_focus, after_focus.clone());
    self.dirty = true;
    if let Some(focus) = after_focus {
      if self.document.node(&focus).is_some() {
        self.set_focus(Some(focus), window, cx);
      } else {
        cx.notify();
      }
    } else {
      cx.notify();
    }
  }

  pub fn add_flow(&mut self, template: DebateStyleFlow, window: &mut Window, cx: &mut Context<Self>) {
    let command = add_new_flow_actions(self.document.flow_ids().len(), &template, self.switch_speakers);
    let new_flow_id = command.focus.clone();
    self.perform_command(command, self.focus_id.clone(), window, cx);
    if let Some(flow_id) = new_flow_id {
      self.selected_flow_id = Some(flow_id.clone());
      self.set_focus(Some(flow_id), window, cx);
    }
  }

  pub fn delete_selected_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(flow_id) = self.selected_flow_id.clone() else {
      return;
    };
    let old_index = self
      .document
      .flow_ids()
      .iter()
      .position(|id| id == &flow_id)
      .unwrap_or(0);
    let Some(command) = delete_node_actions(&self.document, flow_id) else {
      return;
    };
    self.perform_command(command, self.focus_id.clone(), window, cx);
    let next_flow = if self.document.flow_ids().is_empty() {
      None
    } else if old_index == 0 {
      self.document.flow_ids().first().cloned()
    } else {
      self.document.flow_ids().get(old_index - 1).cloned()
    };
    self.selected_flow_id = next_flow.clone();
    self.set_focus(next_flow, window, cx);
  }

  pub fn move_flow_to_index(&mut self, flow_id: NodeId, new_index: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(current_index) = self
      .document
      .flow_ids()
      .iter()
      .position(|id| id == &flow_id)
    else {
      return;
    };
    let target_index = new_index.min(self.document.flow_ids().len().saturating_sub(1));
    if current_index == target_index {
      self.select_flow(flow_id, window, cx);
      return;
    }
    let Some(command) = move_node_actions(&self.document, flow_id.clone(), target_index) else {
      return;
    };
    self.perform_command(command, self.focus_id.clone(), window, cx);
    self.selected_flow_id = Some(flow_id.clone());
    self.set_focus(Some(flow_id), window, cx);
  }

  fn add_empty_at_column(&mut self, level: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(flow_id) = self.selected_flow_id.clone() else {
      return;
    };
    if let Some(command) = add_new_empty_actions(&self.document, flow_id, level) {
      self.perform_command(command, self.focus_id.clone(), window, cx);
    }
  }

  pub fn add_child_to_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(target_id) = self.focus_id.clone() else {
      return;
    };
    self.add_child(target_id, window, cx);
  }

  fn add_child(&mut self, target_id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
    let Some(node) = self.document.node(&target_id).cloned() else {
      return;
    };
    let Some(flow_id) = self.document.parent_flow_id(&target_id) else {
      return;
    };
    let Some(flow) = self.document.flow(&flow_id) else {
      return;
    };
    if node.level >= flow.columns.len() as i32 {
      self.set_focus(Some(target_id), window, cx);
      return;
    }
    let mut index = 0;
    if let Some(first_child) = node.children.first()
      && self
        .document
        .box_node(first_child)
        .is_some_and(|box_node| box_node.is_extension)
    {
      index = 1;
    }
    if let Some(command) = add_new_box_actions(&self.document, target_id, index, None) {
      self.perform_command(command, self.focus_id.clone(), window, cx);
    }
  }

  pub fn add_sibling_to_focus(&mut self, direction: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(target_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    let Some(node) = self.document.node(&target_id).cloned() else {
      return;
    };
    let Some(parent_id) = node.parent.clone() else {
      return;
    };
    let index = self
      .document
      .child_index(&parent_id, &target_id)
      .map(|index| index + direction)
      .unwrap_or(direction);
    if let Some(command) = add_new_box_actions(&self.document, parent_id, index, None) {
      self.perform_command(command, Some(target_id), window, cx);
    }
  }

  pub fn extend_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(target_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    if self
      .document
      .node(&target_id)
      .and_then(|node| node.children.first())
      .and_then(|first| self.document.box_node(first))
      .is_some_and(|box_node| box_node.is_extension)
    {
      return;
    }
    if self
      .document
      .box_node(&target_id)
      .is_some_and(|box_node| box_node.is_extension)
    {
      return;
    }
    let Some(flow_id) = self.document.parent_flow_id(&target_id) else {
      return;
    };
    let Some(flow) = self.document.flow(&flow_id) else {
      return;
    };
    let Some(node) = self.document.node(&target_id) else {
      return;
    };
    if node.level >= flow.columns.len() as i32 - 1 {
      return;
    }
    if let Some(command) = add_new_extension_actions(&self.document, target_id.clone()) {
      self.perform_command(command, Some(target_id), window, cx);
    }
  }

  pub fn delete_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(target_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    let Some(node) = self.document.node(&target_id).cloned() else {
      return;
    };
    let Some(parent_id) = node.parent.clone() else {
      return;
    };
    let Some(parent) = self.document.node(&parent_id).cloned() else {
      return;
    };
    if parent.value == NodeValue::Root || (matches!(parent.value, NodeValue::Flow(_)) && parent.children.len() <= 1) {
      self.set_focus(Some(target_id), window, cx);
      return;
    }
    let target_index = parent
      .children
      .iter()
      .position(|child| child == &target_id)
      .unwrap_or(0);
    let next_focus = if target_index > 0 {
      parent.children.get(target_index - 1).cloned()
    } else {
      Some(parent_id.clone()).filter(|id| id != ROOT_ID)
    };
    if let Some(command) = delete_node_actions(&self.document, target_id.clone()) {
      self.perform_command(command, Some(target_id), window, cx);
      self.set_focus(next_focus, window, cx);
    }
  }

  fn delete_empty_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(target_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    if !self
      .document
      .box_node(&target_id)
      .is_some_and(|box_node| box_node.content.is_empty())
    {
      return;
    }
    let Some(node) = self.document.node(&target_id).cloned() else {
      return;
    };
    let Some(parent_id) = node.parent.clone() else {
      return;
    };
    let Some(parent) = self.document.node(&parent_id).cloned() else {
      return;
    };
    if parent.value == NodeValue::Root || (matches!(parent.value, NodeValue::Flow(_)) && parent.children.len() <= 1) {
      return;
    }
    let target_index = parent
      .children
      .iter()
      .position(|child| child == &target_id)
      .unwrap_or(0);
    let next_focus = if target_index > 0 {
      parent.children.get(target_index - 1).cloned()
    } else {
      Some(parent_id.clone()).filter(|id| id != ROOT_ID)
    };
    if let Some(command) = delete_node_actions(&self.document, target_id.clone()) {
      self.perform_command(command, Some(target_id), window, cx);
      self.set_focus(next_focus, window, cx);
    }
  }

  pub fn toggle_format_focus(&mut self, format: FormatKind, window: &mut Window, cx: &mut Context<Self>) {
    let Some(target_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    if self
      .document
      .box_node(&target_id)
      .is_some_and(|box_node| box_node.is_extension)
    {
      return;
    }
    if let Some(command) = toggle_box_format_actions(&self.document, target_id.clone(), format) {
      self.perform_command(command, Some(target_id), window, cx);
    }
  }

  pub fn toggle_fold_focus(&mut self, cx: &mut Context<Self>) {
    let Some(target_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    if self
      .document
      .node(&target_id)
      .is_none_or(|node| node.children.is_empty())
    {
      return;
    }
    if !self.folded.insert(target_id.clone()) {
      self.folded.remove(&target_id);
    }
    cx.notify();
  }

  pub fn undo_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.resolve_pending_edit(cx);
    let Some(owner) = self.selected_flow_id.clone() else {
      return;
    };
    if let Some(focus) = self.history.undo(owner, &mut self.document) {
      self.dirty = true;
      self.set_focus(focus, window, cx);
    }
  }

  pub fn redo_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.resolve_pending_edit(cx);
    let Some(owner) = self.selected_flow_id.clone() else {
      return;
    };
    if let Some(focus) = self.history.redo(owner, &mut self.document) {
      self.dirty = true;
      self.set_focus(focus, window, cx);
    }
  }

  fn begin_or_update_edit(&mut self, id: NodeId, value: NodeValue, cx: &mut Context<Self>) {
    if self.syncing_inputs.contains(&id) {
      return;
    }
    let Some(old_node) = self.document.node(&id).cloned() else {
      return;
    };
    if old_node.value == value {
      return;
    }
    let owner = match &value {
      NodeValue::Flow(_) => id.clone(),
      NodeValue::Box(_) => self.document.parent_flow_id(&id).unwrap_or_else(|| ROOT_ID.to_string()),
      NodeValue::Root => ROOT_ID.to_string(),
    };
    if self
      .pending_edit
      .as_ref()
      .is_none_or(|pending| pending.id != id)
    {
      self.resolve_pending_edit(cx);
      self.pending_edit = Some(PendingEdit {
        id: id.clone(),
        owner,
        before_focus: self.focus_id.clone(),
        after_focus: self.focus_id.clone(),
        old_value: old_node.value.clone(),
      });
    } else if let Some(pending) = &mut self.pending_edit {
      pending.after_focus = self.focus_id.clone();
    }
    if let Some(node) = self.document.node_mut(&id) {
      node.value = value;
      self.dirty = true;
      cx.notify();
    }
  }

  fn resolve_pending_edit(&mut self, cx: &mut Context<Self>) {
    let Some(pending) = self.pending_edit.take() else {
      return;
    };
    let Some(current) = self.document.node(&pending.id).map(|node| node.value.clone()) else {
      return;
    };
    if current == pending.old_value {
      return;
    }
    self.history.add(
      pending.owner,
      vec![Action::Update {
        id: pending.id,
        new_value: pending.old_value,
      }],
      pending.before_focus,
      pending.after_focus,
    );
    cx.notify();
  }

  fn on_title_change(&mut self, flow_id: &str, value: String, cx: &mut Context<Self>) {
    let Some(flow) = self.document.flow(flow_id).cloned() else {
      return;
    };
    let mut next = flow;
    next.content = sanitize_input_value(value);
    self.begin_or_update_edit(flow_id.to_string(), NodeValue::Flow(next), cx);
  }

  fn on_box_change(&mut self, box_id: &str, value: String, cx: &mut Context<Self>) {
    let Some(box_node) = self.document.box_node(box_id).cloned() else {
      return;
    };
    let mut next = box_node;
    next.content = sanitize_input_value(value);
    self.begin_or_update_edit(box_id.to_string(), NodeValue::Box(next), cx);
  }

  fn focus_first_child(&mut self, flow_id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
    let focus = self
      .document
      .node(&flow_id)
      .and_then(|flow| {
        flow
          .children
          .iter()
          .find(|child_id| {
            self
              .document
              .box_node(child_id)
              .is_some_and(|box_node| !box_node.empty)
          })
          .cloned()
      })
      .unwrap_or(flow_id);
    self.set_focus(Some(focus), window, cx);
  }

  fn ensure_box_input(&mut self, box_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
    if let Some(input) = self.box_inputs.get(box_id) {
      return input.clone();
    }
    let box_node = self.document.box_node(box_id).cloned();
    let value = box_node
      .as_ref()
      .map(|box_node| box_node.content.clone())
      .unwrap_or_default();
    let placeholder = box_node
      .and_then(|box_node| box_node.placeholder)
      .unwrap_or_else(|| "type here".to_string());
    let input = cx.new(|cx| {
      InputState::new(window, cx)
        .placeholder(placeholder)
        .default_value(value)
    });
    let id = box_id.to_string();
    let subscription = cx.subscribe_in(&input, window, move |editor, input, event: &InputEvent, window, cx| match event {
      InputEvent::Change => {
        let value = input.read(cx).value().to_string();
        editor.on_box_change(&id, value, cx);
      },
      InputEvent::Focus => {
        if editor.focus_id.as_deref() != Some(&id) {
          editor.resolve_pending_edit(cx);
          editor.focus_id = Some(id.clone());
          if let Some(flow_id) = editor.document.parent_flow_id(&id) {
            editor.last_focus_ids.insert(flow_id, id.clone());
          }
          cx.notify();
        }
      },
      InputEvent::Blur => {
        editor.resolve_pending_edit(cx);
      },
      InputEvent::BackspaceEmpty | InputEvent::DeleteEmpty => {
        if editor.focus_id.as_deref() == Some(&id) {
          editor.delete_empty_focus(window, cx);
        }
      },
      InputEvent::PressEnter { secondary: false } => {
        if editor.focus_id.as_deref() == Some(&id) {
          editor.add_sibling_to_focus(1, window, cx);
        }
      },
      InputEvent::PressEnter { secondary: true } => {
        if editor.focus_id.as_deref() == Some(&id) {
          editor.add_child_to_focus(window, cx);
        }
      },
    });
    self.input_subscriptions.push(subscription);
    self.box_inputs.insert(box_id.to_string(), input.clone());
    input
  }

  fn sync_input(&mut self, id: &str, input: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut Context<Self>) {
    if input.read(cx).value().as_ref() == value {
      return;
    }
    self.syncing_inputs.insert(id.to_string());
    input.update(cx, |input, cx| input.set_value(value.to_string(), window, cx));
    self.syncing_inputs.remove(id);
  }

  fn focused_box_input_is_focused(&self, window: &Window, cx: &App) -> bool {
    self
      .focus_id
      .as_ref()
      .and_then(|id| self.box_inputs.get(id))
      .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    let key = event.keystroke.key.as_str();
    let modifiers = &event.keystroke.modifiers;
    let command_control = modifiers.platform || modifiers.control;
    let mut handled = true;

    if matches!(key, "enter" | "backspace" | "delete") && self.focused_box_input_is_focused(window, cx) {
      return;
    }

    match (command_control, modifiers.shift, modifiers.alt, key) {
      (true, true, _, "z") => self.redo_selected(window, cx),
      (true, false, _, "z") => self.undo_selected(window, cx),
      (true, false, _, "y") => self.redo_selected(window, cx),
      (true, false, _, "n") => {
        if let Some(template) = self.templates().first().cloned() {
          self.add_flow(template, window, cx);
        }
      },
      (true, true, _, "n") => {
        if let Some(template) = self.templates().get(1).cloned() {
          self.add_flow(template, window, cx);
        }
      },
      (true, false, _, "b") => self.toggle_format_focus(FormatKind::Bold, window, cx),
      (true, true, _, "x") => self.toggle_format_focus(FormatKind::Crossed, window, cx),
      (true, false, _, "e") => self.extend_focus(window, cx),
      (true, false, _, "backspace") | (true, false, _, "delete") => self.delete_focus(window, cx),
      (false, false, false, "enter") => {
        if let Some(flow_id) = self
          .focus_id
          .clone()
          .filter(|id| self.document.flow(id).is_some())
        {
          self.focus_first_child(flow_id, window, cx);
        } else {
          self.add_sibling_to_focus(1, window, cx);
        }
      },
      (false, true, _, "enter") => self.add_child_to_focus(window, cx),
      (false, false, true, "enter") => self.add_sibling_to_focus(0, window, cx),
      (false, false, _, "tab") => self.focus_sibling(1, window, cx),
      (false, true, _, "tab") => self.focus_sibling_reverse(window, cx),
      (false, false, _, "left") => self.focus_parent(window, cx),
      (false, false, _, "right") => self.focus_first_child_of_focus(window, cx),
      (false, false, _, "up") => self.focus_adjacent(-1, window, cx),
      (false, false, _, "down") => self.focus_adjacent(1, window, cx),
      (false, false, _, "backspace") => {
        let can_delete_empty = self
          .focus_id
          .as_ref()
          .and_then(|id| self.document.box_node(id))
          .is_some_and(|box_node| box_node.content.is_empty());
        if can_delete_empty {
          self.delete_focus(window, cx);
        } else {
          handled = false;
        }
      },
      (false, false, _, "l") if modifiers.control => self.toggle_fold_focus(cx),
      _ => handled = false,
    }

    if handled {
      window.prevent_default();
      cx.stop_propagation();
    }
  }

  fn focus_sibling(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(focus_id) = self
      .focus_id
      .as_ref()
      .and_then(|id| self.document.check_box_id(id))
    else {
      return;
    };
    let Some(node) = self.document.node(&focus_id) else {
      return;
    };
    let Some(parent_id) = node.parent.clone() else {
      return;
    };
    let Some(parent) = self.document.node(&parent_id) else {
      return;
    };
    let Some(index) = parent.children.iter().position(|id| id == &focus_id) else {
      return;
    };
    let next_index = if direction < 0 {
      index.checked_sub(direction.unsigned_abs())
    } else {
      Some(index + direction as usize).filter(|ix| *ix < parent.children.len())
    };
    if let Some(next_focus) = next_index.and_then(|ix| parent.children.get(ix)).cloned() {
      self.set_focus(Some(next_focus), window, cx);
    } else if parent_id != ROOT_ID {
      self.set_focus(Some(parent_id), window, cx);
    }
  }

  fn focus_sibling_reverse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.focus_sibling(-1, window, cx);
  }

  fn focus_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(focus_id) = self.focus_id.clone() else {
      return;
    };
    let Some(parent_id) = self
      .document
      .node(&focus_id)
      .and_then(|node| node.parent.clone())
      .filter(|id| id != ROOT_ID)
    else {
      return;
    };
    self.set_focus(Some(parent_id), window, cx);
  }

  fn focus_first_child_of_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(focus_id) = self.focus_id.clone() else {
      return;
    };
    let Some(child_id) = self
      .document
      .node(&focus_id)
      .and_then(|node| node.children.first())
      .cloned()
    else {
      return;
    };
    self.set_focus(Some(child_id), window, cx);
  }

  fn focus_adjacent(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(flow_id) = self.selected_flow_id.clone() else {
      return;
    };
    let mut ids = Vec::new();
    self.collect_visible_boxes(&flow_id, &mut ids);
    let Some(focus_id) = self.focus_id.clone() else {
      return;
    };
    let Some(index) = ids.iter().position(|id| id == &focus_id) else {
      return;
    };
    let next = if direction < 0 {
      index
        .checked_sub(direction.unsigned_abs())
        .and_then(|ix| ids.get(ix))
        .cloned()
    } else {
      ids.get(index + direction as usize).cloned()
    };
    if let Some(next) = next {
      self.set_focus(Some(next), window, cx);
    }
  }

  fn collect_visible_boxes(&self, id: &str, ids: &mut Vec<NodeId>) {
    let Some(node) = self.document.node(id) else {
      return;
    };
    if matches!(node.value, NodeValue::Box(_)) {
      ids.push(id.to_string());
    }
    if self.folded.contains(id) {
      return;
    }
    for child in &node.children {
      self.collect_visible_boxes(child, ids);
    }
  }

  fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let Some(flow_id) = self.selected_flow_id.clone() else {
      return self.render_empty_flow_state(cx).into_any_element();
    };
    if self.document.flow(&flow_id).is_none() {
      return self.render_empty_flow_state(cx).into_any_element();
    }
    v_flex()
      .flex_1()
      .size_full()
      .overflow_hidden()
      .bg(cx.theme().background)
      .child(self.render_flow_board(flow_id, window, cx))
      .into_any_element()
  }

  fn render_empty_flow_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .flex_1()
      .size_full()
      .flex()
      .items_center()
      .justify_center()
      .text_sm()
      .text_color(cx.theme().muted_foreground)
      .child("Choose a debate style and add a flow")
  }

  fn render_flow_board(&mut self, flow_id: NodeId, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let Some(flow) = self.document.flow(&flow_id).cloned() else {
      return self.render_empty_flow_state(cx).into_any_element();
    };
    let width = px(COLUMN_WIDTH * flow.columns.len().max(1) as f32);
    let children = self
      .document
      .node(&flow_id)
      .map(|node| node.children.clone())
      .unwrap_or_default();
    v_flex()
      .flex_1()
      .overflow_scrollbar()
      .p_4()
      .child(
        v_flex()
          .w(width)
          .min_w(width)
          .gap_2()
          .child(
            h_flex()
              .bg(cx.theme().background)
              .children(flow.columns.iter().enumerate().map(|(ix, column)| {
                let colors = flow_column_colors(column, cx);
                h_flex()
                  .w(px(COLUMN_WIDTH))
                  .h(px(34.0))
                  .items_center()
                  .justify_center()
                  .gap_1()
                  .border_1()
                  .border_color(colors.border)
                  .bg(colors.background)
                  .text_color(colors.foreground)
                  .text_sm()
                  .font_weight(FontWeight::MEDIUM)
                  .child(div().truncate().child(column.clone()))
                  .child(
                    Button::new(("flow-add-empty-column", ix))
                      .icon(IconName::Plus)
                      .xsmall()
                      .ghost()
                      .tooltip("Add argument in column")
                      .on_click(cx.listener(move |editor, _, window, cx| {
                        editor.add_empty_at_column(ix, window, cx);
                      })),
                  )
                  .into_any_element()
              })),
          )
          .child(
            v_flex()
              .gap_2()
              .children(children.into_iter().map(|child| self.render_box_tree(child, &flow.columns, window, cx))),
          ),
      )
      .into_any_element()
  }

  fn render_box_tree(&mut self, box_id: NodeId, columns: &[String], window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
    let Some(node) = self.document.node(&box_id).cloned() else {
      return div().into_any_element();
    };
    if self
      .document
      .box_node(&box_id)
      .is_some_and(|box_node| box_node.empty)
    {
      return h_flex()
        .items_start()
        .child(div().w(px(COLUMN_WIDTH)).h(px(10.0)).flex_none())
        .child(
          v_flex()
            .gap_2()
            .children(node.children.into_iter().map(|child| self.render_box_tree(child, columns, window, cx))),
        )
        .into_any_element();
    }
    let children = node.children.clone();
    h_flex()
      .items_start()
      .child(self.render_box_cell(box_id.clone(), node, columns, window, cx))
      .when(!self.folded.contains(&box_id), |this| {
        this.child(
          v_flex()
            .gap_2()
            .children(children.into_iter().map(|child| self.render_box_tree(child, columns, window, cx))),
        )
      })
      .when(self.folded.contains(&box_id), |this| {
        this.child(
          Button::new(("flow-unfold", stable_element_id(&box_id)))
            .icon(IconName::PanelRightOpen)
            .xsmall()
            .ghost()
            .tooltip("Unfold")
            .on_click(cx.listener(move |editor, _, _, cx| {
              editor.folded.remove(&box_id);
              cx.notify();
            })),
        )
      })
      .into_any_element()
  }

  fn render_box_cell(
    &mut self,
    box_id: NodeId,
    node: Node,
    columns: &[String],
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> impl IntoElement {
    let Some(box_node) = self.document.box_node(&box_id).cloned() else {
      return div().into_any_element();
    };
    let input = self.ensure_box_input(&box_id, window, cx);
    self.sync_input(&box_id, &input, &box_node.content, window, cx);
    if self.focus_id.as_deref() == Some(box_id.as_str()) && !input.focus_handle(cx).is_focused(window) {
      input.update(cx, |input, cx| input.focus(window, cx));
    }

    let selected = self.focus_id.as_deref() == Some(box_id.as_str());
    let colors = columns
      .get(flow_box_column_index(node.level))
      .map(|column| flow_column_colors(column, cx))
      .unwrap_or_else(|| affirmative_flow_colors(cx));
    let weak = cx.theme().muted_foreground;
    let id_for_click = box_id.clone();
    v_flex()
      .id(("flow-box-cell", stable_element_id(&box_id)))
      .w(px(COLUMN_WIDTH))
      .min_h(px(44.0))
      .p_1()
      .border_1()
      .border_color(if selected { colors.selected_border } else { colors.border })
      .bg(if selected { colors.selected_background } else { colors.background })
      .text_color(if box_node.crossed { weak } else { colors.foreground })
      .when(box_node.bold, |this| this.font_weight(FontWeight::BOLD))
      .when(box_node.crossed, |this| this.line_through())
      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .on_click(cx.listener(move |editor, _, window, cx| {
        editor.set_focus(Some(id_for_click.clone()), window, cx);
      }))
      .child(
        h_flex()
          .w_full()
          .items_start()
          .gap_1()
          .when(box_node.is_extension, |this| {
            this.child(Icon::new(IconName::ArrowRight).xsmall().text_color(colors.foreground))
          })
          .child(
            div()
              .flex_1()
              .child(
                Input::new(&input)
                  .appearance(false)
                  .bordered(false)
                  .focus_bordered(false)
                  .text_color(if box_node.crossed { weak } else { colors.foreground })
                  .placeholder_color(colors.foreground.opacity(0.62))
                  .w_full(),
              ),
          ),
      )
      .into_any_element()
  }
}

impl EventEmitter<()> for FlowEditor {}

impl Focusable for FlowEditor {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowEditor {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size_full()
      .overflow_hidden()
      .track_focus(&self.focus_handle)
      .on_key_down(cx.listener(Self::on_key_down))
      .child(self.render_main(window, cx))
  }
}

fn sanitize_input_value(value: String) -> String {
  value.replace(['\r', '\n'], " ")
}

#[derive(Clone, Copy, Debug)]
pub struct FlowSideColors {
  pub background: Hsla,
  pub border: Hsla,
  pub foreground: Hsla,
  pub selected_background: Hsla,
  pub selected_border: Hsla,
}

pub fn affirmative_flow_colors(cx: &mut App) -> FlowSideColors {
  FlowSideColors {
    background: cx.theme().primary_hover,
    border: cx.theme().primary,
    foreground: cx.theme().primary_foreground,
    selected_background: cx.theme().primary_active,
    selected_border: cx.theme().primary_active,
  }
}

pub fn flow_column_colors(column: &str, cx: &mut App) -> FlowSideColors {
  let affirmative = affirmative_flow_colors(cx);
  match debate_column_side(column) {
    DebateColumnSide::Negative => affirmative.inverse(),
    DebateColumnSide::Affirmative | DebateColumnSide::Unknown => affirmative,
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DebateColumnSide {
  Affirmative,
  Negative,
  Unknown,
}

fn debate_column_side(column: &str) -> DebateColumnSide {
  column
    .split(|ch: char| !ch.is_ascii_alphanumeric())
    .find_map(debate_column_token_side)
    .unwrap_or(DebateColumnSide::Unknown)
}

fn debate_column_token_side(token: &str) -> Option<DebateColumnSide> {
  let token = token.trim().to_ascii_uppercase();
  if token.is_empty() {
    return None;
  }

  const AFFIRMATIVE_TOKENS: &[&str] = &["A1", "A2", "A3", "P1", "P2", "PC", "PR", "PM", "DPM", "MG", "GW", "PW"];
  const NEGATIVE_TOKENS: &[&str] = &["N1", "N2", "N3", "O1", "O2", "CC", "CR", "LO", "DLO", "MO", "CW", "OW", "OR"];

  if token.contains("NEG")
    || token.contains("CON")
    || token.contains("OPP")
    || token.contains("NC")
    || token.contains("NR")
    || token.contains("NFF")
    || NEGATIVE_TOKENS.contains(&token.as_str())
    || token == "NS"
    || token.ends_with('N')
  {
    return Some(DebateColumnSide::Negative);
  }

  if token.contains("AFF")
    || token.contains("PRO")
    || token.contains("PROP")
    || token.contains("AC")
    || token.contains("AR")
    || AFFIRMATIVE_TOKENS.contains(&token.as_str())
    || token == "AS"
    || token.ends_with('A')
  {
    return Some(DebateColumnSide::Affirmative);
  }

  None
}

impl FlowSideColors {
  fn inverse(self) -> Self {
    Self {
      background: inverse_color(self.background),
      border: inverse_color(self.border),
      foreground: inverse_color(self.foreground),
      selected_background: inverse_color(self.selected_background),
      selected_border: inverse_color(self.selected_border),
    }
  }
}

fn inverse_color(color: Hsla) -> Hsla {
  let rgb = color.to_rgb();
  Hsla::from(gpui::Rgba {
    r: 1.0 - rgb.r,
    g: 1.0 - rgb.g,
    b: 1.0 - rgb.b,
    a: rgb.a,
  })
}

fn flow_box_column_index(level: i32) -> usize {
  level.saturating_sub(1).max(0) as usize
}

fn stable_element_id(value: &str) -> u64 {
  let mut hasher = DefaultHasher::new();
  value.hash(&mut hasher);
  hasher.finish()
}

#[cfg(test)]
mod tests {
  use super::{DebateColumnSide, debate_column_side, flow_box_column_index};

  #[test]
  fn classifies_affirmative_speech_columns() {
    for column in ["1AC", "2AC", "1AR", "2AR", "AC", "AFF", "Q/2A", "A3", "P1", "PC", "PM", "MG"] {
      assert_eq!(debate_column_side(column), DebateColumnSide::Affirmative, "{column}");
    }
  }

  #[test]
  fn classifies_negative_speech_columns() {
    for column in ["1NC", "2NC/1NR", "1NR", "2NR", "NC", "NFF", "Q/1N", "N3", "O1", "CC", "LO", "MO"] {
      assert_eq!(debate_column_side(column), DebateColumnSide::Negative, "{column}");
    }
  }

  #[test]
  fn maps_flow_box_levels_to_column_indices() {
    assert_eq!(flow_box_column_index(1), 0);
    assert_eq!(flow_box_column_index(2), 1);
    assert_eq!(flow_box_column_index(3), 2);
  }
}
