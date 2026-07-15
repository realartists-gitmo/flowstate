// SB-S2/S3: the status bar's save-state + activity models. Save state is
// per-panel and quiet (a dot; text only on failure — Patient 8 decision);
// activity is one window-level slot where transient events surface and fade
// while FAILURES persist until dismissed, retried, or superseded (Law 2).

/// Per-panel save lifecycle, driven by the autosave/save paths. Dirtiness is
/// read live from the panel (`has_unsaved_changes`), not stored here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PanelSaveState {
  Saving,
  Saved,
  /// Never saved to disk (untitled/pathless) — Law 1: never shown as Saved.
  Unsaved,
  Failed { message: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActivityKind {
  Transient,
  Failure,
}

/// Typed follow-up for a failure event — no closures, so events stay
/// inspectable and the action wiring stays in one match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityAction {
  RetrySave { panel_id: Uuid },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityEvent {
  pub(super) kind: ActivityKind,
  pub(super) message: String,
  pub(super) action: Option<ActivityAction>,
  pub(super) generation: u64,
}

const ACTIVITY_TRANSIENT_DECAY: std::time::Duration = std::time::Duration::from_secs(6);

impl Workspace {
  pub(super) fn set_save_state(&mut self, panel_id: Uuid, state: PanelSaveState, cx: &mut Context<Self>) {
    if self.panel_save_states.get(&panel_id) != Some(&state) {
      self.panel_save_states.insert(panel_id, state);
      cx.notify();
    }
  }

  pub(super) fn clear_save_state(&mut self, panel_id: Uuid) {
    self.panel_save_states.remove(&panel_id);
  }

  /// Surface a transient event in the status bar's activity zone; it fades
  /// after [`ACTIVITY_TRANSIENT_DECAY`] unless superseded first. A pending
  /// FAILURE is never displaced by a transient (Law 2: failures persist).
  pub fn report_activity(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
    if matches!(
      self.activity_event,
      Some(ActivityEvent {
        kind: ActivityKind::Failure,
        ..
      })
    ) {
      return;
    }
    self.activity_generation = self.activity_generation.wrapping_add(1);
    let generation = self.activity_generation;
    self.activity_event = Some(ActivityEvent {
      kind: ActivityKind::Transient,
      message: message.into(),
      action: None,
      generation,
    });
    cx.notify();
    cx.spawn(async move |workspace, cx| {
      cx.background_executor().timer(ACTIVITY_TRANSIENT_DECAY).await;
      let _ = workspace.update(cx, |workspace, cx| {
        if workspace
          .activity_event
          .as_ref()
          .is_some_and(|event| event.generation == generation)
        {
          workspace.activity_event = None;
          cx.notify();
        }
      });
    })
    .detach();
  }

  /// Surface a failure in the activity zone. It persists until the user
  /// dismisses it, runs its action, or a newer failure supersedes it.
  pub fn report_failure(&mut self, message: impl Into<String>, action: Option<ActivityAction>, cx: &mut Context<Self>) {
    self.activity_generation = self.activity_generation.wrapping_add(1);
    self.activity_event = Some(ActivityEvent {
      kind: ActivityKind::Failure,
      message: message.into(),
      action,
      generation: self.activity_generation,
    });
    cx.notify();
  }

  pub(super) fn dismiss_activity(&mut self, cx: &mut Context<Self>) {
    if self.activity_event.take().is_some() {
      cx.notify();
    }
  }

  pub(super) fn run_activity_action(&mut self, action: ActivityAction, window: &mut Window, cx: &mut Context<Self>) {
    self.dismiss_activity(cx);
    match action {
      ActivityAction::RetrySave { panel_id } => {
        // Retry through the ordinary save path for whichever panel failed —
        // if it is no longer active, activate-then-save would surprise, so
        // save it directly wherever it lives.
        if let Some(editor) = self
          .document_panels
          .iter()
          .find(|panel| panel.read(cx).id() == panel_id)
          .map(|panel| panel.read(cx).editor().clone())
        {
          self.set_save_state(panel_id, PanelSaveState::Saving, cx);
          let save_task = editor.update(cx, |editor, cx| editor.save(cx));
          cx.spawn(async move |workspace, cx| {
            let result = save_task.await;
            let _ = workspace.update(cx, |workspace, cx| match result {
              Ok(()) => workspace.set_save_state(panel_id, PanelSaveState::Saved, cx),
              Err(error) => {
                workspace.set_save_state(
                  panel_id,
                  PanelSaveState::Failed {
                    message: error.to_string(),
                  },
                  cx,
                );
                workspace.report_failure(
                  format!("Save failed: {error}"),
                  Some(ActivityAction::RetrySave { panel_id }),
                  cx,
                );
              },
            });
          })
          .detach();
        } else if let Some(flow) = self
          .flow_panels
          .iter()
          .find(|panel| panel.read(cx).id() == panel_id)
          .map(|panel| panel.read(cx).editor().clone())
        {
          self.set_save_state(panel_id, PanelSaveState::Saving, cx);
          let save_task = flow.update(cx, |editor, cx| editor.save(cx));
          cx.spawn(async move |workspace, cx| {
            let result = save_task.await;
            let _ = workspace.update(cx, |workspace, cx| match result {
              Ok(()) => workspace.set_save_state(panel_id, PanelSaveState::Saved, cx),
              Err(error) => {
                workspace.set_save_state(
                  panel_id,
                  PanelSaveState::Failed {
                    message: error.to_string(),
                  },
                  cx,
                );
                workspace.report_failure(
                  format!("Save failed: {error}"),
                  Some(ActivityAction::RetrySave { panel_id }),
                  cx,
                );
              },
            });
          })
          .detach();
        }
        let _ = window;
      },
    }
  }
}
