impl Workspace {
  pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let this = Self {
      document_panels: Vec::new(),
      flow_panels: Vec::new(),
      active_document_id: None,
      active_editor: None,
      active_flow: None,
      ribbon_collapsed: false,
      outline_collapsed: false,
      toolkit_collapsed: false,
      tab_bar_scroll_handle: ScrollHandle::new(),
      body_resizable_state: cx.new(|_| ResizableState::default()),
      content_resizable_state: cx.new(|_| ResizableState::default()),
      ribbon_resizable_state: cx.new(|_| ResizableState::default()),
      committed_ribbon_height: px(112.0),
      outline_tree: cx.new(|cx| TreeState::new(cx)),
      outline_cache: None,
      collapsed_outline_items: HashSet::new(),
      outline_revision: 0,
      outline_viewport_paragraph: None,
      outline_scrolled_paragraph: None,
      editor_subscriptions: Vec::new(),
      styles_settings_open: false,
      file_search_overlay: None,
    };

    if let Some(path) = initial_path {
      // Initial window creation happens before GPUI has produced stable
      // layout bounds for the resizable document area. Documents opened later
      // already run after that first layout pass, so defer startup loading by
      // one frame to give the initial editor the same settled geometry.
      cx.on_next_frame(window, move |workspace, window, cx| {
        workspace.open_document_path(path, window, cx);
      });
    }

