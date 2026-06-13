#[hotpath::measure_all]
impl Workspace {
  pub fn start_collaboration_on_active_document(&mut self, cx: &mut Context<Self>) -> Option<flowstate_collab::SessionId> {
    let panel_id = self.active_document_id?;
    let editor = self.active_editor.clone()?;
    let title = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .map(|panel| panel.read(cx).title_text().to_string())
      .unwrap_or_else(|| "Shared document".to_string());

    match crate::collab::start_session_for_panel(panel_id, editor, title, cx) {
      Ok(session) => Some(session),
      Err(error) => {
        eprintln!("starting collaboration session failed: {error:#}");
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

  pub fn join_collaboration_session(
    &mut self,
    ticket: flowstate_collab::ticket::SessionTicket,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<flowstate_collab::SessionId> {
    let request = match crate::collab::join_session(ticket, cx) {
      Ok(request) => request,
      Err(error) => {
        eprintln!("joining collaboration session failed: {error:#}");
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
      .is_some_and(|panel_id| crate::collab::leave_session_for_panel(panel_id, cx))
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
