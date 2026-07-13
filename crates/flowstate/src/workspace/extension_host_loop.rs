use std::{sync::mpsc::Receiver, time::Duration};

use flowstate_extension::HostError;
use gpui::{AnyWindowHandle, Context, Entity, PathPromptOptions, PromptButton, PromptLevel, SharedString, Timer};

use crate::rich_text_element::{RichTextEditor, read_document};
use super::super::extensions_panel::{EditorHostRequest, ExtensionPanelEvent, ExtensionRunState};
use super::Workspace;

impl Workspace {
  pub(super) fn spawn_extension_host_loop(
    &self,
    extension_id: SharedString,
    editor: Option<Entity<RichTextEditor>>,
    requests: Receiver<EditorHostRequest>,
    window: AnyWindowHandle,
    cx: &mut Context<Self>,
  ) {
    cx.spawn(async move |workspace, cx| {
      loop {
        while let Ok(request) = requests.try_recv() {
          match request {
            EditorHostRequest::Snapshot(reply) => {
              let result = editor.as_ref().ok_or_else(no_document).and_then(|editor| editor.read_with(cx, |editor, _| editor.extension_snapshot_json()).map_err(host_access).and_then(|value| value.map_err(host_json)));
              let _ = reply.send(result);
            },
            EditorHostRequest::Selection(reply) => {
              let result = editor.as_ref().ok_or_else(no_document).and_then(|editor| editor
                .read_with(cx, |editor, _| editor.extension_snapshot_json())
                .map_err(host_access)
                .and_then(|value| value.map_err(host_json))
                .and_then(selection_json));
              let _ = reply.send(result);
            },
            EditorHostRequest::ApplyEdits(edits, reply) => {
              let result = editor.as_ref().ok_or_else(no_document).and_then(|editor| editor
                .update(cx, |editor, cx| editor.apply_extension_edits_json(&edits, cx))
                .map_err(host_access)
                .and_then(|value| value.map_err(|error| host_error("invalid-edits", error))));
              let _ = reply.send(result);
            },
            EditorHostRequest::Refresh(reply) => {
              let result = match editor.as_ref() {
                Some(editor) => refresh_editor_from_disk(editor, window, cx).await,
                None => Err(no_document()),
              };
              let _ = reply.send(result);
            },
            EditorHostRequest::ActionLabel(action_id, label) => {
              let _ = workspace.update(cx, |workspace, cx| {
                workspace.extensions.apply(ExtensionPanelEvent::ActionLabel {
                  extension_id: extension_id.clone(),
                  action_id: action_id.into(),
                  label: label.into(),
                });
                cx.notify();
              });
            },
            EditorHostRequest::Status(message) => {
              let _ = workspace.update(cx, |workspace, cx| {
                workspace.extensions.apply(ExtensionPanelEvent::Output {
                  extension_id: extension_id.clone(),
                  output: message.into(),
                });
                cx.notify();
              });
            },
            EditorHostRequest::DirectoryAccess(mode, suggested_path, reply) => {
              let access = match mode {
                flowstate_extension::DirectoryAccess::Read => "read-only",
                flowstate_extension::DirectoryAccess::ReadWrite => "read and write",
              };
              let detail = format!(
                "This extension is requesting {access} access to a directory. Suggested path: {}. The grant takes effect on its next invocation.",
                suggested_path.as_deref().unwrap_or("none"),
              );
              let approval = window
                .update(cx, |_, window, cx| {
                  window.prompt(
                    PromptLevel::Warning,
                    "Grant directory access?",
                    Some(&detail),
                    &[PromptButton::ok("Choose Directory"), PromptButton::cancel("Cancel")],
                    cx,
                  )
                })
                .map_err(host_access);
              let approved = match approval {
                Ok(answer) => matches!(answer.await, Ok(0)),
                Err(_) => false,
              };
              if !approved {
                let _ = reply.send(Err(host_error("directory-access-cancelled", "Directory access was not granted")));
                continue;
              }
              let selection = window
                .update(cx, |_, _, cx| {
                  cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Grant extension directory access".into()),
                  })
                })
                .map_err(host_access);
              let result = match selection {
                Ok(selection) => match selection.await {
                  Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => workspace
                      .update(cx, |workspace, _| {
                        workspace
                          .extension_service
                          .as_ref()
                          .ok_or_else(|| host_error("runtime-unavailable", "Extension runtime is unavailable"))?
                          .grant_directory(extension_id.as_ref(), mode, path)
                      })
                      .map_err(host_access)
                      .and_then(std::convert::identity),
                    None => Err(host_error("directory-access-cancelled", "No directory was selected")),
                  },
                  _ => Err(host_error("directory-access-cancelled", "Directory access was not granted")),
                },
                Err(error) => Err(error),
              };
              let _ = reply.send(result);
            },
          }
        }
        let terminal = workspace
          .update(cx, |workspace, cx| {
            let events = workspace.extension_service.as_ref().map_or_else(Vec::new, |service| service.drain_events_for(extension_id.as_ref()));
            for event in events { workspace.extensions.apply(event); }
            let terminal = !matches!(workspace.extensions.state(extension_id.as_ref()), ExtensionRunState::Running { .. });
            if terminal { cx.notify(); }
            terminal
          })
          .unwrap_or(false);
        if terminal { break }
        Timer::after(Duration::from_millis(16)).await;
      }
    })
    .detach();
  }
}

fn selection_json(snapshot: String) -> Result<String, HostError> {
  let value: serde_json::Value = serde_json::from_str(&snapshot).map_err(host_json)?;
  serde_json::to_string(value.get("selection").unwrap_or(&serde_json::Value::Null)).map_err(host_json)
}

async fn refresh_editor_from_disk(
  editor: &Entity<RichTextEditor>,
  window: AnyWindowHandle,
  cx: &mut gpui::AsyncApp,
) -> Result<String, HostError> {
  let (path, dirty) = editor
    .read_with(cx, |editor, _| (editor.document_path().cloned(), editor.has_unsaved_changes()))
    .map_err(host_access)?;
  let path = path.ok_or_else(|| host_error("unsaved-document", "The active document has no on-disk path"))?;
  if dirty {
    let answer = window
      .update(cx, |_, window, cx| {
        window.prompt(
          PromptLevel::Warning,
          "Discard unsaved changes?",
          Some("The extension requested a disk refresh. This replaces the editor contents and clears undo history."),
          &[PromptButton::ok("Refresh"), PromptButton::cancel("Cancel")],
          cx,
        )
      })
      .map_err(host_access)?;
    if !matches!(answer.await, Ok(0)) {
      return Err(host_error("refresh-cancelled", "Document refresh was cancelled"));
    }
  }
  let document = read_document(&path).map_err(|error| host_error("refresh-failed", error))?;
  editor
    .update(cx, |editor, cx| {
      editor.replace_document_from_disk(document, cx);
      editor.extension_snapshot_json()
    })
    .map_err(host_access)?
    .map_err(host_json)
}

fn host_json(error: serde_json::Error) -> HostError {
  host_error("json", error)
}

fn no_document() -> HostError {
  host_error("no-document", "No rich document is active")
}

fn host_access(error: impl std::fmt::Display) -> HostError {
  host_error("editor-unavailable", error)
}

fn host_error(code: &str, error: impl std::fmt::Display) -> HostError {
  HostError { code: code.to_owned(), message: error.to_string() }
}
