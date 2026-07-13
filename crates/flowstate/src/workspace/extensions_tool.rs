use gpui::{Context, PromptButton, PromptLevel, SharedString, Window};

use super::Workspace;

impl Workspace {
  fn reload_extensions(&mut self, cx: &mut Context<Self>) {
    self.extensions.reload();
    self
      .expanded_extensions
      .retain(|id| self.extensions.extensions().iter().any(|extension| &extension.id == id));
    cx.notify();
  }

  fn request_extension_action(
    &mut self,
    extension_id: SharedString,
    action_id: SharedString,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    if !self.extensions.requires_trust(extension_id.as_ref()) {
      self.extensions.invoke(extension_id.as_ref(), action_id.as_ref());
      cx.notify();
      return;
    }

    let answer = window.prompt(
      PromptLevel::Warning,
      "Run trusted extension?",
      Some("Extensions can access your active document, its folder, the network, and their own data. Only continue if you trust this extension."),
      &[PromptButton::ok("Trust and Run"), PromptButton::cancel("Cancel")],
      cx,
    );
    cx.spawn(async move |workspace, cx| {
      if !matches!(answer.await, Ok(0)) {
        return;
      }
      let _ = workspace.update(cx, |workspace, cx| {
        if let Err(error) = workspace.extensions.trust(extension_id.as_ref()) {
          workspace.extensions.apply(super::super::extensions_panel::ExtensionPanelEvent::Failed {
            extension_id,
            message: error,
          });
        } else {
          workspace.extensions.invoke(extension_id.as_ref(), action_id.as_ref());
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn cancel_extension(&mut self, extension_id: SharedString, cx: &mut Context<Self>) {
    self.extensions.cancel(extension_id.as_ref());
    cx.notify();
  }

  fn toggle_extension_group(&mut self, extension_id: SharedString, cx: &mut Context<Self>) {
    if !self.expanded_extensions.remove(&extension_id) {
      self.expanded_extensions.insert(extension_id);
    }
    cx.notify();
  }
}
