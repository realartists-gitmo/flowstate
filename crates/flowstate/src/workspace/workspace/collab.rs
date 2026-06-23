#[hotpath::measure_all]
impl Workspace {
  pub fn open_collaboration_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.open_collaboration_dialog_with_mode(crate::collab::share_dialog::CollabDialogMode::Share, window, cx);
  }

  pub fn open_join_collaboration_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.open_collaboration_dialog_with_mode(crate::collab::share_dialog::CollabDialogMode::Join, window, cx);
  }

  pub fn close_collaboration_dialog(&mut self, cx: &mut Context<Self>) {
    self.collaboration_dialog = None;
    cx.notify();
  }

  fn open_collaboration_dialog_with_mode(
    &mut self,
    mode: crate::collab::share_dialog::CollabDialogMode,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    if self.collaboration_dialog.is_some() {
      window.close_dialog(cx);
      self.collaboration_dialog = None;
    }
    let workspace = cx.entity().downgrade();
    let panel_id = self.active_editor.as_ref().and(self.active_document_id);
    let dialog = cx.new(|cx| crate::collab::share_dialog::CollabShareDialog::new(workspace, panel_id, mode, window, cx));
    let dialog_for_render = dialog.clone();
    let workspace_for_close = cx.entity().downgrade();
    window.open_dialog(cx, move |component_dialog, _, _| {
      let workspace_for_close = workspace_for_close.clone();
      component_dialog
        .title("Share / Collaborate")
        .w(px(620.0))
        .max_w(px(620.0))
        .on_close(move |_, _, cx| {
          let _ = workspace_for_close.update(cx, |workspace, cx| workspace.close_collaboration_dialog(cx));
        })
        .child(dialog_for_render.clone())
    });
    dialog.update(cx, |dialog, cx| dialog.focus(window, cx));
    self.collaboration_dialog = Some(dialog);
    cx.notify();
  }

  pub fn start_collaboration_on_active_document(&mut self, cx: &mut Context<Self>) -> Option<flowstate_collab::SessionId> {
    self.start_collaboration_on_document(self.active_document_id?, cx)
  }

  pub fn start_collaboration_on_document(&mut self, panel_id: Uuid, cx: &mut Context<Self>) -> Option<flowstate_collab::SessionId> {
    let panel = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)?;
    let editor = panel.read(cx).editor();
    let title = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .map(|panel| panel.read(cx).title_text().to_string())
      .unwrap_or_else(|| "Shared document".to_string());
    let runtime = self.document_runtimes.get(&panel_id)?.clone();

    tracing::info!(%panel_id, title = %title, "workspace starting collaboration on document");
    self.flush_document_runtime_edits(panel_id, editor.clone(), cx);
    if editor.read(cx).runtime_edit_in_flight() {
      tracing::warn!(%panel_id, "collaboration start deferred because local edits are still being committed to Loro");
      return None;
    }
    match crate::collab::start_session_for_panel(panel_id, editor, title, runtime, cx) {
      Ok(session) => {
        tracing::info!(%panel_id, %session, "workspace started collaboration on document");
        Some(session)
      },
      Err(error) => {
        tracing::error!(%panel_id, error = %format_args!("{error:#}"), "starting collaboration session failed");
        None
      },
    }
  }

  pub fn request_active_collaboration_ticket(
    &mut self,
    cx: &mut Context<Self>,
  ) -> Option<async_channel::Receiver<anyhow::Result<flowstate_collab::ticket::SessionTicket>>> {
    crate::collab::request_ticket_for_panel(self.active_document_id?, cx)
  }

  pub fn copy_active_collaboration_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(panel_id) = self.active_document_id else {
      return false;
    };
    let mut ticket_rx = crate::collab::request_ticket_for_panel(panel_id, cx);
    if ticket_rx.is_none() {
      if self.start_collaboration_on_active_document(cx).is_none() {
        std::mem::drop(window.prompt(
          PromptLevel::Critical,
          "Share failed",
          Some("The active document could not be shared."),
          &[PromptButton::ok("Ok")],
          cx,
        ));
        return true;
      }
      ticket_rx = crate::collab::request_ticket_for_panel(panel_id, cx);
    }

    let Some(ticket_rx) = ticket_rx else {
      std::mem::drop(window.prompt(
        PromptLevel::Critical,
        "Share failed",
        Some("The collaboration ticket is not available yet."),
        &[PromptButton::ok("Ok")],
        cx,
      ));
      return true;
    };

    let window_handle = window.window_handle();
    cx.spawn(async move |_, cx| {
      let result = ticket_rx.recv().await;
      let _ = window_handle.update(cx, |_, window, cx| match result {
        Ok(Ok(ticket)) => {
          tracing::info!(session = %ticket.session, inviter = %ticket.inviter.id, "copied collaboration invite ticket to clipboard");
          cx.write_to_clipboard(gpui::ClipboardItem::new_string(ticket.encode_text()));
          std::mem::drop(window.prompt(
            PromptLevel::Info,
            "Invite copied",
            Some("The collaboration invite ticket was copied to the clipboard."),
            &[PromptButton::ok("Ok")],
            cx,
          ));
        },
        Ok(Err(error)) => {
          tracing::error!(error = %format_args!("{error:#}"), "creating collaboration invite failed");
          let detail = format!("Creating collaboration invite failed: {error:#}");
          std::mem::drop(window.prompt(PromptLevel::Critical, "Share failed", Some(&detail), &[PromptButton::ok("Ok")], cx));
        },
        Err(error) => {
          tracing::error!(error = %error, "collaboration invite receiver closed");
          let detail = format!("Creating collaboration invite failed: {error}");
          std::mem::drop(window.prompt(PromptLevel::Critical, "Share failed", Some(&detail), &[PromptButton::ok("Ok")], cx));
        },
      });
    })
    .detach();
    true
  }

  pub fn join_collaboration_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(item) = cx.read_from_clipboard() else {
      tracing::warn!("join collaboration from clipboard failed because clipboard is empty");
      std::mem::drop(window.prompt(
        PromptLevel::Critical,
        "Join failed",
        Some("The clipboard does not contain a collaboration invite ticket."),
        &[PromptButton::ok("Ok")],
        cx,
      ));
      return true;
    };
    let Some(text) = item.text() else {
      tracing::warn!("join collaboration from clipboard failed because clipboard has no text");
      std::mem::drop(window.prompt(
        PromptLevel::Critical,
        "Join failed",
        Some("The clipboard does not contain text."),
        &[PromptButton::ok("Ok")],
        cx,
      ));
      return true;
    };
    let ticket = match flowstate_collab::ticket::SessionTicket::decode_text(&text) {
      Ok(ticket) => ticket,
      Err(error) => {
        tracing::warn!(bytes = text.len(), error = %format_args!("{error:#}"), "clipboard collaboration invite decode failed");
        let detail = format!("The clipboard text is not a valid collaboration invite: {error:#}");
        std::mem::drop(window.prompt(PromptLevel::Critical, "Join failed", Some(&detail), &[PromptButton::ok("Ok")], cx));
        return true;
      },
    };
    tracing::info!(session = %ticket.session, inviter = %ticket.inviter.id, "joining collaboration session from clipboard");
    self.join_collaboration_session(ticket, window, cx).is_some()
  }

  pub fn confirm_leave_collaboration_on_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    self
      .active_document_id
      .is_some_and(|panel_id| self.confirm_leave_collaboration_on_panel(panel_id, window, cx))
  }

  pub fn confirm_leave_collaboration_on_panel(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let phase = crate::collab::phase_for_panel(panel_id, cx);
    if !collaboration_phase_blocks_close(phase.as_ref()) {
      return false;
    }
    let detail = format!("{} Your copy of the document stays open.", collaboration_leave_detail(phase.as_ref()));
    let answer = window.prompt(
      PromptLevel::Warning,
      "Leave this session?",
      Some(&detail),
      &[PromptButton::ok("Leave"), PromptButton::cancel("Cancel")],
      cx,
    );
    cx.spawn(async move |workspace, cx| {
      if !matches!(answer.await, Ok(0)) {
        return;
      }
      let _ = workspace.update(cx, |workspace, cx| {
        workspace.leave_collaboration_on_panel(panel_id, cx);
      });
    })
    .detach();
    true
  }

  pub fn leave_collaboration_on_panel(&mut self, panel_id: Uuid, cx: &mut Context<Self>) -> bool {
    let left = crate::collab::leave_session_for_panel(panel_id, cx);
    if left
      && let Some(runtime) = self.document_runtimes.get(&panel_id).cloned()
    {
      self.attach_runtime_to_document_panel(panel_id, runtime, cx);
    }
    tracing::info!(%panel_id, left, "workspace leave collaboration requested");
    left
  }

  pub fn join_collaboration_session(
    &mut self,
    ticket: flowstate_collab::ticket::SessionTicket,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<flowstate_collab::SessionId> {
    tracing::info!(session = %ticket.session, inviter = %ticket.inviter.id, "workspace joining collaboration session");
    let request = match crate::collab::join_session(ticket, cx) {
      Ok(request) => request,
      Err(error) => {
        tracing::error!(error = %format_args!("{error:#}"), "joining collaboration session failed");
        let detail = format!("Joining collaboration session failed: {error:#}");
        std::mem::drop(window.prompt(
          PromptLevel::Critical,
          "Join failed",
          Some(&detail),
          &[PromptButton::ok("Ok")],
          cx,
        ));
        return None;
      },
    };
    let session = request.session;
    let completed = request.completed;
    let window_handle = window.window_handle();

    cx.spawn(async move |workspace, cx| {
      let result = completed.recv().await;
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          match result {
            Ok(Ok(joined)) => {
              tracing::info!(session = %joined.session, title = %joined.title, "collaboration join completed; opening joined document");
              let Some(runtime) = crate::collab::runtime_for_session(joined.session, cx) else {
                tracing::error!(session = %joined.session, "joined collaboration runtime is unavailable");
                std::mem::drop(window.prompt(
                  PromptLevel::Critical,
                  "Join failed",
                  Some("The joined collaboration runtime is unavailable."),
                  &[PromptButton::ok("Ok")],
                  cx,
                ));
                return;
              };
              let panel = match workspace.add_joined_collaboration_panel(joined.document, joined.title, runtime.clone(), window, cx) {
                Ok(panel) => panel,
                Err(error) => {
                  tracing::error!(session = %joined.session, error = %format_args!("{error:#}"), "starting joined document runtime failed");
                  let detail = format!("Joined document runtime could not be started: {error:#}");
                  std::mem::drop(window.prompt(
                    PromptLevel::Critical,
                    "Join failed",
                    Some(&detail),
                    &[PromptButton::ok("Ok")],
                    cx,
                  ));
                  return;
                },
              };
              let panel_id = panel.read(cx).id();
              let editor = panel.read(cx).editor();
              if let Err(error) = crate::collab::attach_joined_session(joined.session, panel_id, editor, cx) {
                tracing::error!(session = %joined.session, %panel_id, error = %format_args!("{error:#}"), "collaboration joined document attachment failed");
                let detail = format!("Joined document opened, but collaboration attachment failed: {error:#}");
                std::mem::drop(window.prompt(
                  PromptLevel::Critical,
                  "Join failed",
                  Some(&detail),
                  &[PromptButton::ok("Ok")],
                  cx,
                ));
              } else {
                workspace.attach_runtime_to_document_panel(panel_id, runtime, cx);
                if workspace.collaboration_dialog.is_some() {
                  workspace.close_collaboration_dialog(cx);
                  window.close_dialog(cx);
                }
              }
            },
            Ok(Err(error)) => {
              tracing::error!(%session, error = %format_args!("{error:#}"), "joining collaboration session failed");
              let detail = format!("Joining collaboration session failed: {error:#}");
              std::mem::drop(window.prompt(
                PromptLevel::Critical,
                "Join failed",
                Some(&detail),
                &[PromptButton::ok("Ok")],
                cx,
              ));
            },
            Err(error) => {
              tracing::error!(%session, error = %error, "collaboration join completion channel closed");
              let detail = format!("Joining collaboration session failed: {error}");
              std::mem::drop(window.prompt(
                PromptLevel::Critical,
                "Join failed",
                Some(&detail),
                &[PromptButton::ok("Ok")],
                cx,
              ));
            },
          }
        });
      });
    })
    .detach();

    Some(session)
  }

  fn add_joined_collaboration_panel(
    &mut self,
    document: DocumentProjection,
    title: String,
    runtime: flowstate_collab::crdt_runtime_actor::CrdtRuntimeHandle,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> anyhow::Result<Entity<DocumentPanel>> {
    let panel = self.create_document_panel(document, None, Some(title), DocumentRuntimeSource::Handle(runtime), window, cx)?;
    self.persist_temporary_workspace_session(cx);
    cx.notify();
    Ok(panel)
  }

  pub fn leave_collaboration_on_active_document(&mut self, cx: &mut Context<Self>) -> bool {
    self
      .active_document_id
      .is_some_and(|panel_id| self.leave_collaboration_on_panel(panel_id, cx))
  }

  pub fn leave_all_collaboration_sessions(&mut self, cx: &mut Context<Self>) {
    let panel_ids = self
      .document_panels
      .iter()
      .map(|panel| panel.read(cx).id())
      .collect::<Vec<_>>();
    for panel_id in panel_ids {
      crate::collab::leave_session_for_panel(panel_id, cx);
    }
  }
}
