use std::{collections::BTreeMap, sync::Arc};

use gpui::SharedString;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionActionView {
  pub id: SharedString,
  pub label: SharedString,
  pub requires_document: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionView {
  pub id: SharedString,
  pub name: SharedString,
  pub version: SharedString,
  pub component_hash: SharedString,
  pub actions: Vec<ExtensionActionView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionRunState {
  Idle,
  Running { action_id: SharedString },
  Failed(SharedString),
  Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "runtime events are consumed once the Wasmtime adapter is connected")]
pub enum ExtensionPanelEvent {
  Started { extension_id: SharedString, action_id: SharedString },
  ActionLabel { extension_id: SharedString, action_id: SharedString, label: SharedString },
  Output { extension_id: SharedString, output: SharedString },
  Failed { extension_id: SharedString, message: SharedString },
  Finished { extension_id: SharedString },
  Cancelled { extension_id: SharedString },
}

pub trait ExtensionPanelAdapter: Send + Sync {
  fn installed(&self) -> Result<Vec<ExtensionView>, SharedString>;
  fn is_trusted(&self, extension_id: &str, component_hash: &str) -> bool;
  fn trust(&self, extension_id: &str, component_hash: &str) -> Result<(), SharedString>;
  fn invoke(&self, extension_id: &str, action_id: &str) -> Result<(), SharedString>;
  fn cancel(&self, extension_id: &str) -> Result<(), SharedString>;
}

pub struct ExtensionPanelController {
  adapter: Arc<dyn ExtensionPanelAdapter>,
  extensions: Vec<ExtensionView>,
  states: BTreeMap<SharedString, ExtensionRunState>,
  labels: BTreeMap<(SharedString, SharedString), SharedString>,
  outputs: BTreeMap<SharedString, SharedString>,
  error: Option<SharedString>,
}

impl ExtensionPanelController {
  pub fn new(adapter: Arc<dyn ExtensionPanelAdapter>) -> Self {
    Self {
      adapter,
      extensions: Vec::new(),
      states: BTreeMap::new(),
      labels: BTreeMap::new(),
      outputs: BTreeMap::new(),
      error: None,
    }
  }

  pub fn reload(&mut self) {
    match self.adapter.installed() {
      Ok(extensions) => {
        self.extensions = extensions;
        self.error = None;
      },
      Err(error) => self.error = Some(error),
    }
  }

  pub fn extensions(&self) -> &[ExtensionView] {
    &self.extensions
  }

  pub fn state(&self, extension_id: &str) -> ExtensionRunState {
    self.states.get(extension_id).cloned().unwrap_or(ExtensionRunState::Idle)
  }

  pub fn label(&self, extension_id: &str, action: &ExtensionActionView) -> SharedString {
    self
      .labels
      .get(&(SharedString::from(extension_id.to_owned()), action.id.clone()))
      .cloned()
      .unwrap_or_else(|| action.label.clone())
  }

  pub fn output(&self, extension_id: &str) -> Option<&SharedString> {
    self.outputs.get(extension_id)
  }

  pub fn error(&self) -> Option<&SharedString> {
    self.error.as_ref()
  }

  pub fn requires_trust(&self, extension_id: &str) -> bool {
    self
      .extensions
      .iter()
      .find(|extension| extension.id.as_ref() == extension_id)
      .is_some_and(|extension| !self.adapter.is_trusted(extension_id, extension.component_hash.as_ref()))
  }

  pub fn trust(&mut self, extension_id: &str) -> Result<(), SharedString> {
    let extension = self
      .extensions
      .iter()
      .find(|extension| extension.id.as_ref() == extension_id)
      .ok_or_else(|| SharedString::from("Extension is no longer installed"))?;
    self.adapter.trust(extension_id, extension.component_hash.as_ref())
  }

  pub fn invoke(&mut self, extension_id: &str, action_id: &str) {
    self
      .states
      .insert(
        SharedString::from(extension_id.to_owned()),
        ExtensionRunState::Running { action_id: SharedString::from(action_id.to_owned()) },
      );
    self.outputs.remove(extension_id);
    if let Err(error) = self.adapter.invoke(extension_id, action_id) {
      self.states.insert(SharedString::from(extension_id.to_owned()), ExtensionRunState::Failed(error));
    }
  }

  pub fn cancel(&mut self, extension_id: &str) {
    if let Err(error) = self.adapter.cancel(extension_id) {
      self.states.insert(SharedString::from(extension_id.to_owned()), ExtensionRunState::Failed(error));
    }
  }

  pub fn apply(&mut self, event: ExtensionPanelEvent) {
    match event {
      ExtensionPanelEvent::Started { extension_id, action_id } => {
        self.states.insert(extension_id, ExtensionRunState::Running { action_id });
      },
      ExtensionPanelEvent::ActionLabel { extension_id, action_id, label } => {
        self.labels.insert((extension_id, action_id), label);
      },
      ExtensionPanelEvent::Output { extension_id, output } => {
        self.outputs.insert(extension_id, output);
      },
      ExtensionPanelEvent::Failed { extension_id, message } => {
        self.states.insert(extension_id, ExtensionRunState::Failed(message));
      },
      ExtensionPanelEvent::Finished { extension_id } => {
        self.states.insert(extension_id, ExtensionRunState::Idle);
      },
      ExtensionPanelEvent::Cancelled { extension_id } => {
        self.states.insert(extension_id, ExtensionRunState::Cancelled);
      },
    }
  }
}
