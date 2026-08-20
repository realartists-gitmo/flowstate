impl Workspace {
  pub fn new(_: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let zoom_slider = cx.new(|_| {
      SliderState::new()
        .min(25.0)
        .max(400.0)
        .step(5.0)
        .default_value(100.0)
    });
    let zoom_slider_subscription = cx.subscribe(&zoom_slider, |workspace, _, event: &SliderEvent, cx| {
      if let SliderEvent::Change(SliderValue::Single(percent)) = event
        && let Some(editor) = workspace.active_editor.clone()
      {
        editor.update(cx, |editor, cx| editor.set_zoom_percent(*percent, cx));
      }
    });
    let toolkit_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tub blocks, tags, and analytics"));
    let tub_file_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tub"));
    let tub_subscription = cx.subscribe(&tub_file_search_input, |_, _, _: &InputEvent, _| {});
    let toolkit_subscription = cx.subscribe(&toolkit_search_input, |_, _, _: &InputEvent, _| {});
    let keybinding_interceptor = cx.intercept_keystrokes(|_, _, _| {});

    let mut document = gpui_flowtext::demo_document();
    document.theme = flowstate_document_theme();
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

    Self {
      document_panels: vec![panel],
      active_document_id: Some(id),
      active_editor: Some(editor),
      ribbon_collapsed: false,
      outline_collapsed: false,
      toolkit_collapsed: true,
      active_toolkit_tool: None,
      recent_documents: Vec::new(),
      recent_document_previews: HashMap::new(),
      recent_document_preview_generation: 0,
      left_nav_mode: LeftNavMode::Outline,
      tab_bar_scroll_handle: ScrollHandle::new(),
      pinned_document_ids: Vec::new(),
      speech_document_id: None,
      speech_word_count_cache: FxHashMap::default(),
      speech_word_count_pending: FxHashSet::default(),
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
      autosave_enabled: false,
      autosave_document_generations: FxHashMap::default(),
      autosave_pending_generation: FxHashMap::default(),
      tub_tree: cx.new(|cx| TreeState::new(cx)),
      tub_tree_items: Vec::new(),
      tub_file_search_input,
      tub_file_search_generation: 0,
      tub_status: "Browser document".into(),
      tub_watch_polling: false,
      tub_scan_in_flight: false,
      tub_scan_pending: false,
      active_tub_path: None,
      toolkit_search_input,
      toolkit_search_filter: ToolkitSearchFilter::All,
      expanded_toolkit_hits: HashSet::new(),
      toolkit_results_scroll_handle: VirtualListScrollHandle::new(),
      toolkit_status: "Browser document".into(),
      toolkit_search_generation: 0,
      _tub_file_search_subscription: tub_subscription,
      _toolkit_search_subscription: toolkit_subscription,
      zoom_slider,
      _zoom_slider_subscription: zoom_slider_subscription,
      _keybinding_interceptor: keybinding_interceptor,
    }
  }
}

pub fn open_workspace_window(document_path: Option<PathBuf>, cx: &mut App) -> WeakEntity<Workspace> {
  let opened_workspace = Rc::new(RefCell::new(None));
  let opened_workspace_slot = opened_workspace.clone();
  cx.open_window(
    WindowOptions {
      app_id: Some("dev.flowstate.Flowstate".to_string()),
      titlebar: Some(TitleBar::title_bar_options()),
      ..Default::default()
    },
    |window, cx| {
      window.set_window_title("Flowstate");
      let workspace = cx.new(|cx| Workspace::new(document_path, window, cx));
      *opened_workspace_slot.borrow_mut() = Some(workspace.downgrade());
      cx.new(|cx| Root::new(workspace, window, cx))
    },
  )
  .expect("failed to open Flowstate browser workspace");
  let result = opened_workspace
    .borrow_mut()
    .take()
    .expect("workspace window builder must install its workspace");
  result
}
