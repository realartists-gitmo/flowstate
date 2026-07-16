/// How a flow panel obtains its gated runtime (spec S8): rebuilt from a
/// loaded/new `FlowDocument`, or attached pre-built (the S10 join handoff).
pub(crate) enum FlowRuntimeSource {
  FromDocument(Box<flowstate_flow::FlowDocument>),
  #[allow(dead_code, reason = "constructed by the S10 join handoff")]
  Attachment {
    handle: std::sync::Arc<flowstate_collab::flow::FlowDocHandle>,
    io: flowstate_collab::flow::FlowIoHandle,
  },
}

enum DocumentRuntimeSource {
  FromProjection,
  Runtime(Box<flowstate_collab::crdt_runtime::CrdtRuntime>),
  Attachment(DocumentRuntimeAttachment),
}

/// Loro-first document wiring (spec §3): the write authority handed to the
/// editor (the ONE local write path, identical for solo and collaborative
/// documents) plus the I/O service handle for saves/exports/imports.
pub(crate) struct DocumentRuntimeAttachment {
  pub authority: std::sync::Arc<flowstate_collab::local_write::LocalDocHandle>,
  pub io: flowstate_collab::doc_io::DocIoHandle,
}

/// Wrap a document core in the Loro-first services: the write gate, the
/// editor's write authority, and the background I/O service thread.
pub(crate) fn attach_local_write(core: flowstate_collab::crdt_runtime::CrdtRuntime) -> std::io::Result<DocumentRuntimeAttachment> {
  let (handle, gate) = flowstate_collab::local_write::LocalDocHandle::new(core, flowstate_collab::local_write::LocalWriteConfig::default());
  let io = flowstate_collab::doc_io::DocIoHandle::spawn(gate)?;
  Ok(DocumentRuntimeAttachment {
    authority: std::sync::Arc::new(handle),
    io,
  })
}

/// Install the write authority + native save/export/recovery I/O hooks onto an
/// editor (spec §5). Shared by the normal one-shot open (`create_document_panel`)
/// and the §act-three C phase-G attach (`attach_runtime_to_pending_panel`), so
/// both wire the editor identically. `set_write_authority` replaces the editor's
/// projection with the authority's canonical one, so a read-only phase-V editor
/// becomes editable and authoritative here.
fn install_editor_write_authority(
  editor: &Entity<RichTextEditor>,
  attachment: &DocumentRuntimeAttachment,
  document: DocumentProjection,
  cx: &mut Context<Workspace>,
) {
  let smart_word_selection = load_smart_word_selection();
  let save_io = attachment.io.clone();
  let export_io = attachment.io.clone();
  let recovery_io = attachment.io.clone();
  let authority = std::sync::Arc::clone(&attachment.authority);
  editor.update(cx, |editor, cx| {
    editor.set_smart_word_selection(smart_word_selection, cx);
    // Loro-first (spec §5): the editor's write authority IS the document.
    // Local intents commit synchronously; hooks below are pure I/O — no
    // pending-edit drains, no projection replacement, no undo hook (undo
    // executes through the authority).
    editor.set_write_authority(authority, document, cx);
    editor.set_native_save_hook(Some(Rc::new(move |path, _assets| {
      let io = save_io.clone();
      Box::pin(async move {
        let title = document_package_title_for_path(&path);
        // The checkpoint's LocalUpdate events are intentionally dropped here:
        // this UI-agnostic save hook future has no GPUI context to reach the
        // collaboration session. The workspace re-syncs collaborators after
        // the save completes via collab::refresh_after_external_checkpoint.
        io.checkpoint_package(title.clone(), Some(path.clone()))
          .await
          .map_err(runtime_io_error)?;
        if crate::app_settings::load_dropbox_document_binding(&path).is_some() {
          let package = io
            .package_bytes(title.clone())
            .await
            .map_err(runtime_io_error)?;
          crate::collab::dropbox_checkpoint::sync_bound_checkpoint(&path, title, &io, package).await?;
        }
        Ok(())
      })
    })));
    editor.set_native_export_hook(Some(Rc::new(move |path, format, _assets| {
      let io = export_io.clone();
      Box::pin(async move {
        match format {
          crate::rich_text_element::DocumentExportFormat::Native | crate::rich_text_element::DocumentExportFormat::NativeWithExtension(_) => {
            let bytes = io
              .package_bytes(document_package_title_for_path(&path))
              .await
              .map_err(runtime_io_error)?;
            write_bytes_to_path(&path, &bytes)?;
          },
          crate::rich_text_element::DocumentExportFormat::Docx => {
            let document = io.projection_snapshot().await.map_err(runtime_io_error)?;
            crate::docx_conversion::write_docx(&path, &document)?;
          },
          crate::rich_text_element::DocumentExportFormat::Pdf => {
            let document = io.projection_snapshot().await.map_err(runtime_io_error)?;
            let bytes = io
              .package_bytes("PDF Source".to_string())
              .await
              .map_err(runtime_io_error)?;
            crate::docx_conversion::write_pdf_with_db8_bytes(&path, &document, &bytes)?;
          },
        };
        Ok(())
      })
    })));
    editor.set_native_recovery_hook(Some(Rc::new(move |path| {
      let io = recovery_io.clone();
      Box::pin(async move {
        let bytes = io
          .package_bytes("Recovery snapshot".to_string())
          .await
          .map_err(runtime_io_error)?;
        write_bytes_to_path(&path, &bytes)
      })
    })));
  });
}

