use rustc_hash::FxHashMap;

use crate::actions::ActionBundle;
use crate::document::{FlowDocument, NodeId, ROOT_ID};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAction {
  pub before_focus: Option<NodeId>,
  pub after_focus: Option<NodeId>,
  pub action_bundle: ActionBundle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
  index: Option<usize>,
  actions: Vec<HistoryAction>,
}

#[hotpath::measure_all]
impl History {
  pub fn add(&mut self, action_bundle: ActionBundle, before_focus: Option<NodeId>, after_focus: Option<NodeId>) {
    let keep = self.index.map_or(0, |index| index + 1);
    self.actions.truncate(keep);
    self.actions.push(HistoryAction {
      before_focus,
      after_focus,
      action_bundle,
    });
    self.index = Some(self.actions.len() - 1);
  }

  pub fn undo(&mut self, document: &mut FlowDocument) -> Option<Option<NodeId>> {
    let index = self.index?;
    let action = self.actions.get_mut(index)?;
    let inverse = document.apply_action_bundle(action.action_bundle.clone());
    action.action_bundle = inverse;
    self.index = index.checked_sub(1);
    Some(action.before_focus.clone())
  }

  pub fn redo(&mut self, document: &mut FlowDocument) -> Option<Option<NodeId>> {
    let next_index = self.index.map_or(0, |index| index + 1);
    if next_index >= self.actions.len() {
      return None;
    }
    self.index = Some(next_index);
    let action = self.actions.get_mut(next_index)?;
    let inverse = document.apply_action_bundle(action.action_bundle.clone());
    action.action_bundle = inverse;
    Some(
      action
        .after_focus
        .clone()
        .or_else(|| action.before_focus.clone()),
    )
  }

  #[must_use]
  pub const fn can_undo(&self) -> bool {
    self.index.is_some()
  }

  #[must_use]
  pub fn can_redo(&self) -> bool {
    self
      .index
      .map_or(!self.actions.is_empty(), |index| index + 1 < self.actions.len())
  }

  pub fn clear(&mut self) {
    self.index = None;
    self.actions.clear();
  }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryHolder {
  histories: FxHashMap<NodeId, History>,
  last_added_owner: Option<NodeId>,
}

#[hotpath::measure_all]
impl HistoryHolder {
  #[must_use]
  pub fn new() -> Self {
    let mut histories = FxHashMap::default();
    histories.insert(ROOT_ID.to_owned(), History::default());
    Self {
      histories,
      last_added_owner: None,
    }
  }

  pub fn add(&mut self, owner: NodeId, action_bundle: ActionBundle, before_focus: Option<NodeId>, after_focus: Option<NodeId>) {
    self
      .histories
      .entry(owner.clone())
      .or_default()
      .add(action_bundle, before_focus, after_focus);
    self.last_added_owner = Some(owner);
  }

  pub fn undo(&mut self, owner: impl AsRef<str>, document: &mut FlowDocument) -> Option<Option<NodeId>> {
    self.histories.get_mut(owner.as_ref())?.undo(document)
  }

  pub fn redo(&mut self, owner: impl AsRef<str>, document: &mut FlowDocument) -> Option<Option<NodeId>> {
    self.histories.get_mut(owner.as_ref())?.redo(document)
  }

  pub fn can_undo(&self, owner: impl AsRef<str>) -> bool {
    self
      .histories
      .get(owner.as_ref())
      .is_some_and(History::can_undo)
  }

  pub fn can_redo(&self, owner: impl AsRef<str>) -> bool {
    self
      .histories
      .get(owner.as_ref())
      .is_some_and(History::can_redo)
  }

  pub fn clear(&mut self) {
    self.histories.clear();
    self
      .histories
      .insert(ROOT_ID.to_owned(), History::default());
    self.last_added_owner = None;
  }
}
