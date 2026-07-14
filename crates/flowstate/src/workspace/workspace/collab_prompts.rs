#[derive(Clone)]
struct CollaborationClosePanel {
  id: Uuid,
  panel: PanelKind,
  save_prompt: CollaborationSavePrompt,
}

#[derive(Clone, Copy)]
enum CollaborationSavePrompt {
  Clean,
  Dirty,
  Pathless,
}

enum CollaborationCloseResolution {
  Proceed { discard: Option<PanelKind> },
  Cancelled,
}

#[hotpath::measure_all]
impl Workspace {
  fn close_collaboration_document_panel(
    &mut self,
    panel_id: Uuid,
    panel_kind: PanelKind,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> bool {
    let phase = crate::collab::phase_for_panel(panel_id, cx);
    if !collaboration_phase_blocks_close(phase.as_ref()) {
      return false;
    }

    let detail = collaboration_leave_detail(phase.as_ref());
    let save_prompt = collaboration_save_prompt_for_panel(&panel_kind, cx);
    let answer = window.prompt(
      PromptLevel::Warning,
      "Leave the collaboration session?",
      Some(&detail),
      &[PromptButton::ok("Leave"), PromptButton::cancel("Cancel")],
      cx,
    );
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      if !matches!(answer.await, Ok(0)) {
        return;
      }

      let resolution = resolve_collaboration_save(panel_kind, save_prompt, window_handle, cx).await;
      let CollaborationCloseResolution::Proceed { discard } = resolution else {
        return;
      };
      if let Some(panel) = discard {
        panel.discard(cx);
      }

      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.leave_collaboration_on_panel(panel_id, cx);
          workspace.remove_document_panel(panel_id, window, cx);
        });
      });
    })
    .detach();
    true
  }

  fn request_close_window_with_collaboration(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let collaboration_panels = self.collaboration_close_panels(cx);
    if collaboration_panels.is_empty() {
      return false;
    }

    let collaboration_ids = collaboration_panels
      .iter()
      .map(|panel| panel.id)
      .collect::<HashSet<_>>();
    let dirty_panels = self
      .dirty_panels(cx)
      .into_iter()
      .filter(|panel| !collaboration_ids.contains(&panel.panel_id(cx)))
      .collect::<Vec<_>>();

    let answer = if collaboration_panels.len() == 1 {
      let phase = crate::collab::phase_for_panel(collaboration_panels[0].id, cx);
      window.prompt(
        PromptLevel::Warning,
        "Leave the collaboration session and quit?",
        Some(&collaboration_leave_detail(phase.as_ref())),
        &[PromptButton::ok("Leave"), PromptButton::cancel("Cancel")],
        cx,
      )
    } else {
      let detail = format!(
        "You're in {} collaboration sessions. Leave all and quit?",
        collaboration_panels.len()
      );
      window.prompt(
        PromptLevel::Warning,
        "Leave collaboration sessions?",
        Some(&detail),
        &[PromptButton::ok("Leave"), PromptButton::cancel("Cancel")],
        cx,
      )
    };
    let window_handle = window.window_handle();

    cx.spawn(async move |workspace, cx| {
      if !matches!(answer.await, Ok(0)) {
        return;
      }

      let mut discards = Vec::new();
      for panel in collaboration_panels {
        match resolve_collaboration_save(panel.panel, panel.save_prompt, window_handle, cx).await {
          CollaborationCloseResolution::Proceed { discard } => {
            if let Some(discard) = discard {
              discards.push(discard);
            }
          },
          CollaborationCloseResolution::Cancelled => return,
        }
      }

      if !resolve_dirty_window_close(dirty_panels, window_handle, cx).await {
        return;
      }

      for panel in discards {
        panel.discard(cx);
      }

      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| workspace.leave_all_collaboration_sessions(cx));
        window.remove_window();
      });
    })
    .detach();
    true
  }

  fn collaboration_close_panels(&self, cx: &App) -> Vec<CollaborationClosePanel> {
    self
      .document_panels
      .iter()
      .filter_map(|panel| {
        let panel_state = panel.read(cx);
        let id = panel_state.id();
        let phase = crate::collab::phase_for_panel(id, cx);
        collaboration_phase_blocks_close(phase.as_ref()).then(|| {
          let panel_kind = PanelKind::Document {
            panel: panel.clone(),
            editor: panel_state.editor(),
          };
          CollaborationClosePanel {
            id,
            save_prompt: collaboration_save_prompt_for_panel(&panel_kind, cx),
            panel: panel_kind,
          }
        })
      })
      .collect()
  }
}

#[hotpath::measure_all]
impl PanelKind {
  fn panel_id(&self, cx: &App) -> Uuid {
    match self {
      PanelKind::Document { panel, .. } => panel.read(cx).id(),
      PanelKind::Flow { panel, .. } => panel.read(cx).id(),
    }
  }
}