    this
  }

  fn create_document_panel(
    &mut self,
    mut document: Document,
    path: Option<PathBuf>,
    _window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<DocumentPanel> {
    // DB8 stores style assignments, not style appearance. The render theme is
    // local user preference loaded from app settings.
    document.theme = load_document_theme();
    let editor = cx.new(|cx| RichTextEditor::new_with_path(document, path.clone(), cx));
    let smart_word_selection = load_smart_word_selection();
    editor.update(cx, |editor, cx| {
      editor.set_smart_word_selection(smart_word_selection, cx);
    });
    let workspace = cx.entity().downgrade();
    let title = path
      .as_ref()
      .and_then(|path| path.file_name())
      .map(|name| name.to_string_lossy().to_string())
      .or_else(|| Some(self.next_untitled_title(cx)));
    let panel = cx.new(|cx| DocumentPanel::new_with_title(title, path, editor.clone(), workspace, cx));
    let id = panel.read(cx).id();
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, |workspace, _, cx| {
        let viewport_paragraph = workspace.active_editor_viewport_paragraph(cx);
        if workspace.outline_viewport_paragraph != viewport_paragraph {
          workspace.outline_viewport_paragraph = viewport_paragraph;
          cx.notify();
        }
      }),
    ));
    self.active_document_id = Some(id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.document_panels.push(panel.clone());
    panel
  }

  pub fn set_active_document(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    self.active_document_id = Some(panel_id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.refresh_outline_tree(cx);
    cx.notify();
  }

  pub fn set_active_flow(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    self.active_document_id = Some(panel_id);
    self.active_editor = None;
    self.active_flow = Some(editor);
    self.outline_cache = None;
    self.outline_viewport_paragraph = None;
    self.outline_scrolled_paragraph = None;
    cx.notify();
  }

  pub fn remove_document_panel(&mut self, panel_id: Uuid, _: &mut Window, cx: &mut Context<Self>) {
    let closing_active_document = self.active_document_id == Some(panel_id);
    if let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      let editor = panel.read(cx).editor();
      editor.update(cx, |editor, _| editor.dispose_for_close());
    }
    if let Some(panel) = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      let editor = panel.read(cx).editor();
      editor.update(cx, |editor, _| editor.discard_recovery_file());
    }
    self
      .document_panels
      .retain(|panel| panel.read(cx).id() != panel_id);
    self.flow_panels.retain(|panel| panel.read(cx).id() != panel_id);
    self.editor_subscriptions.retain(|(id, _)| *id != panel_id);
    if closing_active_document {
      if let Some(panel) = self.document_panels.last() {
        self.active_document_id = Some(panel.read(cx).id());
        self.active_editor = Some(panel.read(cx).editor());
        self.active_flow = None;
      } else if let Some(panel) = self.flow_panels.last() {
        self.active_document_id = Some(panel.read(cx).id());
        self.active_editor = None;
        self.active_flow = Some(panel.read(cx).editor());
      } else {
        self.active_document_id = None;
        self.active_editor = None;
        self.active_flow = None;
      }
      self.outline_cache = None;
      self.outline_viewport_paragraph = self
        .active_editor
        .as_ref()
        .and_then(|editor| editor.read(cx).viewport_anchor_paragraph());
      self.outline_scrolled_paragraph = None;
    }
    if self.active_document_id.is_none() {
      self.outline_cache = None;
      self.outline_viewport_paragraph = None;
      self.outline_scrolled_paragraph = None;
      self.collapsed_outline_items.clear();
      self
        .outline_tree
        .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
    } else if closing_active_document {
      self.refresh_outline_tree(cx);
    }
    cx.notify();
  }

  pub fn new_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.add_document_panel(new_blank_document(), None, window, cx);
  }

  pub fn new_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.add_flow_panel(flowstate_flow::FlowDocument::new(), None, window, cx);
  }

  pub fn open_demo_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let path = PathBuf::from("data/demo.db8");
    self.open_document_path(path, window, cx);
  }

  pub fn open_document_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let path_for_error = path.clone();
      let loaded = cx
        .background_executor()
        .spawn(async move { load_workspace_document(path) })
        .await;
      match loaded {
        Ok(LoadedWorkspaceDocument::Document { document, path }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.add_document_panel(*document, path, window, cx);
            });
          });
        },
        Ok(LoadedWorkspaceDocument::Flow { document, path }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.add_flow_panel(document, Some(path), window, cx);
            });
          });
        },
        Err(error) => {
          let detail = format!("Failed to open {}: {error}", path_for_error.display());
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Open failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
        },
      }
    })
    .detach();
  }

  pub fn prompt_open_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Open .db8, .docx, or .fl0 document".into()),
    });
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let Ok(Ok(Some(paths))) = paths.await else {
        return;
      };
      let Some(path) = paths.into_iter().next() else {
        return;
      };
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.open_document_path(path, window, cx);
        });
      });
    })
    .detach();
  }

  fn add_document_panel(&mut self, document: Document, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
    self.create_document_panel(document, path, window, cx);
    cx.notify();
  }

  fn create_flow_panel(
    &mut self,
    document: flowstate_flow::FlowDocument,
    path: Option<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<FlowPanel> {
    let editor = cx.new(|cx| FlowEditor::new_with_path(document, path.clone(), window, cx));
    let workspace = cx.entity().downgrade();
    let title = path
      .as_ref()
      .and_then(|path| path.file_name())
      .map(|name| name.to_string_lossy().to_string())
      .or_else(|| Some(self.next_untitled_flow_title(cx)));
    let panel = cx.new(|cx| FlowPanel::new_with_title(title, path, editor.clone(), workspace, window, cx));
    let id = panel.read(cx).id();
    self.active_document_id = Some(id);
    self.active_editor = None;
    self.active_flow = Some(editor);
    self.flow_panels.push(panel.clone());
    self.outline_cache = None;
    self.outline_viewport_paragraph = None;
    self.outline_scrolled_paragraph = None;
    panel
  }

  fn add_flow_panel(
    &mut self,
    document: flowstate_flow::FlowDocument,
    path: Option<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    self.create_flow_panel(document, path, window, cx);
    cx.notify();
  }

  pub fn close_document_panel(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
    let document_panel = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .cloned();
    let flow_panel = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .cloned();
    let Some(panel_kind) = document_panel
      .map(|panel| PanelKind::Document(panel.read(cx).editor()))
      .or_else(|| flow_panel.map(|panel| PanelKind::Flow(panel.read(cx).editor())))
    else {
      return;
    };
    if !panel_kind.is_dirty(cx) {
      self.remove_document_panel(panel_id, window, cx);
      return;
    }

    let answer = window.prompt(
      PromptLevel::Warning,
      "Save changes before closing?",
      Some("This document has unsaved changes."),
      &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
      cx,
    );
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let should_close = match answer.await {
        Ok(0) => match panel_kind.save(cx).await {
          Ok(()) => true,
          Err(error) => {
            eprintln!("failed to save before close: {error}");
            false
          },
        },
        Ok(1) => {
          panel_kind.discard(cx);
          true
        },
        _ => false,
      };

      if should_close {
        let _ = window_handle.update(cx, |_, window, cx| {
          let _ = workspace.update(cx, |workspace, cx| workspace.remove_document_panel(panel_id, window, cx));
        });
      }
    })
    .detach();
  }

  fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let dirty_panels = self.dirty_panels(cx);
    if dirty_panels.is_empty() {
      window.remove_window();
      return;
    }

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
    let window_handle = window.window_handle();

    cx.spawn(async move |_, cx| {
      let should_close = match answer.await {
        Ok(0) => {
          let mut ok = true;
          for panel in dirty_panels {
            match panel.save(cx).await {
              Ok(()) => {},
              Err(error) => {
                ok = false;
                let detail = error;
                let _ = window_handle.update(cx, |_, window, cx| {
                  window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
                });
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

      if should_close {
        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
      }
    })
    .detach();
  }

  pub fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.active_editor.clone() {
      if editor.read(cx).document_path().is_none() {
        self.prompt_save_active_as(editor, window, cx);
        return;
      }
      let save_task = editor.update(cx, |editor, cx| editor.save(cx));
      let window_handle = window.window_handle();
      cx.spawn(async move |_, cx| {
        if let Err(error) = save_task.await {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
        }
      })
      .detach();
      cx.notify();
      return;
    }
    if let Some(editor) = self.active_flow.clone() {
      if editor.read(cx).document_path().is_none() {
        self.prompt_save_active_flow_as(editor, window, cx);
        return;
      }
      let save_task = editor.update(cx, |editor, cx| editor.save(cx));
      let window_handle = window.window_handle();
      cx.spawn(async move |_, cx| {
        if let Err(error) = save_task.await {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
        }
      })
      .detach();
      cx.notify();
    }
  }

  pub fn save_active_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.active_editor.clone() {
      self.prompt_save_active_as(editor, window, cx);
    } else if let Some(editor) = self.active_flow.clone() {
      self.prompt_save_active_flow_as(editor, window, cx);
    }
  }

  pub fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    self.close_document_panel(panel_id, window, cx);
  }

  pub fn open_file_search_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(overlay) = self.file_search_overlay.clone() {
      overlay.update(cx, |overlay, cx| overlay.focus_search(window, cx));
      return;
    }

    let workspace = cx.entity().downgrade();
    let overlay = cx.new(|cx| FileSearchOverlay::new(workspace, window, cx));
    overlay.update(cx, |overlay, cx| overlay.focus_search(window, cx));
    self.file_search_overlay = Some(overlay);
    cx.notify();
  }

  pub fn close_file_search_overlay(&mut self, cx: &mut Context<Self>) {
    self.file_search_overlay = None;
    cx.notify();
  }

  fn prompt_save_active_as(&mut self, editor: Entity<RichTextEditor>, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    let save_path = cx.prompt_for_new_path(&default_save_directory(), Some(UNTITLED_DOCUMENT_NAME));
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let path = match save_path.await {
        Ok(Ok(Some(path))) => normalize_db8_path(path),
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
          return;
        },
        Err(error) => {
          eprintln!("save dialog was canceled before completion: {error}");
          return;
        },
      };

      match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
        Ok(task) => match task.await {
          Ok(()) => {
            let _ = workspace.update(cx, |workspace, cx| {
              if let Some(panel) = workspace
                .document_panels
                .iter()
                .find(|panel| panel.read(cx).id() == panel_id)
              {
                panel.update(cx, |panel, cx| panel.set_path(path, cx));
              }
              cx.notify();
            });
          },
          Err(error) => {
            let detail = error.to_string();
            let _ = window_handle.update(cx, |_, window, cx| {
              window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
            });
          },
        },
        Err(error) => {
          eprintln!("failed to access editor before save: {error}");
        },
      }
    })
    .detach();
  }

  fn prompt_save_active_flow_as(&mut self, editor: Entity<FlowEditor>, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    let save_path = cx.prompt_for_new_path(&default_save_directory(), Some(UNTITLED_FLOW_NAME));
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let path = match save_path.await {
        Ok(Ok(Some(path))) => normalize_fl0_path(path),
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
          return;
        },
        Err(error) => {
          eprintln!("save dialog was canceled before completion: {error}");
          return;
        },
      };

      match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
        Ok(task) => match task.await {
          Ok(()) => {
            let _ = workspace.update(cx, |workspace, cx| {
              if let Some(panel) = workspace
                .flow_panels
                .iter()
                .find(|panel| panel.read(cx).id() == panel_id)
              {
                panel.update(cx, |panel, cx| panel.set_path(path, cx));
              }
              cx.notify();
            });
          },
          Err(error) => {
            let detail = error.to_string();
            let _ = window_handle.update(cx, |_, window, cx| {
              window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
            });
          },
        },
        Err(error) => {
          eprintln!("failed to access flow before save: {error}");
        },
      }
    })
    .detach();
  }

}

