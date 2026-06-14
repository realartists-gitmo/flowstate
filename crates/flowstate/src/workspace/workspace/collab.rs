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

    match crate::collab::start_session_for_panel(panel_id, editor, title, cx) {
      Ok(session) => Some(session),
      Err(error) => {
        tracing::warn!("starting collaboration session failed: {error:#}");
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
          let detail = format!("Creating collaboration invite failed: {error:#}");
          std::mem::drop(window.prompt(PromptLevel::Critical, "Share failed", Some(&detail), &[PromptButton::ok("Ok")], cx));
        },
        Err(error) => {
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
        let detail = format!("The clipboard text is not a valid collaboration invite: {error:#}");
        std::mem::drop(window.prompt(PromptLevel::Critical, "Join failed", Some(&detail), &[PromptButton::ok("Ok")], cx));
        return true;
      },
    };
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
    crate::collab::leave_session_for_panel(panel_id, cx)
  }

  pub fn join_collaboration_session(
    &mut self,
    ticket: flowstate_collab::ticket::SessionTicket,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<flowstate_collab::SessionId> {
    let request = match crate::collab::join_session(ticket, cx) {
      Ok(request) => request,
      Err(error) => {
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
              let panel = workspace.add_joined_collaboration_panel(joined.document, joined.title, window, cx);
              let panel_id = panel.read(cx).id();
              let editor = panel.read(cx).editor();
              if let Err(error) = crate::collab::attach_joined_session(joined.session, panel_id, editor, cx) {
                let detail = format!("Joined document opened, but collaboration attachment failed: {error:#}");
                std::mem::drop(window.prompt(
                  PromptLevel::Critical,
                  "Join failed",
                  Some(&detail),
                  &[PromptButton::ok("Ok")],
                  cx,
                ));
              } else if workspace.collaboration_dialog.is_some() {
                workspace.close_collaboration_dialog(cx);
                window.close_dialog(cx);
              }
            },
            Ok(Err(error)) => {
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
    document: Document,
    title: String,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<DocumentPanel> {
    let panel = self.create_document_panel(document, None, Some(title), window, cx);
    self.persist_temporary_workspace_session(cx);
    cx.notify();
    panel
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
