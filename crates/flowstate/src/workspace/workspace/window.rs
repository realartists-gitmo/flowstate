#[hotpath::measure]
pub fn install_workspace_close_prompt(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) {
  let prompt_open = Rc::new(Cell::new(false));
  let allow_close = Rc::new(Cell::new(false));
  let window_handle = window.window_handle();

  window.on_window_should_close(cx, move |window, cx| {
    if allow_close.get() {
      return true;
    }

    let collaboration_panels = workspace.read(cx).collaboration_close_panels(cx);
    if !collaboration_panels.is_empty() {
      if prompt_open.get() {
        return false;
      }
      prompt_open.set(true);

      let collaboration_ids = collaboration_panels
        .iter()
        .map(|panel| panel.id)
        .collect::<HashSet<_>>();
      let dirty_panels = workspace
        .read(cx)
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
      let prompt_open = prompt_open.clone();
      let allow_close = allow_close.clone();
      let workspace = workspace.clone();

      cx.spawn(async move |cx| {
        let mut should_close = false;
        if matches!(answer.await, Ok(0)) {
          let mut discards = Vec::new();
          let mut ok = true;
          for panel in collaboration_panels {
            match resolve_collaboration_save(panel.panel, panel.save_prompt, window_handle, cx).await {
              CollaborationCloseResolution::Proceed { discard } => {
                if let Some(discard) = discard {
                  discards.push(discard);
                }
              },
              CollaborationCloseResolution::Cancelled => {
                ok = false;
                break;
              },
            }
          }
          if ok && resolve_dirty_window_close(dirty_panels, window_handle, cx).await {
            for panel in discards {
              panel.discard(cx);
            }
            should_close = true;
          }
        }

        prompt_open.set(false);
        if should_close {
          allow_close.set(true);
          let _ = window_handle.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| workspace.leave_all_collaboration_sessions(cx));
            window.remove_window();
          });
        }
      })
      .detach();

      return false;
    }

    let dirty_panels = workspace.read(cx).dirty_panels(cx);
    if dirty_panels.is_empty() {
      workspace.update(cx, |workspace, cx| workspace.leave_all_collaboration_sessions(cx));
      return true;
    }

    if prompt_open.get() {
      return false;
    }
    prompt_open.set(true);

    let message = if dirty_panels.len() == 1 {
      "This document has unsaved changes."
    } else {
      "One or more documents have unsaved changes."
    };
    let answer = window.prompt(
      PromptLevel::Warning,
      "Save changes before closing?",
      Some(message),
      &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
      cx,
    );
    let prompt_open = prompt_open.clone();
    let allow_close = allow_close.clone();
    let workspace = workspace.clone();

    cx.spawn(async move |cx| {
      let should_close = match answer.await {
        Ok(0) => {
          let mut ok = true;
          for panel in dirty_panels {
            match panel.save(window_handle, cx).await {
              PanelSaveOutcome::Saved => {},
              PanelSaveOutcome::Cancelled => {
                ok = false;
                break;
              },
              PanelSaveOutcome::Failed(error) => {
                ok = false;
                show_save_failed(window_handle, cx, error);
                break;
              },
            }
          }
          ok
        },
        Ok(1) => {
          for panel in dirty_panels {
            panel.discard(cx);
          }
          true
        },
        _ => false,
      };

      prompt_open.set(false);
      if should_close {
        let _ = workspace.update(cx, |workspace, cx| workspace.leave_all_collaboration_sessions(cx));
        allow_close.set(true);
        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
      }
    })
    .detach();

    false
  });
}

#[hotpath::measure]
pub fn open_workspace_window(document_path: Option<PathBuf>, cx: &mut App) {
  let window_bounds = startup_window_bounds(cx);
  cx.open_window(
    WindowOptions {
      window_bounds,
      titlebar: Some(TitleBar::title_bar_options()),
      window_decorations: window_decorations(),
      ..Default::default()
    },
    |window, cx| {
      window.set_window_title("Flowstate");
      let workspace = cx.new(|cx| Workspace::new(document_path, window, cx));
      install_workspace_close_prompt(workspace.clone(), window, cx);
      cx.new(|cx| Root::new(workspace, window, cx))
    },
  )
  .unwrap();
}

#[cfg(target_os = "windows")]
fn startup_window_bounds(cx: &mut App) -> Option<WindowBounds> {
  Some(WindowBounds::Maximized(Bounds::centered(
    None,
    size(px(1200.0), px(800.0)),
    cx,
  )))
}

#[cfg(not(target_os = "windows"))]
fn startup_window_bounds(_: &mut App) -> Option<WindowBounds> {
  None
}

#[cfg(target_os = "linux")]
fn window_decorations() -> Option<WindowDecorations> {
  Some(WindowDecorations::Client)
}

#[cfg(not(target_os = "linux"))]
fn window_decorations() -> Option<WindowDecorations> {
  None
}
