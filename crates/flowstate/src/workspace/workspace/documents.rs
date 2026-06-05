#[hotpath::measure_all]
impl Workspace {
  pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let zoom_slider = cx.new(|_| {
      SliderState::new()
        .min(25.0)
        .max(400.0)
        .step(5.0)
        .default_value(100.0)
    });
    let zoom_slider_subscription = cx.subscribe(&zoom_slider, |workspace, _, event: &SliderEvent, cx| {
      let SliderEvent::Change(SliderValue::Single(percent)) = event else {
        return;
      };
      if let Some(editor) = workspace.active_editor.clone() {
        editor.update(cx, |editor, cx| {
          editor.set_zoom_percent(*percent, cx);
        });
      }
    });
    let workspace = cx.entity().downgrade();
    let window_handle = window.window_handle();
    let keybinding_interceptor = cx.intercept_keystrokes(move |event, window, cx| {
      if window.window_handle() != window_handle {
        return;
      }
      let Some(command) = workspace_command_for_keystroke(&event.keystroke) else {
        return;
      };
      if workspace
        .update(cx, |workspace, cx| workspace.handle_window_keybinding(command, window, cx))
        .unwrap_or(false)
      {
        cx.stop_propagation();
      }
    });
    let toolkit_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tub blocks, tags, and analytics"));
    let tub_file_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tub"));
    let _tub_file_search_subscription = cx.subscribe(&tub_file_search_input, |workspace, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        workspace.refresh_tub_file_search(cx);
      }
    });
    let _toolkit_search_subscription = cx.subscribe(&toolkit_search_input, |workspace, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        workspace.refresh_toolkit_search(cx);
      }
    });

    let mut this = Self {
      document_panels: Vec::new(),
      flow_panels: Vec::new(),
      active_document_id: None,
      active_editor: None,
      active_flow: None,
      ribbon_collapsed: false,
      outline_collapsed: false,
      toolkit_collapsed: false,
      active_toolkit_tool: None,
      recent_documents: load_recent_documents(),
      recent_document_previews: HashMap::new(),
      recent_document_preview_generation: 0,
      temporary_workspace_session_pending: None,
      temporary_workspace_session_persist_scheduled: false,
      left_nav_mode: LeftNavMode::Outline,
      tab_bar_scroll_handle: ScrollHandle::new(),
      pinned_document_ids: Vec::new(),
      speech_document_id: None,
      speech_word_count_cache: HashMap::new(),
      body_resizable_state: cx.new(|_| ResizableState::default()),
      content_resizable_state: cx.new(|_| ResizableState::default()),
      ribbon_resizable_state: cx.new(|_| ResizableState::default()),
      committed_ribbon_height: px(112.0),
      outline_tree: cx.new(|cx| TreeState::new(cx)),
      outline_cache: None,
      collapsed_outline_items: HashSet::new(),
      outline_revision: 0,
      outline_viewport_paragraph: None,
      outline_active_paragraph: None,
      outline_scrolled_paragraph: None,
      editor_subscriptions: Vec::new(),
      settings_overlay: None,
      document_style_picker_revision: 0,
      document_style_section: DocumentStyleSection::Text,
      settings_section: WorkspaceSettingsSection::General,
      autosave_enabled: load_autosave(),
      autosave_document_generations: HashMap::new(),
      autosave_flow_in_flight: HashSet::new(),
      file_search_overlay: None,
      tub_root: None,
      tub_index: None,
      tub_files: Vec::new(),
      tub_tree: cx.new(|cx| TreeState::new(cx)),
      tub_tree_items: Vec::new(),
      tub_tree_entries: Vec::new(),
      tub_expanded_dirs: HashSet::new(),
      tub_file_search_input,
      tub_file_search_generation: 0,
      tub_status: "No tub selected".into(),
      tub_watcher: None,
      tub_watch_polling: false,
      tub_scan_in_flight: false,
      tub_scan_pending: false,
      active_tub_path: None,
      toolkit_search_input,
      toolkit_search_filter: ToolkitSearchFilter::All,
      toolkit_hits: Vec::new(),
      expanded_toolkit_hits: HashSet::new(),
      toolkit_results_scroll_handle: VirtualListScrollHandle::new(),
      toolkit_status: "Select a tub to search evidence.".into(),
      toolkit_search_generation: 0,
      _tub_file_search_subscription,
      _toolkit_search_subscription,
      zoom_slider,
      _zoom_slider_subscription: zoom_slider_subscription,
      _keybinding_interceptor: keybinding_interceptor,
    };

    this.refresh_recent_document_previews(cx);

    if let Some(root) = load_tub_root() {
      this.load_tub_root(root, cx);
    }

    if let Some(path) = initial_path {
      // Initial window creation happens before GPUI has produced stable
      // layout bounds for the resizable document area. Documents opened later
      // already run after that first layout pass, so defer startup loading by
      // one frame to give the initial editor the same settled geometry.
      cx.on_next_frame(window, move |workspace, window, cx| {
        workspace.open_document_path(path, window, cx);
      });
    } else if let Some(session) = load_temporary_workspace_session() {
      cx.on_next_frame(window, move |workspace, window, cx| {
        workspace.restore_temporary_workspace_session(session, window, cx);
      });
    }

    this
  }

  fn create_document_panel(
    &mut self,
    mut document: Document,
    path: Option<PathBuf>,
    title: Option<String>,
    window: &mut Window,
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
    let title = title
      .or_else(|| {
        path
          .as_ref()
          .and_then(|path| path.file_name())
          .map(|name| name.to_string_lossy().to_string())
      })
      .or_else(|| Some(self.next_untitled_title(cx)));
    if let Some(title) = title.clone() {
      editor.update(cx, |editor, cx| {
        editor.set_document_display_name(title.into(), cx);
      });
    }
    let panel = cx.new(|cx| DocumentPanel::new_with_title(title, path, editor.clone(), workspace, window, cx));
    let id = panel.read(cx).id();
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, move |workspace, editor, cx| {
        let viewport_paragraph = workspace.active_editor_viewport_paragraph(cx);
        workspace.update_outline_viewport_paragraph(viewport_paragraph, cx);
        workspace.maybe_autosave_document(id, editor.clone(), cx);
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
    self.outline_viewport_paragraph = self.active_editor_viewport_paragraph(cx);
    self.outline_active_paragraph = None;
    self.outline_scrolled_paragraph = None;
    self.refresh_outline_tree(cx);
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  pub fn set_active_flow(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    self.active_document_id = Some(panel_id);
    self.active_editor = None;
    self.active_flow = Some(editor);
    self.outline_cache = None;
    self.outline_viewport_paragraph = None;
    self.outline_active_paragraph = None;
    self.outline_scrolled_paragraph = None;
    self.persist_temporary_workspace_session(cx);
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
    self
      .flow_panels
      .retain(|panel| panel.read(cx).id() != panel_id);
    self.editor_subscriptions.retain(|(id, _)| *id != panel_id);
    self.pinned_document_ids.retain(|id| *id != panel_id);
    self.speech_word_count_cache.remove(&panel_id);
    if self.speech_document_id == Some(panel_id) {
      self.speech_document_id = None;
    }
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
      self.outline_active_paragraph = None;
      self.outline_scrolled_paragraph = None;
    }
    self.persist_temporary_workspace_session(cx);

    if self.active_document_id.is_none() {
      self.outline_cache = None;
      self.outline_viewport_paragraph = None;
      self.outline_active_paragraph = None;
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
    self.open_document_path_with_target(path, None, window, cx);
  }

  pub fn open_document_path_at_paragraph(&mut self, path: PathBuf, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    self.open_document_path_with_target(path, Some(paragraph_ix), window, cx);
  }

  fn open_document_path_with_target(&mut self, path: PathBuf, target_paragraph_ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
    let window_handle = window.window_handle();
    let path_for_recent = path.clone();
    cx.spawn(async move |workspace, cx| {
      let path_for_error = path.clone();
      let loaded = cx
        .background_executor()
        .spawn(async move { load_workspace_document(path) })
        .await;
      match loaded {
        Ok(LoadedWorkspaceDocument::Document { document, path, title }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.record_recent_document(path_for_recent.clone(), cx);
              workspace.add_document_panel_with_title(*document, path, title, window, cx);
              if let Some(paragraph_ix) = target_paragraph_ix {
                workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx);
                cx.on_next_frame(window, move |workspace, window, cx| {
                  workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx);
                });
              }
            });
          });
        },
        Ok(LoadedWorkspaceDocument::Flow { document, path }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.record_recent_document(path.clone(), cx);
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

  fn persist_temporary_workspace_session(&mut self, cx: &mut Context<Self>) {
    let mut entries = Vec::new();
    let mut active_index = None;

    for panel in &self.document_panels {
      let panel = panel.read(cx);
      let Some(path) = panel.editor().read(cx).document_path().cloned() else {
        continue;
      };
      if Some(panel.id()) == self.active_document_id {
        active_index = Some(entries.len());
      }
      entries.push(TemporaryWorkspaceSessionEntry {
        kind: TemporaryWorkspaceSessionEntryKind::Document,
        path,
      });
    }

    for panel in &self.flow_panels {
      let panel = panel.read(cx);
      let Some(path) = panel.editor().read(cx).document_path().cloned() else {
        continue;
      };
      if Some(panel.id()) == self.active_document_id {
        active_index = Some(entries.len());
      }
      entries.push(TemporaryWorkspaceSessionEntry {
        kind: TemporaryWorkspaceSessionEntryKind::Flow,
        path,
      });
    }

    self.temporary_workspace_session_pending = Some(TemporaryWorkspaceSession { entries, active_index });
    if self.temporary_workspace_session_persist_scheduled {
      return;
    }
    self.temporary_workspace_session_persist_scheduled = true;

    cx.spawn(async move |workspace, cx| {
      cx.background_executor()
        .timer(Duration::from_millis(150))
        .await;
      let session = workspace.update(cx, |workspace, _| {
        workspace.temporary_workspace_session_persist_scheduled = false;
        workspace.temporary_workspace_session_pending.take()
      });
      let Ok(Some(session)) = session else {
        return;
      };
      cx.background_executor()
        .spawn(async move { persist_temporary_workspace_session_to_disk(session) })
        .await;
    })
    .detach();
  }

  fn restore_temporary_workspace_session(&mut self, session: TemporaryWorkspaceSession, window: &mut Window, cx: &mut Context<Self>) {
    let active_index = session.active_index;
    let mut active_id = None;
    for (entry_index, entry) in session.entries.into_iter().enumerate() {
      if !entry.path.exists() {
        continue;
      }
      let loaded = if matches!(entry.kind, TemporaryWorkspaceSessionEntryKind::Flow) && is_flow_path(&entry.path) {
        Some(LoadedWorkspaceDocument::Flow {
          document: flowstate_flow::load_flow_document_or_new(&entry.path),
          path: entry.path,
        })
      } else {
        load_workspace_document(entry.path).ok()
      };
      let Some(loaded) = loaded else {
        continue;
      };
      let id = match loaded {
        LoadedWorkspaceDocument::Document { document, path, title } => {
          let panel = self.create_document_panel(*document, path, title, window, cx);
          panel.read(cx).id()
        },
        LoadedWorkspaceDocument::Flow { document, path } => {
          let panel = self.create_flow_panel(document, Some(path), window, cx);
          panel.read(cx).id()
        },
      };
      if Some(entry_index) == active_index {
        active_id = Some(id);
      }
    }

    if let Some(active_id) = active_id {
      self.activate_document_id(active_id, cx);
    }
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  fn refresh_recent_document_previews(&mut self, cx: &mut Context<Self>) {
    self
      .recent_document_previews
      .retain(|path, _| self.recent_documents.iter().any(|recent| recent == path));

    let paths = self
      .recent_documents
      .iter()
      .filter(|path| !self.recent_document_previews.contains_key(*path) && !is_flow_path(path))
      .cloned()
      .collect::<Vec<_>>();
    if paths.is_empty() {
      return;
    }

    self.recent_document_preview_generation = self.recent_document_preview_generation.wrapping_add(1);
    let generation = self.recent_document_preview_generation;
    for path in paths {
      cx.spawn(async move |workspace, cx| {
        let preview = cx
          .background_executor()
          .spawn({
            let path = path.clone();
            async move {
              let mut loaded = load_document_for_open(&path).ok()?;
              loaded.document.theme = load_document_theme();
              Some(recent_document_preview_document(&loaded.document))
            }
          })
          .await;

        let _ = workspace.update(cx, |workspace, cx| {
          if workspace.recent_document_preview_generation != generation
            || !workspace
              .recent_documents
              .iter()
              .any(|recent| recent == &path)
          {
            return;
          }
          if let Some(preview) = preview {
            workspace.recent_document_previews.insert(path, preview);
            cx.notify();
          }
        });
      })
      .detach();
    }
  }

  fn record_recent_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.recent_documents.retain(|recent| recent != &path);
    self.recent_documents.insert(0, path);
    self.recent_documents.truncate(3);
    if let Err(error) = save_recent_documents(self.recent_documents.clone()) {
      eprintln!("failed to save recent documents: {error}");
    }
    self.refresh_recent_document_previews(cx);
    cx.notify();
  }

  pub fn prompt_open_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Open .db8, .docx, .pdf, or .fl0 document".into()),
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
    self.create_document_panel(document, path, None, window, cx);
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  fn add_document_panel_with_title(
    &mut self,
    document: Document,
    path: Option<PathBuf>,
    title: Option<String>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    self.create_document_panel(document, path, title, window, cx);
    self.persist_temporary_workspace_session(cx);
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
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, move |workspace, editor, cx| {
        workspace.maybe_autosave_flow(id, editor.clone(), cx);
      }),
    ));
    self.active_document_id = Some(id);
    self.active_editor = None;
    self.active_flow = Some(editor);
    self.flow_panels.push(panel.clone());
    self.outline_cache = None;
    self.outline_viewport_paragraph = None;
    self.outline_active_paragraph = None;
    self.outline_scrolled_paragraph = None;
    panel
  }

  fn add_flow_panel(&mut self, document: flowstate_flow::FlowDocument, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
    self.create_flow_panel(document, path, window, cx);
    self.persist_temporary_workspace_session(cx);
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
      .map(|panel| {
        let editor = panel.read(cx).editor();
        PanelKind::Document { panel, editor }
      })
      .or_else(|| {
        flow_panel.map(|panel| {
          let editor = panel.read(cx).editor();
          PanelKind::Flow { panel, editor }
        })
      })
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
        Ok(0) => match panel_kind.save(window_handle, cx).await {
          PanelSaveOutcome::Saved => true,
          PanelSaveOutcome::Cancelled => false,
          PanelSaveOutcome::Failed(error) => {
            show_save_failed(window_handle, cx, error);
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
    let tub_search = self.active_tub_index_for_search();
    let overlay = cx.new(|cx| FileSearchOverlay::new(workspace, tub_search, window, cx));
    overlay.update(cx, |overlay, cx| overlay.focus_search(window, cx));
    self.file_search_overlay = Some(overlay);
    cx.notify();
  }

  pub fn close_file_search_overlay(&mut self, cx: &mut Context<Self>) {
    self.file_search_overlay = None;
    cx.notify();
  }

  fn maybe_autosave_document(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    if !self.autosave_enabled {
      return;
    }

    let (has_path, has_unsaved_changes, generation) = {
      let editor = editor.read(cx);
      (editor.document_path().is_some(), editor.has_unsaved_changes(), editor.edit_generation())
    };
    if !has_path || !has_unsaved_changes {
      return;
    }
    if self.autosave_document_generations.get(&panel_id) == Some(&generation) {
      return;
    }
    self
      .autosave_document_generations
      .insert(panel_id, generation);
    let save_task = editor.update(cx, |editor, cx| editor.save(cx));
    cx.spawn(async move |workspace, cx| {
      if let Err(error) = save_task.await {
        eprintln!("autosave failed: {error}");
        let _ = workspace.update(cx, |workspace, _| {
          if workspace.autosave_document_generations.get(&panel_id) == Some(&generation) {
            workspace.autosave_document_generations.remove(&panel_id);
          }
        });
      }
    })
    .detach();
  }

  fn maybe_autosave_flow(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    if !self.autosave_enabled
      || self.autosave_flow_in_flight.contains(&panel_id)
      || editor.read(cx).document_path().is_none()
      || !editor.read(cx).has_unsaved_changes()
    {
      return;
    }

    self.autosave_flow_in_flight.insert(panel_id);
    let save_task = editor.update(cx, |editor, cx| editor.save(cx));
    cx.spawn(async move |workspace, cx| {
      if let Err(error) = save_task.await {
        eprintln!("flow autosave failed: {error}");
      }
      let _ = workspace.update(cx, |workspace, _| {
        workspace.autosave_flow_in_flight.remove(&panel_id);
      });
    })
    .detach();
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
              workspace.persist_temporary_workspace_session(cx);
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
              workspace.persist_temporary_workspace_session(cx);
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
  Document {
    panel: Entity<DocumentPanel>,
    editor: Entity<RichTextEditor>,
  },
  Flow {
    panel: Entity<FlowPanel>,
    editor: Entity<FlowEditor>,
  },
}

enum PanelSaveOutcome {
  Saved,
  Cancelled,
  Failed(String),
}

#[hotpath::measure_all]
impl PanelKind {
  fn is_dirty(&self, cx: &App) -> bool {
    match self {
      PanelKind::Document { editor, .. } => editor.read(cx).has_unsaved_changes(),
      PanelKind::Flow { editor, .. } => editor.read(cx).has_unsaved_changes(),
    }
  }

  async fn save(&self, window_handle: AnyWindowHandle, cx: &mut gpui::AsyncApp) -> PanelSaveOutcome {
    match self {
      PanelKind::Document { panel, editor } => {
        let needs_save_as = match editor.update(cx, |editor, _| editor.document_path().is_none()) {
          Ok(needs_save_as) => needs_save_as,
          Err(error) => return PanelSaveOutcome::Failed(format!("failed to access editor before save: {error}")),
        };
        if needs_save_as {
          let path = match prompt_for_panel_save_path(window_handle, cx, UNTITLED_DOCUMENT_NAME).await {
            Ok(Some(path)) => normalize_db8_path(path),
            Ok(None) => return PanelSaveOutcome::Cancelled,
            Err(error) => return PanelSaveOutcome::Failed(error),
          };
          match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
            Ok(task) => match task.await {
              Ok(()) => {
                let _ = panel.update(cx, |panel, cx| panel.set_path(path, cx));
                PanelSaveOutcome::Saved
              },
              Err(error) => PanelSaveOutcome::Failed(error.to_string()),
            },
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access editor before save: {error}")),
          }
        } else {
          match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(task) => task
              .await
              .map(|_| PanelSaveOutcome::Saved)
              .unwrap_or_else(|error| PanelSaveOutcome::Failed(error.to_string())),
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access editor before save: {error}")),
          }
        }
      },
      PanelKind::Flow { panel, editor } => {
        let needs_save_as = match editor.update(cx, |editor, _| editor.document_path().is_none()) {
          Ok(needs_save_as) => needs_save_as,
          Err(error) => return PanelSaveOutcome::Failed(format!("failed to access flow before save: {error}")),
        };
        if needs_save_as {
          let path = match prompt_for_panel_save_path(window_handle, cx, UNTITLED_FLOW_NAME).await {
            Ok(Some(path)) => normalize_fl0_path(path),
            Ok(None) => return PanelSaveOutcome::Cancelled,
            Err(error) => return PanelSaveOutcome::Failed(error),
          };
          match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
            Ok(task) => match task.await {
              Ok(()) => {
                let _ = panel.update(cx, |panel, cx| panel.set_path(path, cx));
                PanelSaveOutcome::Saved
              },
              Err(error) => PanelSaveOutcome::Failed(error.to_string()),
            },
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access flow before save: {error}")),
          }
        } else {
          match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(task) => task
              .await
              .map(|_| PanelSaveOutcome::Saved)
              .unwrap_or_else(|error| PanelSaveOutcome::Failed(error.to_string())),
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access flow before save: {error}")),
          }
        }
      },
    }
  }

  fn discard(&self, cx: &mut gpui::AsyncApp) {
    match self {
      PanelKind::Document { editor, .. } => {
        let _ = editor.update(cx, |editor, cx| editor.discard_recovery_file(cx));
      },
      PanelKind::Flow { editor, .. } => {
        let _ = editor.update(cx, |editor, _| editor.discard_recovery_file());
      },
    }
  }
}

