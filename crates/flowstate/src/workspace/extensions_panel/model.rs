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
      .get(&(extension_id.into(), action.id.clone()))
      .cloned()
      .unwrap_or_else(|| action.label.clone())
  }

  pub fn output(&self, extension_id: &str) -> Option<&SharedString> {
    self.outputs.get(extension_id)
  }

  pub fn error(&self) -> Option<&SharedString> {
    self.error.as_ref()
  }
}
