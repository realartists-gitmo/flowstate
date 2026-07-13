use gpui::{AnyElement, AnyWindowHandle, Context, IntoElement, PromptButton, PromptLevel, SharedString, Window, div, prelude::*, px};
use gpui_component::{
  ActiveTheme as _, Disableable, Icon, IconName, Selectable, Sizable,
  button::{Button, ButtonVariants},
  collapsible::Collapsible,
  h_flex, v_flex,
};

use super::super::extensions_panel::{ExtensionRunState, ExtensionView};
use super::Workspace;

impl Workspace {
  pub(super) fn render_extensions_panel(&self, cx: &mut Context<Self>) -> AnyElement {
    let extensions = self.extensions.extensions().to_vec();
    let body = if let Some(error) = self.extensions.error() {
      self.render_extensions_message(error.clone(), cx)
    } else if extensions.is_empty() {
      self.render_extensions_message("No extensions installed".into(), cx)
    } else {
      v_flex()
        .w_full()
        .p_2()
        .gap_2()
        .children(extensions.iter().map(|extension| self.render_extension_card(extension, cx)))
        .into_any_element()
    };

    v_flex()
      .size_full()
      .min_h_0()
      .bg(cx.theme().background)
      .border_l(super::APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .child(
        h_flex()
          .h(px(34.0))
          .flex_none()
          .items_center()
          .gap_2()
          .px_2()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(
            Button::new("extensions-panel-tool")
              .icon(Icon::new(IconName::Bot).text_color(cx.theme().sidebar_primary))
              .xsmall()
              .ghost()
              .selected(true)
              .tooltip("Close extensions panel")
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.toggle_toolkit_tool(super::ToolkitTool::Extensions, cx);
              })),
          )
          .child(div().flex_1().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child("Extensions"))
          .child(
            Button::new("reload-extensions")
              .icon(Icon::new(IconName::Redo2).text_color(cx.theme().muted_foreground))
              .xsmall()
              .ghost()
              .tooltip("Reload extensions")
              .on_click(cx.listener(|workspace, _, _, cx| workspace.reload_extensions(cx))),
          )
          .child(
            Button::new("hide-extensions-rail")
              .icon(Icon::new(IconName::PanelRightClose).text_color(cx.theme().muted_foreground))
              .xsmall()
              .ghost()
              .tooltip("Hide toolkit")
              .on_click(cx.listener(|workspace, _, _, cx| workspace.toggle_toolkit(cx))),
          ),
      )
      .child(div().id("extensions-panel-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
      .into_any_element()
  }

  fn render_extensions_message(&self, message: SharedString, cx: &mut Context<Self>) -> AnyElement {
    div()
      .h(px(120.0))
      .flex()
      .items_center()
      .justify_center()
      .text_sm()
      .text_color(cx.theme().muted_foreground)
      .child(message)
      .into_any_element()
  }

  fn render_extension_card(&self, extension: &ExtensionView, cx: &mut Context<Self>) -> AnyElement {
    let extension_id = extension.id.clone();
    let open = self.expanded_extensions.contains(&extension_id);
    let state = self.extensions.state(extension_id.as_ref());
    let running = matches!(state, ExtensionRunState::Running { .. });
    let toggle_id = extension_id.clone();
    let chevron = if open { IconName::ChevronDown } else { IconName::ChevronRight };
    let actions = extension.actions.iter().map(|action| {
      let action_id = action.id.clone();
      let target_extension = extension_id.clone();
      let label = self.extensions.label(extension_id.as_ref(), action);
      let action_running = matches!(
        &state,
        ExtensionRunState::Running { action_id: running_action } if running_action == &action.id
      );
      Button::new(SharedString::from(format!("extension-action-{}-{}", extension_id, action.id)))
        .label(label)
        .w_full()
        .loading(action_running)
        .disabled(running || (action.requires_document && self.active_editor.is_none()))
        .on_click(cx.listener(move |workspace, _, window, cx| {
          workspace.request_extension_action(target_extension.clone(), action_id.clone(), window, cx);
        }))
    });
    let status = match &state {
      ExtensionRunState::Idle => None,
      ExtensionRunState::Running { .. } => Some("Running…".into()),
      ExtensionRunState::Failed(message) => Some(message.clone()),
      ExtensionRunState::Cancelled => Some("Cancelled".into()),
    };
    let output = self.extensions.output(extension_id.as_ref()).cloned();
    let cancel_id = extension_id.clone();

    Collapsible::new()
      .open(open)
      .w_full()
      .rounded(cx.theme().radius)
      .border_1()
      .border_color(cx.theme().border)
      .child(
        Button::new(SharedString::from(format!("extension-group-{extension_id}")))
          .icon(Icon::new(chevron).text_color(cx.theme().muted_foreground))
          .label(extension.name.clone())
          .w_full()
          .ghost()
          .on_click(cx.listener(move |workspace, _, _, cx| {
            workspace.toggle_extension_group(toggle_id.clone(), cx);
          })),
      )
      .content(
        v_flex()
          .w_full()
          .gap_2()
          .px_2()
          .pb_2()
          .child(
            div()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(format!("{} · {}", extension.id, extension.version)),
          )
          .children(actions)
          .when(running, |this| {
            this.child(
              Button::new(SharedString::from(format!("cancel-extension-{extension_id}")))
                .label("Cancel")
                .outline()
                .w_full()
                .on_click(cx.listener(move |workspace, _, _, cx| {
                  workspace.cancel_extension(cancel_id.clone(), cx);
                })),
            )
          })
          .when_some(status, |this, status| {
            this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(status))
          })
          .when_some(output, |this, output| {
            this.child(
              div()
                .id(SharedString::from(format!("extension-output-{extension_id}")))
                .max_h(px(160.0))
                .overflow_y_scroll()
                .rounded(cx.theme().radius)
                .bg(cx.theme().muted)
                .p_2()
                .text_xs()
                .child(output),
            )
          }),
      )
      .into_any_element()
  }

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
      self.start_extension_action(extension_id, action_id, window.window_handle(), cx);
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
    let window_handle = window.window_handle();
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
          workspace.start_extension_action(extension_id, action_id, window_handle, cx);
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn start_extension_action(
    &mut self,
    extension_id: SharedString,
    action_id: SharedString,
    window: AnyWindowHandle,
    cx: &mut Context<Self>,
  ) {
    let Some(service) = self.extension_service.clone() else {
      self.extensions.apply(super::super::extensions_panel::ExtensionPanelEvent::Failed {
        extension_id,
        message: "Extension runtime is unavailable".into(),
      });
      return;
    };
    let editor = self.active_editor.clone();
    let document_root = editor.as_ref().and_then(|editor| editor.read(cx).document_path().and_then(|path| path.parent().map(ToOwned::to_owned)));
    let (host, requests) = super::super::extensions_panel::EditorHostBridge::bounded(32);
    self.spawn_extension_host_loop(extension_id.clone(), editor, requests, window, cx);
    self.extensions.invoke(extension_id.as_ref(), action_id.as_ref());
    if let Err(message) = service.invoke_with_host(extension_id.as_ref(), action_id.as_ref(), document_root, Box::new(host)) {
      self.extensions.apply(super::super::extensions_panel::ExtensionPanelEvent::Failed { extension_id, message });
    }
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