#[hotpath::measure]
async fn resolve_collaboration_save(
  panel: PanelKind,
  prompt: CollaborationSavePrompt,
  window_handle: AnyWindowHandle,
  cx: &mut gpui::AsyncApp,
) -> CollaborationCloseResolution {
  match prompt {
    CollaborationSavePrompt::Clean => CollaborationCloseResolution::Proceed { discard: None },
    CollaborationSavePrompt::Dirty | CollaborationSavePrompt::Pathless => {
      let answer = match window_handle.update(cx, |_, window, cx| {
        let (detail, save_label) = match prompt {
          CollaborationSavePrompt::Pathless => ("This shared tab has no saved file.", "Save As..."),
          CollaborationSavePrompt::Dirty => ("This document has unsaved changes.", "Save"),
          CollaborationSavePrompt::Clean => unreachable!(),
        };
        window.prompt(
          PromptLevel::Warning,
          "Save changes before closing?",
          Some(detail),
          &[PromptButton::ok(save_label), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
          cx,
        )
      }) {
        Ok(answer) => answer,
        Err(error) => {
          show_save_failed(window_handle, cx, format!("failed to show save prompt: {error}"));
          return CollaborationCloseResolution::Cancelled;
        },
      };

      match answer.await {
        Ok(0) => match panel.save(window_handle, cx).await {
          PanelSaveOutcome::Saved => CollaborationCloseResolution::Proceed { discard: None },
          PanelSaveOutcome::Cancelled => CollaborationCloseResolution::Cancelled,
          PanelSaveOutcome::Failed(error) => {
            show_save_failed(window_handle, cx, error);
            CollaborationCloseResolution::Cancelled
          },
        },
        Ok(1) => CollaborationCloseResolution::Proceed { discard: Some(panel) },
        _ => CollaborationCloseResolution::Cancelled,
      }
    },
  }
}

#[hotpath::measure]
async fn resolve_dirty_window_close(
  dirty_panels: Vec<PanelKind>,
  window_handle: AnyWindowHandle,
  cx: &mut gpui::AsyncApp,
) -> bool {
  if dirty_panels.is_empty() {
    return true;
  }
  let message = if dirty_panels.len() == 1 {
    "This document has unsaved changes."
  } else {
    "One or more documents have unsaved changes."
  };
  let answer = match window_handle.update(cx, |_, window, cx| {
    window.prompt(
      PromptLevel::Warning,
      "Save changes before closing?",
      Some(message),
      &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
      cx,
    )
  }) {
    Ok(answer) => answer,
    Err(error) => {
      show_save_failed(window_handle, cx, format!("failed to show save prompt: {error}"));
      return false;
    },
  };

  match answer.await {
    Ok(0) => {
      for panel in dirty_panels {
        match panel.save(window_handle, cx).await {
          PanelSaveOutcome::Saved => {},
          PanelSaveOutcome::Cancelled => return false,
          PanelSaveOutcome::Failed(error) => {
            show_save_failed(window_handle, cx, error);
            return false;
          },
        }
      }
      true
    },
    Ok(1) => {
      for panel in dirty_panels {
        panel.discard(cx);
      }
      true
    },
    _ => false,
  }
}

#[hotpath::measure]
fn collaboration_save_prompt_for_panel(panel: &PanelKind, cx: &App) -> CollaborationSavePrompt {
  match panel {
    PanelKind::Document { editor, .. } => {
      let editor = editor.read(cx);
      if editor.document_path().is_none() {
        CollaborationSavePrompt::Pathless
      } else if editor.has_unsaved_changes() {
        CollaborationSavePrompt::Dirty
      } else {
        CollaborationSavePrompt::Clean
      }
    },
    PanelKind::Flow { editor, .. } => {
      let editor = editor.read(cx);
      if editor.document_path().is_none() {
        CollaborationSavePrompt::Pathless
      } else if editor.has_unsaved_changes() {
        CollaborationSavePrompt::Dirty
      } else {
        CollaborationSavePrompt::Clean
      }
    },
  }
}

#[hotpath::measure]
fn collaboration_phase_blocks_close(phase: Option<&crate::collab::SessionPhase>) -> bool {
  phase.is_some_and(|phase| !matches!(phase, crate::collab::SessionPhase::Detached(_)))
}

#[hotpath::measure]
fn collaboration_leave_detail(phase: Option<&crate::collab::SessionPhase>) -> String {
  match phase {
    Some(crate::collab::SessionPhase::Attached(attachment)) if attachment.peers_present == 0 => {
      "This tab is live, but no other people are present right now.".to_string()
    },
    Some(crate::collab::SessionPhase::Attached(attachment)) => format!(
      "This tab is live with {} other {}.",
      attachment.peers_present,
      if attachment.peers_present == 1 { "person" } else { "people" }
    ),
    Some(crate::collab::SessionPhase::Creating) => "This tab is starting a collaboration session.".to_string(),
    Some(crate::collab::SessionPhase::Joining(_)) => "This tab is joining a collaboration session.".to_string(),
    _ => "This tab is connected to a collaboration session.".to_string(),
  }
}
