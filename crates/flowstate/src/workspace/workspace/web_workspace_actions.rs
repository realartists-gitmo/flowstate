impl Workspace {
  fn persist_temporary_workspace_session(&mut self, _: &mut Context<Self>) {}

  pub fn set_active_document(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    self.active_document_id = Some(panel_id);
    self.active_editor = Some(editor);
    cx.notify();
  }

  pub fn remove_document_panel(&mut self, panel_id: Uuid, _: &mut Window, cx: &mut Context<Self>) {
    self.document_panels.retain(|panel| panel.read(cx).id() != panel_id);
    if self.active_document_id == Some(panel_id) {
      let next = self.document_panels.last().cloned();
      self.active_document_id = next.as_ref().map(|panel| panel.read(cx).id());
      self.active_editor = next.map(|panel| panel.read(cx).editor());
    }
    cx.notify();
  }

  pub fn close_document_panel(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
    self.remove_document_panel(panel_id, window, cx);
  }

  pub fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(id) = self.active_document_id {
      self.close_document_panel(id, window, cx);
    }
  }

  pub fn new_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let document = document_from_input(flowstate_document_theme(), Vec::new());
    let editor = cx.new(|cx| RichTextEditor::new_with_path(document, None, cx));
    let workspace = cx.entity().downgrade();
    let panel = cx.new(|cx| {
      DocumentPanel::new_with_title(
        Some("Untitled.db8".to_string()),
        None,
        editor.clone(),
        workspace,
        window,
        cx,
      )
    });
    let id = panel.read(cx).id();
    self.document_panels.push(panel);
    self.set_active_document(id, editor, cx);
  }

  pub fn toggle_speech_document(&mut self, _: Uuid, _: &mut Context<Self>) {}
  pub fn send_selection_to_speech_document(&mut self, _: &mut Window, _: &mut Context<Self>) -> bool { false }
  pub fn send_selection_to_speech_document_end(&mut self, _: &mut Window, _: &mut Context<Self>) -> bool { false }
  pub fn request_close_window(&mut self, window: &mut Window, _: &mut Context<Self>) { window.remove_window(); }
  pub fn new_flow(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn save_active(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn save_active_as(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn prompt_open_document(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn open_document_path(&mut self, _: PathBuf, _: &mut Window, _: &mut Context<Self>) {}
  pub fn open_file_search_overlay(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn open_collaboration_dialog(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn open_join_collaboration_dialog(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn open_comment_dialog(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn open_revision_dialog(&mut self, _: &mut Window, _: &mut Context<Self>) {}
  pub fn copy_active_collaboration_ticket(&mut self, _: &mut Window, _: &mut Context<Self>) -> bool { false }
  pub fn join_collaboration_from_clipboard(&mut self, _: &mut Window, _: &mut Context<Self>) -> bool { false }
  pub fn confirm_leave_collaboration_on_active_document(&mut self, _: &mut Window, _: &mut Context<Self>) -> bool { false }
}