#[derive(Clone)]
enum PanelKind {
  Document(Entity<RichTextEditor>),
  Flow(Entity<FlowEditor>),
}

impl PanelKind {
  fn is_dirty(&self, cx: &App) -> bool {
    match self {
      PanelKind::Document(editor) => editor.read(cx).has_unsaved_changes(),
      PanelKind::Flow(editor) => editor.read(cx).has_unsaved_changes(),
    }
  }

  async fn save(&self, cx: &mut gpui::AsyncApp) -> Result<(), String> {
    match self {
      PanelKind::Document(editor) => match editor.update(cx, |editor, cx| editor.save(cx)) {
        Ok(task) => task.await.map_err(|error| error.to_string()),
        Err(error) => Err(format!("failed to access editor before save: {error}")),
      },
      PanelKind::Flow(editor) => match editor.update(cx, |editor, cx| editor.save(cx)) {
        Ok(task) => task.await.map_err(|error| error.to_string()),
        Err(error) => Err(format!("failed to access flow before save: {error}")),
      },
    }
  }

  fn discard(&self, cx: &mut gpui::AsyncApp) {
    match self {
      PanelKind::Document(editor) => {
        let _ = editor.update(cx, |editor, cx| editor.discard_recovery_file(cx));
      },
      PanelKind::Flow(editor) => {
        let _ = editor.update(cx, |editor, _| editor.discard_recovery_file());
      },
    }
  }
}

enum LoadedWorkspaceDocument {
  Document {
    document: Box<Document>,
    path: Option<PathBuf>,
  },
  Flow {
    document: flowstate_flow::FlowDocument,
    path: PathBuf,
  },
}

fn load_workspace_document(path: PathBuf) -> Result<LoadedWorkspaceDocument, String> {
  if is_flow_path(&path) {
    return Ok(LoadedWorkspaceDocument::Flow {
      document: flowstate_flow::load_flow_document_or_new(&path),
      path,
    });
  }
  load_document_for_open(&path)
    .map(|(document, path)| LoadedWorkspaceDocument::Document {
      document: Box::new(document),
      path,
    })
    .map_err(|error| error.to_string())
}

fn is_flow_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("fl0"))
}