#[hotpath::measure]
async fn prompt_for_panel_save_path(
  window_handle: AnyWindowHandle,
  cx: &mut gpui::AsyncApp,
  suggested_name: &'static str,
) -> Result<Option<PathBuf>, String> {
  let save_path = window_handle
    .update(cx, |_, _, cx| cx.prompt_for_new_path(&default_save_directory(), Some(suggested_name)))
    .map_err(|error| format!("failed to open save dialog: {error}"))?;
  match save_path.await {
    Ok(Ok(path)) => Ok(path),
    Ok(Err(error)) => Err(error.to_string()),
    Err(error) => Err(format!("save dialog closed unexpectedly: {error}")),
  }
}

#[hotpath::measure]
fn show_save_failed(window_handle: AnyWindowHandle, cx: &mut gpui::AsyncApp, detail: String) {
  let _ = window_handle.update(cx, |_, window, cx| {
    window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
  });
}

enum LoadedWorkspaceDocument {
  Document {
    document: Box<Document>,
    path: Option<PathBuf>,
    title: Option<String>,
  },
  Flow {
    document: flowstate_flow::FlowDocument,
    path: PathBuf,
  },
}

#[hotpath::measure]
fn load_workspace_document(path: PathBuf) -> Result<LoadedWorkspaceDocument, String> {
  if is_flow_path(&path) {
    return Ok(LoadedWorkspaceDocument::Flow {
      document: flowstate_flow::load_flow_document_or_new(&path),
      path,
    });
  }
  load_document_for_open(&path)
    .map(|loaded| LoadedWorkspaceDocument::Document {
      document: Box::new(loaded.document),
      path: loaded.path,
      title: loaded.title,
    })
    .map_err(|error| error.to_string())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct TemporaryWorkspaceSession {
  entries: Vec<TemporaryWorkspaceSessionEntry>,
  active_index: Option<usize>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct TemporaryWorkspaceSessionEntry {
  kind: TemporaryWorkspaceSessionEntryKind,
  path: PathBuf,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum TemporaryWorkspaceSessionEntryKind {
  Document,
  Flow,
}

#[hotpath::measure]
fn temporary_workspace_session_path() -> PathBuf {
  std::env::temp_dir().join("flowstate-open-tabs-session.json")
}

#[hotpath::measure]
fn load_temporary_workspace_session() -> Option<TemporaryWorkspaceSession> {
  fs::read(temporary_workspace_session_path())
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

#[hotpath::measure]
fn persist_temporary_workspace_session_to_disk(session: TemporaryWorkspaceSession) {
  let path = temporary_workspace_session_path();
  if session.entries.is_empty() {
    if let Err(error) = fs::remove_file(&path)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      eprintln!("failed to remove temporary workspace session {}: {error}", path.display());
    }
    return;
  }

  match serde_json::to_vec(&session) {
    Ok(bytes) => {
      if let Err(error) = fs::write(&path, bytes) {
        eprintln!("failed to write temporary workspace session {}: {error}", path.display());
      }
    },
    Err(error) => {
      eprintln!("failed to serialize temporary workspace session: {error}");
    },
  }
}

#[hotpath::measure]
fn recent_document_preview_document(document: &Document) -> Document {
  const MAX_PARAGRAPHS: usize = 12;
  const MAX_CHARS: usize = 2_200;

  let mut remaining_chars = MAX_CHARS;
  let mut paragraphs = Vec::new();

  for paragraph in document.paragraphs.iter().take(MAX_PARAGRAPHS) {
    if remaining_chars == 0 {
      break;
    }

    let mut run_start = paragraph.byte_range.start;
    let mut runs = Vec::new();
    for run in &paragraph.runs {
      if remaining_chars == 0 {
        break;
      }

      let run_end = run_start + run.len;
      let text = document_text_slice(document, run_start..run_end);
      run_start = run_end;

      if text.is_empty() {
        continue;
      }

      let mut used_chars = 0usize;
      let capped_text = text
        .chars()
        .take_while(|_| {
          let keep = used_chars < remaining_chars;
          if keep {
            used_chars += 1;
          }
          keep
        })
        .collect::<String>();
      remaining_chars -= used_chars;
      runs.push(InputRun {
        text: capped_text,
        styles: run.styles,
      });
    }

    if !runs.is_empty() {
      paragraphs.push(InputParagraph {
        style: paragraph.style,
        runs,
      });
    }
  }

  document_from_input(document.theme.clone(), paragraphs)
}

#[hotpath::measure]
fn is_flow_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("fl0"))
}
