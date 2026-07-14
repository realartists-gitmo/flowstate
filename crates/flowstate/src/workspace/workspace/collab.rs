#[hotpath::measure_all]
impl Workspace {
  pub(crate) fn collaboration_discovery_context(&self, panel_id: Uuid, cx: &App) -> Option<(u128, PathBuf)> {
    if let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      let editor = panel.read(cx).editor();
      let editor = editor.read(cx);
      return Some((editor.document().ids.document_id, editor.document_path()?.clone()));
    }
    // FLOW arm: discovery fingerprints derive from `flow.meta/document_id`.
    let panel = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)?;
    let editor = panel.read(cx).editor();
    let editor = editor.read(cx);
    Some((editor.handle().document_id().ok()?.as_u128(), editor.document_path()?.clone()))
  }

  pub fn open_comment_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else { return };
    let Some(editor) = self.active_editor.clone() else { return };
    let Some(io) = self.document_runtimes.get(&panel_id).cloned() else {
      return;
    };
    if self.comment_dialog.is_some() {
      window.close_dialog(cx);
      self.comment_dialog = None;
    }
    let selection = editor.read(cx).selection().clone();
    let profile = crate::app_settings::load_local_user_profile();
    let dialog = cx
      .new(|cx| crate::workspace::comment_dialog::CommentDialog::new(io, editor, selection, profile.user_id, profile.display_name, window, cx));
    let rendered = dialog.clone();
    let workspace = cx.entity().downgrade();
    window.open_dialog(cx, move |component, _, _| {
      let workspace = workspace.clone();
      component
        .title("Comments")
        .w(px(620.0))
        .max_w(px(620.0))
        .on_close(move |_, _, cx| {
          let _ = workspace.update(cx, |workspace, cx| {
            workspace.comment_dialog = None;
            cx.notify();
          });
        })
        .child(rendered.clone())
    });
    self.comment_dialog = Some(dialog);
    cx.notify();
  }

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
    let panel_id = (self.active_editor.is_some() || self.active_flow.is_some())
      .then_some(self.active_document_id)
      .flatten();
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
    if self.flow_panels.iter().any(|panel| panel.read(cx).id() == panel_id) {
      return self.start_collaboration_on_flow_panel(panel_id, cx);
    }
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
    // Loro-first: nothing is ever pending — local intents commit synchronously
    // into the same gate-protected core the session will now publish from.
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

  /// FLOW arm of session start: what makes "start collaboration on an open
  /// flow tab" free — the editor already commits through its gated authority;
  /// the session only takes the flow I/O handle for transport.
  fn start_collaboration_on_flow_panel(&mut self, panel_id: Uuid, cx: &mut Context<Self>) -> Option<flowstate_collab::SessionId> {
    let panel = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)?;
    let editor = panel.read(cx).editor();
    let title = panel.read(cx).title_text().to_string();
    let runtime = self.flow_document_runtimes.get(&panel_id)?.clone();
    tracing::info!(%panel_id, title = %title, "workspace starting collaboration on flow");
    match crate::collab::start_flow_session_for_panel(panel_id, editor, title, runtime, cx) {
      Ok(session) => {
        tracing::info!(%panel_id, %session, "workspace started collaboration on flow");
        Some(session)
      },
      Err(error) => {
        tracing::error!(%panel_id, error = %format_args!("{error:#}"), "starting flow collaboration session failed");
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
          tracing::info!(session = %ticket.session, bootstrap_count = ticket.bootstrap.len(), "copied collaboration invite ticket to clipboard");
          cx.write_to_clipboard(gpui::ClipboardItem::new_string(ticket.encode_invite_link()));
          std::mem::drop(window.prompt(
            PromptLevel::Info,
            "Invite copied",
            Some("A Flowstate invite link was copied to the clipboard."),
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
    tracing::info!(session = %ticket.session, bootstrap_count = ticket.bootstrap.len(), "joining collaboration session from clipboard");
    self
      .join_collaboration_session(ticket, window, cx)
      .is_some()
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
    // Loro-first: leaving a session stops transport only. The document's write
    // authority, gate, and I/O service are untouched — solo editing continues
    // through the identical path (invariant 5). Nothing to re-attach.
    let left = crate::collab::leave_session_for_panel(panel_id, cx);
    tracing::info!(%panel_id, left, "workspace leave collaboration requested");
    left
  }

  pub fn join_collaboration_session(
    &mut self,
    ticket: flowstate_collab::ticket::SessionTicket,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<flowstate_collab::SessionId> {
    tracing::info!(session = %ticket.session, bootstrap_count = ticket.bootstrap.len(), "workspace joining collaboration session");
    let request = match crate::collab::join_session(ticket, cx) {
      Ok(request) => request,
      Err(error) => {
        tracing::error!(error = %format_args!("{error:#}"), "joining collaboration session failed");
        let detail = format!("Joining collaboration session failed: {error:#}");
        std::mem::drop(window.prompt(PromptLevel::Critical, "Join failed", Some(&detail), &[PromptButton::ok("Ok")], cx));
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
              // Loro-first (spec §3 join gate): the session built the document
              // services from the initial snapshot import; the workspace now
              // takes the write authority + I/O handle and wires the panel
              // exactly like a solo document (invariant 5). The session keeps
              // transport only.
              let Some((authority, io)) = crate::collab::take_joined_document_services_for_session(joined.session, cx) else {
                tracing::error!(session = %joined.session, "joined collaboration document services are unavailable");
                std::mem::drop(window.prompt(
                  PromptLevel::Critical,
                  "Join failed",
                  Some("The joined collaboration document is unavailable."),
                  &[PromptButton::ok("Ok")],
                  cx,
                ));
                return;
              };
              use crate::collab::{CollabEditor, JoinedAuthority, JoinedDocumentPayload};
              let (panel_id, editor) = match (joined.payload, authority, io) {
                (JoinedDocumentPayload::RichText(document), JoinedAuthority::RichText(authority), flowstate_collab::SyncIoHandle::RichText(io)) => {
                  let attachment = DocumentRuntimeAttachment { authority, io };
                  let panel = match workspace.add_joined_collaboration_panel(*document, joined.title, attachment, window, cx) {
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
                  (panel.read(cx).id(), CollabEditor::RichText(panel.read(cx).editor()))
                },
                (JoinedDocumentPayload::Flow(_board), JoinedAuthority::Flow(authority), flowstate_collab::SyncIoHandle::Flow(io)) => {
                  let panel = workspace.add_joined_collaboration_flow_panel(authority, io, joined.title, window, cx);
                  (panel.read(cx).id(), CollabEditor::Flow(panel.read(cx).editor()))
                },
                _ => {
                  tracing::error!(session = %joined.session, "joined collaboration services have mismatched document kinds");
                  std::mem::drop(window.prompt(
                    PromptLevel::Critical,
                    "Join failed",
                    Some("The joined collaboration document kind is inconsistent."),
                    &[PromptButton::ok("Ok")],
                    cx,
                  ));
                  return;
                },
              };
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
              } else if workspace.collaboration_dialog.is_some() {
                // The panel was fully wired by `create_document_panel` (write
                // authority, I/O hooks, runtime map) — no second attach pass.
                workspace.close_collaboration_dialog(cx);
                window.close_dialog(cx);
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
    attachment: DocumentRuntimeAttachment,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> anyhow::Result<Entity<DocumentPanel>> {
    let panel = self.create_document_panel(document, None, Some(title), DocumentRuntimeSource::Attachment(attachment), window, cx)?;
    self.persist_temporary_workspace_session(cx);
    cx.notify();
    Ok(panel)
  }

  /// FLOW join sibling of [`Self::add_joined_collaboration_panel`]: the
  /// session built authority + I/O from the snapshot; the panel wires them
  /// exactly like a solo flow tab (invariant 5) — pathless, so autosave skips
  /// it and the session's recovery hooks cover crashes.
  fn add_joined_collaboration_flow_panel(
    &mut self,
    authority: std::sync::Arc<flowstate_collab::flow::FlowDocHandle>,
    io: flowstate_collab::flow::FlowIoHandle,
    title: String,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<crate::flow::FlowPanel> {
    let panel = self.create_flow_panel_from_attachment(authority, io, Some(title), window, cx);
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
