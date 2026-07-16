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

/// W-S1: the live workspace-window registry. One entry per open window, in
/// creation order. This is what makes the same-file double-open guard and the
/// coordinated quit able to see across windows at all.
#[derive(Default)]
pub struct WorkspaceWindows {
  entries: Vec<(AnyWindowHandle, WeakEntity<Workspace>)>,
}

impl gpui::Global for WorkspaceWindows {}

/// Live (window, workspace) pairs, pruning entries whose workspace has been
/// released (closed windows drop their Root → Workspace chain).
pub fn live_workspace_windows(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
  let registry = cx.default_global::<WorkspaceWindows>();
  registry
    .entries
    .retain(|(_, workspace)| workspace.upgrade().is_some());
  cx.global::<WorkspaceWindows>()
    .entries
    .iter()
    .filter_map(|(handle, workspace)| Some((*handle, workspace.upgrade()?)))
    .collect()
}

fn register_workspace_window(handle: AnyWindowHandle, workspace: WeakEntity<Workspace>, cx: &mut App) {
  cx.default_global::<WorkspaceWindows>()
    .entries
    .push((handle, workspace));
}

/// W-S1: "Quit Flowstate" asks EVERY window to close (each window runs its own
/// collaboration/dirty prompts), not just the focused one. A cancel in any
/// window leaves that window (and the app) alive; the app exits when the last
/// window is gone.
pub fn request_quit_all_windows(cx: &mut App) {
  for (handle, workspace) in live_workspace_windows(cx) {
    let _ = handle.update(cx, |_, window, cx| {
      workspace.update(cx, |workspace, cx| workspace.request_close_window(window, cx));
    });
  }
}

#[hotpath::measure]
pub fn open_workspace_window(document_path: Option<PathBuf>, cx: &mut App) -> WeakEntity<Workspace> {
  let initial_invite = document_path
    .as_ref()
    .and_then(|path| path.to_str())
    .filter(|text| text.starts_with(flowstate_collab::ticket::INVITE_URL_PREFIX))
    .map(flowstate_collab::ticket::SessionTicket::decode_text);
  let document_path = if initial_invite.is_some() { None } else { document_path };
  let window_bounds = startup_window_bounds(cx);
  // W-S1 (W2-B): the first window of the process owns the persisted session;
  // every later window is an ephemeral work surface.
  let session_owner = live_workspace_windows(cx).is_empty();
  let opened_workspace = Rc::new(RefCell::new(None));
  let opened_workspace_slot = opened_workspace.clone();
  cx.open_window(
    WindowOptions {
      window_bounds,
      app_id: Some("dev.flowstate.Flowstate".to_string()),
      titlebar: Some(TitleBar::title_bar_options()),
      window_decorations: window_decorations(),
      ..Default::default()
    },
    |window, cx| {
      window.set_window_title("Flowstate");
      let workspace = cx.new(|cx| Workspace::new(document_path, window, cx));
      // Set before the deferred session restore runs (it is scheduled for the
      // next frame inside `Workspace::new` and gates on this flag).
      workspace.update(cx, |workspace, _| workspace.session_owner = session_owner);
      register_workspace_window(window.window_handle(), workspace.downgrade(), cx);
      if let Some(invite) = initial_invite {
        match invite {
          Ok(ticket) => workspace.update(cx, |workspace, cx| {
            let _ = workspace.join_collaboration_session(ticket, window, cx);
          }),
          Err(error) => {
            let detail = format!("This Flowstate collaboration link is invalid or damaged: {error}");
            std::mem::drop(window.prompt(PromptLevel::Critical, "Invite couldn't be opened", Some(&detail), &[PromptButton::ok("Ok")], cx));
          },
        }
      }
      install_workspace_close_prompt(workspace.clone(), window, cx);
      *opened_workspace_slot.borrow_mut() = Some(workspace.downgrade());
      cx.new(|cx| Root::new(workspace, window, cx))
    },
  )
  .unwrap();
  opened_workspace
    .borrow_mut()
    .take()
    .expect("workspace window builder must install its workspace")
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
