mod bridge;
mod model;
mod service;

use std::sync::Arc;

use gpui::SharedString;

pub use model::{
  ExtensionActionView, ExtensionPanelAdapter, ExtensionPanelController, ExtensionPanelEvent, ExtensionRunState, ExtensionView,
};
pub use service::ExtensionService;
pub use bridge::{EditorHostBridge, EditorHostRequest};

struct DisconnectedExtensionAdapter;

impl ExtensionPanelAdapter for DisconnectedExtensionAdapter {
  fn installed(&self) -> Result<Vec<ExtensionView>, SharedString> {
    Ok(Vec::new())
  }

  fn is_trusted(&self, _: &str, _: &str) -> bool {
    false
  }

  fn trust(&self, _: &str, _: &str) -> Result<(), SharedString> {
    Err("Extension runtime is not connected".into())
  }

  fn cancel(&self, _: &str) -> Result<(), SharedString> {
    Err("Extension runtime is not connected".into())
  }
}

pub fn disconnected_controller() -> ExtensionPanelController {
  ExtensionPanelController::new(Arc::new(DisconnectedExtensionAdapter))
}