#[hotpath::measure_all]
impl Workspace {
  pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    crate::collab::init(cx);
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
      } else if let Some(flow) = workspace.active_flow.clone() {
        flow.update(cx, |flow, cx| flow.set_zoom_percent(*percent, cx));
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
      document_runtimes: FxHashMap::default(), // §perf: FxHash for trusted Uuid keys
      flow_document_runtimes: FxHashMap::default(),
      pending_authority_panels: FxHashSet::default(),
      document_runtime_flush_pending: FxHashSet::default(), // §perf: FxHash for trusted Uuid keys
      flow_panels: Vec::new(),
      active_document_id: None,
      active_editor: None,
      active_flow: None,
      ribbon_collapsed: false,
      outline_collapsed: false,
      active_toolkit_tool: None,
      recent_documents: load_recent_documents(),
      recent_document_previews: HashMap::new(),
      recent_document_preview_generation: 0,
      temporary_workspace_session_pending: None,
      temporary_workspace_session_persist_scheduled: false,
      left_nav_mode: LeftNavMode::Outline,
      restored_nav_width: None,
      keymap_errors: FxHashMap::default(),
      tab_bar_scroll_handle: ScrollHandle::new(),
      pinned_document_ids: Vec::new(),
      speech_document_id: None,
      speech_word_count_cache: FxHashMap::default(),   // §perf: FxHash for trusted Uuid keys
      speech_word_count_pending: FxHashSet::default(), // §perf: FxHash for trusted Uuid keys
      body_resizable_state: cx.new(|_| ResizableState::default()),
      content_resizable_state: cx.new(|_| ResizableState::default()),
      ribbon_resizable_state: cx.new(|_| ResizableState::default()),
      committed_ribbon_height: px(112.0),
      outline_tree: cx.new(|cx| TreeState::new(cx)),
      outline_cache: None,
      collapsed_outline_items: HashSet::new(),
      outline_revision: 0,
      outline_context_menu: None,
      outline_viewport_paragraph: None,
      outline_active_paragraph: None,
      outline_scrolled_paragraph: None,
      editor_subscriptions: Vec::new(),
      settings_overlay: None,
      document_style_picker_revision: 0,
      document_style_section: DocumentStyleSection::Text,
      settings_section: WorkspaceSettingsSection::General,
      autosave_enabled: load_autosave(),
      autosave_document_generations: FxHashMap::default(), // §perf: FxHash for trusted Uuid keys
      autosave_pending_generation: FxHashMap::default(),   // §act-five P9-throttle debounce
      panel_save_states: FxHashMap::default(),
      activity_event: None,
      activity_generation: 0,
      autosave_flow_in_flight: FxHashSet::default(),       // §perf: FxHash for trusted Uuid keys
      collaboration_dialog: None,
      revision_dialog: None,
      collab_notice_subscriptions: FxHashMap::default(), // §perf: FxHash for trusted SessionId keys
      collab_incompatible_version_notices: HashSet::new(),
      file_search_overlay: None,
      command_palette: None,
      comments_panel: None,
      comment_last_seen: std::collections::HashMap::new(),
      unread_comment_count: 0,
      comment_unread_refresh_pending: false,
      comment_unread_refresh_generation: 0,
      tub_root: None,
      tub_index: None,
      tub_files: Vec::new(),
      tub_tree: cx.new(|cx| TreeState::new(cx)),
      tub_tree_items: Vec::new(),
      tub_tree_entries: Vec::new(),
      tub_expanded_dirs: HashSet::new(),
      tub_file_search_input,
      tub_file_search_generation: 0,
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

    // P5-S1 (Law 2/7): a settings file that failed to parse at startup is no
    // longer a silent wipe — the load path backed it up and left a warning.
    if let Some(warning) = crate::app_settings::take_settings_load_warning() {
      this.report_failure(warning, None, cx);
    }

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
    mut document: DocumentProjection,
    path: Option<PathBuf>,
    title: Option<String>,
    runtime: DocumentRuntimeSource,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> anyhow::Result<Entity<DocumentPanel>> {
    // DB8 stores style assignments, not style appearance. The render theme is
    // local user preference loaded from app settings.
    document.theme = load_document_theme();
    let runtime_title = title
      .as_deref()
      .or_else(|| {
        path
          .as_deref()
          .and_then(Path::file_name)
          .and_then(|name| name.to_str())
      })
      .unwrap_or("Flowstate Document");
    // §15/§31: freshly spawned runtimes get the durable author identity bound
    // below. A `Handle` is an already-live collaboration runtime that registered
    // its identity at join time (see `CollabSession::finish_join_snapshot`), so
    // re-binding it here would be a redundant second call on the same runtime.
    let mut register_author_identity = true;
    // Loro-first (spec §3): every document — solo or collaborative — gets the
    // SAME wiring: a gate-protected core, the editor's write authority
    // (LocalDocHandle), and the background I/O service (DocIoHandle).
    let attachment = match runtime {
      DocumentRuntimeSource::FromProjection => {
        let imported = flowstate_document::import_document_projection(document.clone(), runtime_title)
          .map_err(|error| anyhow::anyhow!("creating canonical Loro document failed: {error}"))?;
        let core = flowstate_collab::crdt_runtime::CrdtRuntime::from_imported_document(imported)
          .map_err(|error| anyhow::anyhow!("creating canonical Loro runtime failed: {error:#}"))?;
        attach_local_write(core).map_err(|error| anyhow::anyhow!("starting Loro-first document services failed: {error:#}"))?
      },
      DocumentRuntimeSource::Runtime(runtime) => {
        attach_local_write(*runtime).map_err(|error| anyhow::anyhow!("starting Loro-first document services failed: {error:#}"))?
      },
      DocumentRuntimeSource::Attachment(attachment) => {
        register_author_identity = false;
        attachment
      },
    };
    let local_theme = document.theme.clone();
    document = attachment
      .authority
      .projection()
      .map_err(|error| anyhow::anyhow!("reading canonical startup projection failed: {error}"))?;
    document.theme = local_theme;

    if register_author_identity {
      // Fire-and-forget: a failure to register author identity must never block
      // or break opening the document. The settings read/persist happens on a
      // background thread to keep the foreground non-blocking.
      let identity_io = attachment.io.clone();
      cx.spawn(async move |_, cx| {
        let (user_id, display_name) = cx
          .background_executor()
          .spawn(async { load_local_user_identity() })
          .await;
        if let Err(error) = identity_io.set_author_identity(user_id, display_name).await {
          tracing::warn!(error = %format_args!("{error:#}"), "binding durable author identity to document runtime failed");
        }
      })
      .detach();
    }

    let editor = cx.new(|cx| RichTextEditor::new_with_path(document.clone(), path.clone(), cx));
    install_editor_write_authority(&editor, &attachment, document, cx);
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
    self.document_runtimes.insert(id, attachment.io.clone());
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, move |workspace, editor, cx| {
        // Loro-first: no runtime flush — intents commit synchronously in the
        // editor's write authority. The observation drives view-model updates
        // and autosave only.
        let viewport_paragraph = workspace.active_editor_viewport_paragraph(cx);
        workspace.update_outline_viewport_paragraph(viewport_paragraph, cx);
        workspace.maybe_autosave_document(id, editor.clone(), cx);
        // C-S5: the session's comment nudge lands here too — recount unread.
        workspace.schedule_comment_unread_refresh(cx);
      }),
    ));
    self.active_document_id = Some(id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.document_panels.push(panel.clone());
    Ok(panel)
  }

  pub fn request_document_revisions(
    &self,
    panel_id: Uuid,
    cx: &mut Context<Self>,
  ) -> Option<async_channel::Receiver<anyhow::Result<Vec<flowstate_collab::crdt_runtime::RuntimeRevisionInfo>>>> {
    let runtime = self.document_runtimes.get(&panel_id)?.clone();
    let (tx, rx) = async_channel::bounded(1);
    cx.spawn(async move |_, _| {
      let _ = tx.send(runtime.revisions().await).await;
    })
    .detach();
    Some(rx)
  }

  pub fn open_revision_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    if self.revision_dialog.is_some() {
      window.close_dialog(cx);
      self.revision_dialog = None;
    }
    let workspace = cx.entity().downgrade();
    let revisions = self.request_document_revisions(panel_id, cx);
    let dialog = cx.new(|cx| crate::workspace::revision_dialog::RevisionDialog::new(workspace, panel_id, revisions, cx));
    let dialog_for_render = dialog.clone();
    let workspace_for_close = cx.entity().downgrade();
    window.open_dialog(cx, move |component_dialog, _, _| {
      let workspace_for_close = workspace_for_close.clone();
      component_dialog
        .title("Document Revisions")
        .w(px(520.0))
        .max_w(px(520.0))
        .on_close(move |_, _, cx| {
          let _ = workspace_for_close.update(cx, |workspace, cx| {
            workspace.revision_dialog = None;
            cx.notify();
          });
        })
        .child(dialog_for_render.clone())
    });
    dialog.update(cx, |dialog, cx| dialog.focus(window, cx));
    self.revision_dialog = Some(dialog);
    cx.notify();
  }

  pub fn open_document_revision(&mut self, panel_id: Uuid, revision_id: u128, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(runtime) = self.document_runtimes.get(&panel_id).cloned() else {
      return false;
    };
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let result = runtime.fork_revision(revision_id).await;
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| match result {
          Ok(events) => {
            let fork = events.into_iter().find_map(|event| match event {
              flowstate_collab::crdt_runtime::RuntimeEvent::RevisionForked { document, package, .. } => Some((*document, *package)),
              _ => None,
            });
            let Some((document, package)) = fork else {
              tracing::warn!(revision_id, "revision fork command returned no fork payload");
              return;
            };
            let fork_runtime = match flowstate_collab::crdt_runtime::CrdtRuntime::from_package(package, None) {
              Ok(runtime) => runtime,
              Err(error) => {
                tracing::error!(revision_id, error = %format_args!("{error:#}"), "creating revision fork runtime failed");
                return;
              },
            };
            let panel = match workspace.create_document_panel(
              document,
              None,
              Some(format!("Revision {revision_id:x}")),
              DocumentRuntimeSource::Runtime(Box::new(fork_runtime)),
              window,
              cx,
            ) {
              Ok(panel) => panel,
              Err(error) => {
                tracing::error!(revision_id, error = %format_args!("{error:#}"), "starting revision fork runtime failed");
                return;
              },
            };
            panel
              .read(cx)
              .editor()
              .update(cx, |editor, cx| editor.mark_as_unsaved_branch(cx));
          },
          Err(error) => {
            tracing::error!(revision_id, error = %format_args!("{error:#}"), "opening document revision failed");
          },
        });
      });
    })
    .detach();
    true
  }

  pub fn set_active_document(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    self.save_current_outline_state(cx);
    self.active_document_id = Some(panel_id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.restore_outline_state_for_document(panel_id, cx);
    self.outline_cache = None;
    self.refresh_outline_tree(cx);
    // C-S5: the badge counts the ACTIVE document's threads — recount on switch.
    self.schedule_comment_unread_refresh(cx);
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  pub fn set_active_flow(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    self.save_current_outline_state(cx);
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
    crate::collab::leave_session_for_panel(panel_id, cx);
    // §P3 (act two): flush the projection/search caches into the package on
    // close (revision-less, off-thread) so the NEXT preview/open of this file
    // hits the cache fast path — every edit nulls the cache, so a closed
    // edited document otherwise previews via a multi-second full Loro
    // materialization until an explicit save.
    if let Some(io) = self.document_runtimes.get(&panel_id) {
      let io = io.clone();
      cx.background_executor()
        .spawn(async move {
          if let Err(error) = io.flush_package_caches().await {
            tracing::debug!(%error, "close-time package cache flush failed");
          }
        })
        .detach();
    }
    self.document_runtimes.remove(&panel_id);
    self.flow_document_runtimes.remove(&panel_id);
    self.document_runtime_flush_pending.remove(&panel_id);
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
    self.speech_word_count_pending.remove(&panel_id);
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
      // §act-three C phase V: a cheap cached projection paints a read-only panel
      // immediately, while phase G (the full Loro import + runtime) loads off
      // thread. Editing enables when the authority attaches (phase G below).
      let phase_v_path = path.clone();
      let cached = cx
        .background_executor()
        .spawn(async move { read_open_projection_fast(&phase_v_path) })
        .await;
      let pending_panel_id = if let Some(projection) = cached {
        let scroll_target = target_paragraph_ix;
        window_handle
          .update(cx, |_, window, cx| {
            workspace
              .update(cx, |workspace, cx| {
                let id = workspace.create_pending_document_panel(projection, Some(path_for_recent.clone()), None, window, cx);
                if let Some(paragraph_ix) = scroll_target {
                  workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx);
                }
                id
              })
              .ok()
          })
          .ok()
          .flatten()
      } else {
        None
      };

      let loaded = cx
        .background_executor()
        .spawn(async move { load_workspace_document(path) })
        .await;
      match loaded {
        Ok(LoadedWorkspaceDocument::Document {
          document,
          runtime,
          path,
          title,
        }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace
                .recent_document_previews
                .insert(path_for_recent.clone(), recent_document_preview_document(&document));
              workspace.record_recent_document(path_for_recent.clone(), cx);
              // Phase G: attach the runtime to the phase-V panel if it is still
              // open; otherwise (no cache existed, or the pending panel was
              // closed while loading) open a fresh full panel. `attach` gives
              // the runtime back when it cannot use it.
              let fresh_runtime = match pending_panel_id {
                Some(id) => workspace
                  .attach_runtime_to_pending_panel(id, *runtime, path.clone(), cx)
                  .err()
                  .map(|boxed| *boxed),
                None => Some(*runtime),
              };
              if let Some(runtime) = fresh_runtime {
                workspace.add_document_panel_with_title(*document, path, title, runtime, window, cx);
              }
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
    let mut panel_id_to_entry_index: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();

    for panel in &self.document_panels {
      let panel = panel.read(cx);
      let Some(path) = panel.editor().read(cx).document_path().cloned() else {
        continue;
      };
      let entry_index = entries.len();
      panel_id_to_entry_index.insert(panel.id(), entry_index);
      if Some(panel.id()) == self.active_document_id {
        active_index = Some(entry_index);
      }
      let (collapsed_outline_items, outline_scrolled_paragraph) = if Some(panel.id()) == self.active_document_id {
        (self.collapsed_outline_items.iter().copied().collect(), self.outline_scrolled_paragraph)
      } else {
        let items = panel.collapsed_outline_items.clone().unwrap_or_default();
        (items.into_iter().collect(), panel.outline_scrolled_paragraph)
      };
      let viewport_paragraph = panel.editor().read(cx).viewport_anchor_paragraph();
      entries.push(TemporaryWorkspaceSessionEntry {
        kind: TemporaryWorkspaceSessionEntryKind::Document,
        path,
        collapsed_outline_items,
        outline_scrolled_paragraph,
        viewport_paragraph,
      });
    }

    for panel in &self.flow_panels {
      let panel = panel.read(cx);
      let Some(path) = panel.editor().read(cx).document_path().cloned() else {
        continue;
      };
      let entry_index = entries.len();
      panel_id_to_entry_index.insert(panel.id(), entry_index);
      if Some(panel.id()) == self.active_document_id {
        active_index = Some(entry_index);
      }
      entries.push(TemporaryWorkspaceSessionEntry {
        kind: TemporaryWorkspaceSessionEntryKind::Flow,
        path,
        collapsed_outline_items: Vec::new(),
        outline_scrolled_paragraph: None,
        viewport_paragraph: None,
      });
    }

    let pinned_entry_indices = self
      .pinned_document_ids
      .iter()
      .filter_map(|id| panel_id_to_entry_index.get(id).copied())
      .collect();
    let speech_entry_index = self
      .speech_document_id
      .and_then(|id| panel_id_to_entry_index.get(&id).copied());

    self.temporary_workspace_session_pending = Some(TemporaryWorkspaceSession {
      entries,
      active_index,
      ribbon_collapsed: self.ribbon_collapsed,
      outline_collapsed: self.outline_collapsed,
      pinned_entry_indices,
      speech_entry_index,
      left_nav_tub: self.left_nav_mode == LeftNavMode::Tub,
      nav_width: self
        .body_resizable_state
        .read(cx)
        .sizes()
        .first()
        .map(|width| f32::from(*width)),
      tub_tool_open: self.active_toolkit_tool == Some(ToolkitTool::Tub),
      toolkit_filter: Some(self.toolkit_search_filter.session_key().to_string()),
      tub_expanded_dirs: self
        .tub_expanded_dirs
        .iter()
        .map(|dir| dir.to_string_lossy().into_owned())
        .collect(),
      comment_last_seen: self
        .comment_last_seen
        .iter()
        .map(|(id, seen)| (format!("{id:032x}"), *seen))
        .collect(),
    });
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
    let mut viewport_scrolls: Vec<(Uuid, Option<usize>)> = Vec::new();
    let pinned_indices: std::collections::HashSet<usize> = session.pinned_entry_indices.iter().copied().collect();
    let speech_index = session.speech_entry_index;
    let mut pinned_ids = Vec::new();
    let mut speech_id = None;
    for (entry_index, entry) in session.entries.into_iter().enumerate() {
      if !entry.path.exists() {
        continue;
      }
      let loaded = if matches!(entry.kind, TemporaryWorkspaceSessionEntryKind::Flow) && is_flow_path(&entry.path) {
        flowstate_flow::load_flow_document(&entry.path)
          .ok()
          .map(|document| LoadedWorkspaceDocument::Flow {
            document,
            path: entry.path,
          })
      } else {
        load_workspace_document(entry.path).ok()
      };
      let Some(loaded) = loaded else {
        continue;
      };
      let id = match loaded {
        LoadedWorkspaceDocument::Document {
          document,
          runtime,
          path,
          title,
        } => {
          let panel = match self.create_document_panel(*document, path, title, DocumentRuntimeSource::Runtime(runtime), window, cx) {
            Ok(panel) => panel,
            Err(error) => {
              tracing::error!(error = %format_args!("{error:#}"), "restoring document runtime failed");
              continue;
            },
          };
          let panel_id = panel.read(cx).id();
          panel.update(cx, |panel, _| {
            if !entry.collapsed_outline_items.is_empty() {
              panel.collapsed_outline_items = Some(entry.collapsed_outline_items.iter().copied().collect());
            }
            panel.outline_scrolled_paragraph = entry.outline_scrolled_paragraph;
          });
          viewport_scrolls.push((panel_id, entry.viewport_paragraph));
          panel_id
        },
        LoadedWorkspaceDocument::Flow { document, path } => {
          let panel = self.create_flow_panel(FlowRuntimeSource::FromDocument(Box::new(document)), Some(path), window, cx);
          panel.read(cx).id()
        },
      };
      if Some(entry_index) == active_index {
        active_id = Some(id);
      }
      if pinned_indices.contains(&entry_index) {
        pinned_ids.push(id);
      }
      if speech_index == Some(entry_index) {
        speech_id = Some(id);
      }
    }

    if let Some(active_id) = active_id {
      self.activate_document_id(active_id, cx);
    }
    self.ribbon_collapsed = session.ribbon_collapsed;
    self.outline_collapsed = session.outline_collapsed;
    self.left_nav_mode = if session.left_nav_tub { LeftNavMode::Tub } else { LeftNavMode::Outline };
    self.restored_nav_width = session.nav_width.map(px);
    if session.tub_tool_open {
      self.active_toolkit_tool = Some(ToolkitTool::Tub);
    }
    if let Some(filter) = &session.toolkit_filter {
      self.toolkit_search_filter = ToolkitSearchFilter::from_session_key(filter);
    }
    self
      .tub_expanded_dirs
      .extend(session.tub_expanded_dirs.iter().map(PathBuf::from));
    self.comment_last_seen.extend(
      session
        .comment_last_seen
        .iter()
        .filter_map(|(id, seen)| u128::from_str_radix(id, 16).ok().map(|id| (id, *seen))),
    );
    self.pinned_document_ids = pinned_ids;
    self.speech_document_id = speech_id;

    // Restore editor scroll positions after activation so editors are ready
    for (panel_id, viewport_paragraph) in viewport_scrolls {
      if let Some(paragraph_ix) = viewport_paragraph
        && let Some(panel) = self
          .document_panels
          .iter()
          .find(|p| p.read(cx).id() == panel_id)
      {
        panel.update(cx, |panel, cx| {
          panel.editor().update(cx, |editor, cx| {
            editor.scroll_to_paragraph(paragraph_ix, window, cx);
          });
        });
      }
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
              let mut document = load_document_preview(&path).ok()?;
              document.theme = load_document_theme();
              Some(recent_document_preview_document(&document))
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

  fn add_document_panel(&mut self, document: DocumentProjection, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
    match self.create_document_panel(document, path, None, DocumentRuntimeSource::FromProjection, window, cx) {
      Ok(_) => {
        self.persist_temporary_workspace_session(cx);
        cx.notify();
      },
      Err(error) => {
        let detail = format!("The canonical Loro runtime could not be started: {error:#}");
        tracing::error!(error = %format_args!("{error:#}"), "creating document panel failed");
        std::mem::drop(window.prompt(
          PromptLevel::Critical,
          "Document could not be opened",
          Some(&detail),
          &[PromptButton::ok("Ok")],
          cx,
        ));
      },
    }
  }

  fn add_document_panel_with_title(
    &mut self,
    document: DocumentProjection,
    path: Option<PathBuf>,
    title: Option<String>,
    runtime: flowstate_collab::crdt_runtime::CrdtRuntime,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    match self.create_document_panel(document, path, title, DocumentRuntimeSource::Runtime(Box::new(runtime)), window, cx) {
      Ok(_) => {
        self.persist_temporary_workspace_session(cx);
        cx.notify();
      },
      Err(error) => {
        let detail = format!("The canonical Loro runtime could not be started: {error:#}");
        tracing::error!(error = %format_args!("{error:#}"), "creating document panel failed");
        std::mem::drop(window.prompt(
          PromptLevel::Critical,
          "Document could not be opened",
          Some(&detail),
          &[PromptButton::ok("Ok")],
          cx,
        ));
      },
    }
  }

  fn create_flow_panel(
    &mut self,
    source: FlowRuntimeSource,
    path: Option<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<FlowPanel> {
    self.create_flow_panel_titled(source, path, None, window, cx)
  }

  pub(crate) fn create_flow_panel_titled(
    &mut self,
    source: FlowRuntimeSource,
    path: Option<PathBuf>,
    title_override: Option<String>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<FlowPanel> {
    let (handle, io) = match source {
      FlowRuntimeSource::FromDocument(document) => {
        let runtime = flowstate_collab::flow::FlowRuntime::from_flow_document(&document).unwrap_or_else(|error| {
          // A FlowDocument freshly built or loaded from a validated .fl0 always
          // round-trips; keep the panel usable rather than aborting the open.
          tracing::error!(%error, "flow runtime rebuild from document failed; opening an empty flow");
          flowstate_collab::flow::FlowRuntime::new_empty()
        });
        let (handle, gate) = flowstate_collab::flow::FlowDocHandle::new(runtime);
        let io = flowstate_collab::flow::FlowIoHandle::spawn(gate).expect("flow I/O service spawns");
        (std::sync::Arc::new(handle), io)
      },
      FlowRuntimeSource::Attachment { handle, io } => (handle, io),
    };
    let editor = cx.new(|cx| FlowEditor::new_with_runtime(handle, io.clone(), path.clone(), window, cx));
    let workspace = cx.entity().downgrade();
    let title = title_override.or_else(|| {
      path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .or_else(|| Some(self.next_untitled_flow_title(cx)))
    });
    let panel = cx.new(|cx| FlowPanel::new_with_title(title, path, editor.clone(), workspace, window, cx));
    let id = panel.read(cx).id();
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, move |workspace, editor, cx| {
        workspace.maybe_autosave_flow(id, editor.clone(), cx);
      }),
    ));
    self.flow_document_runtimes.insert(id, io);
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
    self.create_flow_panel(FlowRuntimeSource::FromDocument(Box::new(document)), path, window, cx);
    self.persist_temporary_workspace_session(cx);
    cx.notify();
  }

  /// §act-three C (phase V): open a panel painting a cheap cached projection
  /// with NO write authority — a read-only display surface (scroll/select/copy
  /// work; editing is inert). The authority runtime attaches later via
  /// [`Self::attach_runtime_to_pending_panel`] (phase G). Returns the panel id
  /// so the caller can complete the attach when the runtime finishes loading.
  ///
  /// Session-persist and autosave skip pending panels (a read-only phase-V
  /// panel has no runtime to save); both fire on attach. Because the panel is
  /// read-only until attach, NO local edit can be made — so none can be lost in
  /// the window (the fidelity-safe form of the spec's phase-V open).
  fn create_pending_document_panel(
    &mut self,
    mut document: DocumentProjection,
    path: Option<PathBuf>,
    title: Option<String>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Uuid {
    document.theme = load_document_theme();
    let editor = cx.new(|cx| RichTextEditor::new_with_path(document, path.clone(), cx));
    let smart_word_selection = load_smart_word_selection();
    editor.update(cx, |editor, cx| {
      editor.set_smart_word_selection(smart_word_selection, cx);
      // No `set_write_authority` — the editor stays a read-only display surface
      // (editor/local_write_path.rs: "No authority ⇒ read-only display surface").
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
    self.pending_authority_panels.insert(id);
    self.active_document_id = Some(id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.document_panels.push(panel);
    cx.notify();
    id
  }

  /// C-S6 history-jump: open a read-only tab showing the document as the
  /// comment's author saw it — a checkout at the thread's birth frontier —
  /// and flash the original anchor. The tab reuses the phase-V pending-panel
  /// shape (no write authority ⇒ read-only display surface); no runtime ever
  /// attaches, so it stays a view.
  pub(crate) fn open_comment_history_view(
    &mut self,
    panel_id: Uuid,
    comment_id: u128,
    created_frontier: Vec<u8>,
    quote: String,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    let Some(io) = self.document_runtimes.get(&panel_id).cloned() else {
      self.report_failure("Opening the comment's original context failed: the document is still opening", None, cx);
      return;
    };
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let result = io.frontier_comment_context(created_frontier, comment_id).await;
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| match result {
          Ok((document, anchor)) => {
            let mut short_quote: String = quote.chars().take(24).collect();
            if short_quote.len() < quote.len() {
              short_quote.push('…');
            }
            let title = if short_quote.is_empty() {
              "Original context".to_string()
            } else {
              format!("Original context — “{short_quote}”")
            };
            workspace.create_pending_document_panel(*document, None, Some(title), window, cx);
            if let Some((start, end)) = anchor
              && let Some(editor) = workspace.active_editor.clone()
            {
              editor.update(cx, |editor, cx| {
                editor.peek_paragraph(start.paragraph, crate::rich_text_element::DEFAULT_JUMP_FLASH_RGB, window, cx);
                editor.flash_range(
                  crate::rich_text_element::EditorSelection::range(start, end),
                  crate::rich_text_element::DEFAULT_JUMP_FLASH_RGB,
                  cx,
                );
              });
            }
          },
          Err(error) => {
            workspace.report_failure(format!("Opening the comment's original context failed: {error:#}"), None, cx);
          },
        });
      });
    })
    .detach();
  }

  /// §act-three C (phase G): attach the freshly-loaded runtime to a panel
  /// opened read-only by [`Self::create_pending_document_panel`]. Installs the
  /// write authority + I/O hooks (making the editor editable and swapping in
  /// the runtime's canonical projection), registers the runtime, wires the
  /// autosave observation, and binds the durable author identity — exactly the
  /// wiring the one-shot open does. Returns `Ok(())` when attached (the runtime
  /// is consumed). Returns `Err(runtime)` — handing the runtime back — when the
  /// pending panel was closed before the runtime arrived, so the caller opens a
  /// fresh full panel instead.
  fn attach_runtime_to_pending_panel(
    &mut self,
    panel_id: Uuid,
    runtime: flowstate_collab::crdt_runtime::CrdtRuntime,
    canonical_path: Option<PathBuf>,
    cx: &mut Context<Self>,
  ) -> Result<(), Box<flowstate_collab::crdt_runtime::CrdtRuntime>> {
    // The panel closed while loading — hand the runtime back for a fresh open.
    let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .filter(|_| self.pending_authority_panels.contains(&panel_id))
      .cloned()
    else {
      self.pending_authority_panels.remove(&panel_id);
      return Err(Box::new(runtime));
    };
    // From here the panel exists; any failure leaves it read-only (degraded but
    // safe) rather than opening a duplicate — attach failures are rare hard
    // faults (I/O thread spawn / projection read).
    self.pending_authority_panels.remove(&panel_id);
    let attachment = match attach_local_write(runtime) {
      Ok(attachment) => attachment,
      Err(error) => {
        tracing::error!(error = %format_args!("{error:#}"), "attaching Loro-first services to pending panel failed; leaving panel read-only");
        return Ok(());
      },
    };
    // Adopt the authority's canonical startup projection (the runtime advanced
    // the frontier during construction; the editor must start from it).
    let mut document = match attachment.authority.projection() {
      Ok(document) => document,
      Err(error) => {
        tracing::error!(error = %format_args!("{error:#}"), "reading canonical startup projection for pending panel failed; leaving panel read-only");
        return Ok(());
      },
    };
    document.theme = load_document_theme();
    let editor = panel.read(cx).editor();
    install_editor_write_authority(&editor, &attachment, document, cx);
    // §data-loss fix (2026-07-08): the pending panel seeded the editor with the
    // SOURCE path for phase-V display/recents. Now that the authority is
    // attached and the editor becomes writable, reconcile its write path to the
    // authoritative open path — `None` for imported docx/pdf, so autosave never
    // targets (and overwrites) the source file. Mirrors the one-shot open path.
    editor.update(cx, |editor, cx| editor.set_runtime_document_path(canonical_path, cx));
    self
      .document_runtimes
      .insert(panel_id, attachment.io.clone());
    self.editor_subscriptions.push((
      panel_id,
      cx.observe(&editor, move |workspace, editor, cx| {
        let viewport_paragraph = workspace.active_editor_viewport_paragraph(cx);
        workspace.update_outline_viewport_paragraph(viewport_paragraph, cx);
        workspace.maybe_autosave_document(panel_id, editor.clone(), cx);
        // C-S5: the session's comment nudge lands here too — recount unread.
        workspace.schedule_comment_unread_refresh(cx);
      }),
    ));
    // Bind the durable author identity (fire-and-forget; never blocks open).
    let identity_io = attachment.io.clone();
    cx.spawn(async move |_, cx| {
      let (user_id, display_name) = cx
        .background_executor()
        .spawn(async { load_local_user_identity() })
        .await;
      if let Err(error) = identity_io.set_author_identity(user_id, display_name).await {
        tracing::warn!(error = %format_args!("{error:#}"), "binding durable author identity to attached document runtime failed");
      }
    })
    .detach();
    self.persist_temporary_workspace_session(cx);
    cx.notify();
    Ok(())
  }

  pub fn close_document_panel(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
    self.clear_save_state(panel_id);
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
    if self.close_collaboration_document_panel(panel_id, panel_kind.clone(), window, cx) {
      return;
    }
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
    if self.request_close_window_with_collaboration(window, cx) {
      return;
    }
    let dirty_panels = self.dirty_panels(cx);
    if dirty_panels.is_empty() {
      self.leave_all_collaboration_sessions(cx);
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

    cx.spawn(async move |workspace, cx| {
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
        let _ = window_handle.update(cx, |_, window, cx| {
          let _ = workspace.update(cx, |workspace, cx| workspace.leave_all_collaboration_sessions(cx));
          window.remove_window();
        });
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
      let panel_id = self.active_document_id;
      let save_task = editor.update(cx, |editor, cx| editor.save(cx));
      let window_handle = window.window_handle();
      cx.spawn(async move |workspace, cx| {
        match save_task.await {
          // A successful save checkpoints the runtime, recording a named
          // revision into Loro. For a collaborating document that op must be
          // advertised so peers converge promptly; for a solo document the
          // refresh is a no-op because no session is registered for the panel.
          Ok(()) => {
            if let Some(panel_id) = panel_id {
              let _ = workspace.update(cx, |_, cx| {
                crate::collab::refresh_after_external_checkpoint(panel_id, cx);
              });
            }
          },
          Err(error) => {
            let detail = error.to_string();
            let _ = window_handle.update(cx, |_, window, cx| {
              window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
            });
          },
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
    // Already saved (or already scheduled) for this exact generation? Skip.
    if self.autosave_document_generations.get(&panel_id) == Some(&generation)
      || self.autosave_pending_generation.get(&panel_id) == Some(&generation)
    {
      return;
    }
    // §act-five P9-throttle: DEBOUNCE. Every keystroke bumps the generation and
    // used to fire a full checkpoint (document_from_loro + search reindex +
    // snapshot) — pegging the off-gate package thread during typing/collab.
    // Record the latest pending generation and schedule a TRAILING save; a newer
    // edit overwrites `autosave_pending_generation`, so only the last edit of a
    // burst survives the debounce and ONE checkpoint runs per quiet period. No
    // edit is lost: the trailing timer always fires for the final generation, and
    // close-time `flush_package_caches` covers the tail.
    const AUTOSAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(900);
    self
      .autosave_pending_generation
      .insert(panel_id, generation);
    cx.spawn(async move |workspace, cx| {
      cx.background_executor().timer(AUTOSAVE_DEBOUNCE).await;
      // Superseded by a newer edit within the debounce window? Let that one save.
      let save_task = workspace.update(cx, |workspace, cx| {
        if workspace.autosave_pending_generation.get(&panel_id) != Some(&generation) {
          return None;
        }
        workspace.autosave_pending_generation.remove(&panel_id);
        workspace
          .autosave_document_generations
          .insert(panel_id, generation);
        workspace.set_save_state(panel_id, PanelSaveState::Saving, cx);
        Some(editor.update(cx, |editor, cx| editor.save(cx)))
      });
      let Ok(Some(save_task)) = save_task else {
        return;
      };
      match save_task.await {
        // Keep collaborating peers in sync with the autosave checkpoint; a
        // no-op for solo documents (no session registered for the panel).
        Ok(()) => {
          let _ = workspace.update(cx, |workspace, cx| {
            workspace.set_save_state(panel_id, PanelSaveState::Saved, cx);
            crate::collab::refresh_after_external_checkpoint(panel_id, cx);
          });
        },
        Err(error) => {
          // Law 2: an autosave failure must reach the user, not stderr.
          tracing::error!("autosave failed: {error}");
          let _ = workspace.update(cx, |workspace, cx| {
            if workspace.autosave_document_generations.get(&panel_id) == Some(&generation) {
              workspace.autosave_document_generations.remove(&panel_id);
            }
            workspace.set_save_state(
              panel_id,
              PanelSaveState::Failed {
                message: error.to_string(),
              },
              cx,
            );
            workspace.report_failure(
              format!("Autosave failed: {error}"),
              Some(ActivityAction::RetrySave { panel_id }),
              cx,
            );
          });
        },
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
    self.set_save_state(panel_id, PanelSaveState::Saving, cx);
    let save_task = editor.update(cx, |editor, cx| editor.save(cx));
    cx.spawn(async move |workspace, cx| {
      let result = save_task.await;
      let _ = workspace.update(cx, |workspace, cx| {
        workspace.autosave_flow_in_flight.remove(&panel_id);
        match result {
          Ok(()) => workspace.set_save_state(panel_id, PanelSaveState::Saved, cx),
          Err(error) => {
            // Law 2: an autosave failure must reach the user, not stderr.
            tracing::error!("flow autosave failed: {error}");
            workspace.set_save_state(
              panel_id,
              PanelSaveState::Failed {
                message: error.to_string(),
              },
              cx,
            );
            workspace.report_failure(
              format!("Autosave failed: {error}"),
              Some(ActivityAction::RetrySave { panel_id }),
              cx,
            );
          },
        }
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
              // Advertise the save checkpoint to collaborators (no-op when the
              // panel has no active collaboration session).
              crate::collab::refresh_after_external_checkpoint(panel_id, cx);
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

fn document_package_title_for_path(path: &Path) -> String {
  path
    .file_name()
    .map(|name| name.to_string_lossy().to_string())
    .unwrap_or_else(|| "Flowstate Document".to_string())
}

fn write_bytes_to_path(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    fs::create_dir_all(parent)?;
  }
  atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
    .write(|file| std::io::Write::write_all(file, bytes))
    .map_err(Into::into)
}

/// Stable short tag for a runtime event variant, used only by fidelity firehose
/// lines so a document-runtime commit stream reads which transition was applied.
enum LoadedWorkspaceDocument {
  Document {
    document: Box<DocumentProjection>,
    runtime: Box<flowstate_collab::crdt_runtime::CrdtRuntime>,
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
    let document = flowstate_flow::load_flow_document(&path).map_err(|error| error.to_string())?;
    return Ok(LoadedWorkspaceDocument::Flow { document, path });
  }
  load_document_for_open(&path)
    .map(|loaded| LoadedWorkspaceDocument::Document {
      document: Box::new(loaded.document),
      runtime: Box::new(loaded.runtime),
      path: loaded.path,
      title: loaded.title,
    })
    .map_err(|error| error.to_string())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct TemporaryWorkspaceSession {
  entries: Vec<TemporaryWorkspaceSessionEntry>,
  active_index: Option<usize>,
  #[serde(default)]
  ribbon_collapsed: bool,
  #[serde(default)]
  outline_collapsed: bool,
  #[serde(default)]
  pinned_entry_indices: Vec<usize>,
  #[serde(default)]
  speech_entry_index: Option<usize>,
  /// O-S1: the left nav remembers its mode across launches (the audit's
  /// always-resets-to-Outline amnesia).
  #[serde(default)]
  left_nav_tub: bool,
  /// O-S1: the nav's resized width survives too.
  #[serde(default)]
  nav_width: Option<f32>,
  /// T-S1: toolkit-rail state survives restarts (tool open, filter,
  /// expanded tub directories).
  #[serde(default)]
  tub_tool_open: bool,
  #[serde(default)]
  toolkit_filter: Option<String>,
  #[serde(default)]
  tub_expanded_dirs: Vec<String>,
  /// C-S5: comment read-state (thread id as hex → last-seen activity stamp),
  /// so unread dots survive restarts.
  #[serde(default)]
  comment_last_seen: Vec<(String, i64)>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct TemporaryWorkspaceSessionEntry {
  kind: TemporaryWorkspaceSessionEntryKind,
  path: PathBuf,
  #[serde(default)]
  collapsed_outline_items: Vec<usize>,
  #[serde(default)]
  outline_scrolled_paragraph: Option<usize>,
  #[serde(default)]
  viewport_paragraph: Option<usize>,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum TemporaryWorkspaceSessionEntryKind {
  Document,
  Flow,
}

#[hotpath::measure]
fn temporary_workspace_session_path() -> PathBuf {
  // FLOWSTATE_CONFIG_DIR is the headless-test sandbox root; the tab-session
  // file must follow it or tests restore/clobber the real user's open tabs.
  std::env::var_os("FLOWSTATE_CONFIG_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(std::env::temp_dir)
    .join("flowstate-open-tabs-session.json")
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
      if let Err(error) = write_bytes_to_path(&path, &bytes) {
        eprintln!("failed to write temporary workspace session {}: {error}", path.display());
      }
    },
    Err(error) => {
      eprintln!("failed to serialize temporary workspace session: {error}");
    },
  }
}

#[hotpath::measure]
fn recent_document_preview_document(document: &DocumentProjection) -> DocumentProjection {
  const MAX_PARAGRAPHS: usize = 12;
  const MAX_CHARS: usize = 2_200;

  let mut remaining_chars = MAX_CHARS;
  let mut paragraphs = Vec::new();

  for (paragraph_ix, paragraph) in document.paragraphs.iter().take(MAX_PARAGRAPHS).enumerate() {
    if remaining_chars == 0 {
      break;
    }

    let mut run_start = flowstate_document::paragraph_byte_range(document, paragraph_ix).start;
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
