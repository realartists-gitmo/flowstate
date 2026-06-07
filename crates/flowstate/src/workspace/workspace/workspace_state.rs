#[hotpath::measure_all]
impl Workspace {
  pub fn toggle_ribbon(&mut self, cx: &mut Context<Self>) {
    self.ribbon_collapsed = !self.ribbon_collapsed;
    cx.notify();
  }

  pub fn toggle_outline(&mut self, cx: &mut Context<Self>) {
    let width = self
      .body_resizable_state
      .read(cx)
      .sizes()
      .first()
      .copied()
      .unwrap_or(px(240.0));
    let delta = if self.outline_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH - width
    } else {
      width - SIDE_PANEL_COLLAPSED_WIDTH
    };
    self.prepare_active_editor_for_width_delta(delta, cx);
    self.outline_collapsed = !self.outline_collapsed;
    cx.notify();
  }

  pub fn toggle_toolkit(&mut self, cx: &mut Context<Self>) {
    let width = self
      .content_resizable_state
      .read(cx)
      .sizes()
      .get(1)
      .copied()
      .unwrap_or(px(300.0));
    let delta = if self.toolkit_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH - width
    } else {
      width - SIDE_PANEL_COLLAPSED_WIDTH
    };
    self.prepare_active_editor_for_width_delta(delta, cx);
    self.toolkit_collapsed = !self.toolkit_collapsed;
    if self.toolkit_collapsed {
      self.active_toolkit_tool = None;
    }
    cx.notify();
  }

  fn toggle_toolkit_tool(&mut self, tool: ToolkitTool, cx: &mut Context<Self>) {
    if self.toolkit_collapsed {
      self.toolkit_collapsed = false;
    }

    let was_expanded = self.active_toolkit_tool.is_some();
    self.active_toolkit_tool = if self.active_toolkit_tool == Some(tool) { None } else { Some(tool) };

    let is_expanded = self.active_toolkit_tool.is_some();
    if was_expanded != is_expanded {
      let delta = if is_expanded { px(40.0) - px(380.0) } else { px(380.0) - px(40.0) };
      self.prepare_active_editor_for_width_delta(delta, cx);
    }
    cx.notify();
  }

  fn prepare_active_editor_for_width_delta(&mut self, delta: Pixels, cx: &mut Context<Self>) {
    if delta == px(0.0) {
      return;
    }
    if let Some(editor) = self.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.prepare_for_workspace_width_delta(delta, cx));
    }
  }

  fn refresh_outline_tree(&mut self, cx: &mut Context<Self>) {
    let Some(active_id) = self.active_document_id else {
      if self.outline_cache.is_some() {
        self.outline_cache = None;
        self
          .outline_tree
          .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
      }
      return;
    };
    let Some(editor) = &self.active_editor else {
      self.outline_cache = None;
      self
        .outline_tree
        .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
      return;
    };
    let editor = editor.read(cx);
    let edit_generation = editor.edit_generation();
    if self.outline_cache.as_ref().is_some_and(|cache| {
      cache.document_id == active_id && cache.edit_generation == edit_generation && cache.visible_revision == self.outline_revision
    }) {
      return;
    }
    if let Some(cache) = self
      .outline_cache
      .as_mut()
      .filter(|cache| cache.document_id == active_id)
    {
      if cache.edit_generation != edit_generation {
        let structure_changed = cache.update_signature(editor.document(), edit_generation);
        if !structure_changed && cache.visible_revision == self.outline_revision {
          return;
        }
      }
    } else {
      self.outline_cache = Some(OutlineCache::new(active_id, edit_generation, outline_signature(editor.document())));
    }
    let Some(cache) = self.outline_cache.as_mut() else {
      return;
    };
    if cache.visible_revision != self.outline_revision {
      cache.rebuild_visible(self.outline_revision, &self.collapsed_outline_items);
    }
    let items = cache.tree_items.clone();
    self
      .outline_tree
      .update(cx, |tree, cx| tree.set_items(items, cx));
    if let Some(active_paragraph) = self.outline_active_paragraph_for_viewport(self.outline_viewport_paragraph) {
      self.outline_active_paragraph = Some(active_paragraph);
    }
  }

  pub fn scroll_active_editor_to_paragraph(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = &self.active_editor {
      editor.update(cx, |editor, cx| editor.scroll_to_paragraph(paragraph_ix, window, cx));
    }
  }

  fn toggle_outline_item(&mut self, paragraph_ix: usize, cx: &mut Context<Self>) {
    if !self.collapsed_outline_items.insert(paragraph_ix) {
      self.collapsed_outline_items.remove(&paragraph_ix);
    }
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.outline_cache = None;
    self.outline_scrolled_paragraph = None;
    self.refresh_outline_tree(cx);
    cx.notify();
  }

  pub(super) fn toggle_outline_level(&mut self, level: usize, cx: &mut Context<Self>) {
    let Some(editor) = self.active_editor.as_ref() else {
      return;
    };
    let editor = editor.read(cx);
    let signature = outline_signature(editor.document());
    let target_entries: HashSet<usize> = signature
      .entries
      .iter()
      .filter(|entry| entry.level == level)
      .map(|entry| entry.paragraph_ix)
      .collect();

    if target_entries.is_empty() {
      return;
    }

    let any_expanded = target_entries.iter().any(|ix| !self.collapsed_outline_items.contains(ix));
    if any_expanded {
      self.collapsed_outline_items.extend(target_entries);
    } else {
      for ix in target_entries {
        self.collapsed_outline_items.remove(&ix);
      }
    }
    self.outline_revision = self.outline_revision.wrapping_add(1);
    self.outline_cache = None;
    self.outline_scrolled_paragraph = None;
    self.refresh_outline_tree(cx);
    cx.notify();
  }

  pub(super) fn show_outline_context_menu(&mut self, level: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let workspace = cx.entity().downgrade();
    let menu = PopupMenu::build(window, cx, move |menu, _, _| {
      menu.min_w(px(180.0)).item(
        PopupMenuItem::new(format!("Toggle all {}s", outline_level_name(level))).on_click(move |_, _, cx| {
          let _ = workspace.update(cx, |workspace, cx| {
            workspace.outline_context_menu = None;
            workspace.toggle_outline_level(level, cx);
            cx.notify();
          });
        }),
      )
    });

    let _subscription = cx.subscribe(&menu, |workspace, _, _: &DismissEvent, cx| {
      workspace.outline_context_menu = None;
      cx.notify();
    });

    self.outline_context_menu = Some(OutlineContextMenu { position, menu_view: menu, _subscription });
    cx.notify();
  }

  pub fn dirty_editors(&self, cx: &App) -> Vec<Entity<RichTextEditor>> {
    self
      .document_panels
      .iter()
      .filter_map(|panel| {
        let editor = panel.read(cx).editor();
        editor.read(cx).has_unsaved_changes().then_some(editor)
      })
      .collect()
  }

  fn dirty_panels(&self, cx: &App) -> Vec<PanelKind> {
    let mut panels = self
      .document_panels
      .iter()
      .filter_map(|panel| {
        let panel_state = panel.read(cx);
        if !panel_state.is_dirty(cx) {
          return None;
        }
        Some(PanelKind::Document {
          panel: panel.clone(),
          editor: panel_state.editor(),
        })
      })
      .collect::<Vec<_>>();
    panels.extend(self.flow_panels.iter().filter_map(|panel| {
      let panel_state = panel.read(cx);
      if !panel_state.is_dirty(cx) {
        return None;
      }
      Some(PanelKind::Flow {
        panel: panel.clone(),
        editor: panel_state.editor(),
      })
    }));
    panels
  }

  fn activate_document_id(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    if let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      self.active_document_id = Some(panel_id);
      self.active_editor = Some(panel.read(cx).editor());
      self.active_flow = None;
      self.outline_viewport_paragraph = self.active_editor_viewport_paragraph(cx);
      self.outline_active_paragraph = None;
      self.outline_scrolled_paragraph = None;
      if let Some(editor) = self.active_editor.as_ref() {
        let editor = editor.read(cx);
        let signature = outline_signature(editor.document());
        self.collapsed_outline_items = signature.entries.iter().filter(|entry| entry.level == 2).map(|entry| entry.paragraph_ix).collect();
      }
      self.outline_cache = None;
      self.outline_revision = self.outline_revision.wrapping_add(1);
      self.refresh_outline_tree(cx);
      self.persist_temporary_workspace_session(cx);
      cx.notify();
      return;
    }
    if let Some(panel) = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      self.active_document_id = Some(panel_id);
      self.active_editor = None;
      self.active_flow = Some(panel.read(cx).editor());
      self.outline_cache = None;
      self.outline_viewport_paragraph = None;
      self.outline_active_paragraph = None;
      self.outline_scrolled_paragraph = None;
      self.persist_temporary_workspace_session(cx);
      cx.notify();
    }
  }

  fn active_document_index(&self, cx: &App) -> Option<usize> {
    let active_id = self.active_document_id?;
    self
      .document_tabs(cx)
      .iter()
      .position(|tab| tab.id == active_id)
  }

  fn activate_document_at_index(&mut self, index: usize, cx: &mut Context<Self>) {
    let panel_id = self.document_tabs(cx).get(index).map(|tab| tab.id);
    if let Some(panel_id) = panel_id {
      self.activate_document_id(panel_id, cx);
    }
  }

  fn navigate_active_tab(&mut self, offset: isize, cx: &mut Context<Self>) {
    let tabs = self.document_tabs(cx);
    let Some(active_id) = self.active_document_id else {
      return;
    };
    let Some(active_index) = tabs.iter().position(|tab| tab.id == active_id) else {
      return;
    };
    let len = tabs.len();
    if len == 0 {
      return;
    }
    let target = if offset.is_negative() {
      active_index.wrapping_sub(offset.unsigned_abs()) % len
    } else {
      (active_index + offset as usize) % len
    };
    self.activate_document_id(tabs[target].id, cx);
    self.tab_bar_scroll_handle.scroll_to_item(target);
  }

  fn toggle_active_tab_pin(&mut self, cx: &mut Context<Self>) {
    let Some(active_id) = self.active_document_id else {
      return;
    };
    if let Some(ix) = self
      .pinned_document_ids
      .iter()
      .position(|id| *id == active_id)
    {
      self.pinned_document_ids.remove(ix);
    } else if self.pinned_document_ids.len() < 10 {
      self.pinned_document_ids.push(active_id);
    }
    cx.notify();
  }

  pub(crate) fn toggle_speech_document(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    self.speech_document_id = if self.speech_document_id == Some(panel_id) {
      None
    } else {
      Some(panel_id)
    };
    cx.notify();
  }

  fn toggle_tab_pin(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    if let Some(ix) = self
      .pinned_document_ids
      .iter()
      .position(|id| *id == panel_id)
    {
      self.pinned_document_ids.remove(ix);
    } else if self.pinned_document_ids.len() < 10 {
      self.pinned_document_ids.push(panel_id);
    }
    cx.notify();
  }

  fn activate_tab_shortcut(&mut self, index: usize, cx: &mut Context<Self>) {
    let pinned = self
      .pinned_document_ids
      .iter()
      .copied()
      .filter(|id| self.document_tabs(cx).iter().any(|tab| tab.id == *id))
      .collect::<Vec<_>>();
    if let Some(id) = pinned.get(index).copied() {
      self.activate_document_id(id, cx);
    } else if pinned.is_empty() {
      self.activate_document_at_index(index, cx);
    }
  }

  fn condense_active_selection(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(editor) = self.active_editor.clone() else {
      return false;
    };
    editor.update(cx, |editor, cx| {
      let Some(fragment) = editor.fragment_at_selection_or_enclosing_section(&[0, 1, 2, 3]) else {
        return false;
      };
      let paragraphs = if editor.selection().is_caret() {
        condense_card_fragment_paragraphs(fragment.paragraphs, ' ')
      } else {
        condense_fragment_paragraphs(fragment.paragraphs, ' ')
      };
      if paragraphs.is_empty() {
        return false;
      }
      editor.replace_selection_or_enclosing_section_with_paragraphs(paragraphs, &[0, 1, 2, 3], cx);
      true
    })
  }

  pub(crate) fn send_selection_to_speech_document(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some(speech_document_id) = self.speech_document_id else {
      return false;
    };
    if self.active_document_id == Some(speech_document_id) {
      return false;
    }
    let Some(source_editor) = self.active_editor.clone() else {
      return false;
    };
    let Some(speech_editor) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == speech_document_id)
      .map(|panel| panel.read(cx).editor())
    else {
      return false;
    };
    let fragment = source_editor.update(cx, |editor, cx| {
      editor
        .speech_send_fragment_at_selection_or_hover(&[2, 3, 4], window, cx)
        .unwrap_or_else(|| selected_fragment_or_enclosing_section(editor.document(), editor.selection()))
    });
    if fragment.paragraphs.is_empty() && fragment.blocks.is_empty() {
      return false;
    }
    speech_editor.update(cx, |editor, cx| {
      let mut paragraphs = fragment.paragraphs;
      paragraphs.push(InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: String::new(),
          styles: crate::rich_text_element::RunStyles::default(),
        }],
      });
      editor.insert_toolkit_text_at_caret(paragraphs, cx);
    });
    true
  }

  fn apply_document_theme_to_open_editors(&mut self, theme: DocumentTheme, cx: &mut Context<Self>) {
    for panel in &self.document_panels {
      let editor = panel.read(cx).editor();
      let theme = theme.clone();
      editor.update(cx, |editor, cx| {
        editor.update_document_theme(|document_theme| *document_theme = theme, cx);
      });
    }
    cx.notify();
  }

  fn document_tabs(&self, cx: &App) -> Vec<DocumentTab> {
    let mut tabs = self
      .document_panels
      .iter()
      .map(|panel| {
        let panel = panel.read(cx);
        let title = panel.title_text();
        let dirty = panel.is_dirty(cx);
        let title = truncate_tab_title(&title, 32);
        let label = if dirty { format!("*{title}").into() } else { title.into() };
        let id = panel.id();
        DocumentTab {
          id,
          label,
          active: Some(id) == self.active_document_id,
          pinned: false,
          pin_index: None,
          speech: self.speech_document_id == Some(id),
        }
      })
      .collect::<Vec<_>>();
    tabs.extend(self.flow_panels.iter().map(|panel| {
      let panel = panel.read(cx);
      let title = panel.title_text();
      let dirty = panel.is_dirty(cx);
      let title = truncate_tab_title(&title, 32);
      let label = if dirty { format!("*{title}").into() } else { title.into() };
      let id = panel.id();
      DocumentTab {
        id,
        label,
        active: Some(id) == self.active_document_id,
        pinned: false,
        pin_index: None,
        speech: self.speech_document_id == Some(id),
      }
    }));
    ordered_document_tabs(tabs, &self.pinned_document_ids)
  }

  fn active_outline_paragraph(&self, _: &App) -> Option<usize> {
    self.outline_active_paragraph
  }

  fn active_editor_viewport_paragraph(&self, cx: &App) -> Option<usize> {
    self
      .active_editor
      .as_ref()
      .and_then(|editor| editor.read(cx).viewport_anchor_paragraph())
  }

  fn refresh_outline_viewport(&mut self, cx: &mut Context<Self>) {
    let viewport_paragraph = self.active_editor_viewport_paragraph(cx);
    self.update_outline_viewport_paragraph(viewport_paragraph, cx);
  }

  fn update_outline_viewport_paragraph(&mut self, viewport_paragraph: Option<usize>, cx: &mut Context<Self>) {
    let mut changed = false;
    if self.outline_viewport_paragraph != viewport_paragraph {
      self.outline_viewport_paragraph = viewport_paragraph;
      changed = true;
    }
    if let Some(active_paragraph) = self.outline_active_paragraph_for_viewport(viewport_paragraph)
      && self.outline_active_paragraph != Some(active_paragraph)
    {
      self.outline_active_paragraph = Some(active_paragraph);
      changed = true;
    }
    if changed {
      cx.notify();
    }
  }

  fn outline_active_paragraph_for_viewport(&self, viewport_paragraph: Option<usize>) -> Option<usize> {
    let viewport_paragraph = viewport_paragraph?;
    let cache = self.outline_cache.as_ref()?;
    active_visible_outline_paragraph_from_visible(&cache.visible_paragraphs, viewport_paragraph)
  }

  fn scroll_outline_item_into_view(&mut self, paragraph_ix: Option<usize>, cx: &mut Context<Self>) {
    let Some(paragraph_ix) = paragraph_ix else {
      return;
    };
    if self.outline_scrolled_paragraph == Some(paragraph_ix) {
      return;
    }
    let id = outline_item_id(paragraph_ix);
    self.outline_tree.update(cx, |tree, _| {
      if let Some(ix) = tree.item_index_by_id(&id) {
        tree.scroll_to_item(ix, gpui::ScrollStrategy::Center);
      }
    });
    self.outline_scrolled_paragraph = Some(paragraph_ix);
  }
}

fn ordered_document_tabs(mut tabs: Vec<DocumentTab>, pinned_document_ids: &[Uuid]) -> Vec<DocumentTab> {
  for tab in &mut tabs {
    tab.pin_index = pinned_document_ids
      .iter()
      .position(|pinned_id| *pinned_id == tab.id);
    tab.pinned = tab.pin_index.is_some();
  }
  tabs.sort_by_key(|tab| (tab.pin_index.is_none(), tab.pin_index.unwrap_or(usize::MAX)));
  tabs
}

fn pin_shortcut_label(pin_index: usize) -> Option<&'static str> {
  match pin_index {
    0 => Some("1"),
    1 => Some("2"),
    2 => Some("3"),
    3 => Some("4"),
    4 => Some("5"),
    5 => Some("6"),
    6 => Some("7"),
    7 => Some("8"),
    8 => Some("9"),
    9 => Some("0"),
    _ => None,
  }
}

fn condense_fragment_paragraphs(paragraphs: Vec<InputParagraph>, separator: char) -> Vec<InputParagraph> {
  condense_paragraph_group(paragraphs, separator)
    .map(|paragraph| {
      vec![
        paragraph,
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: Vec::new(),
        },
      ]
    })
    .unwrap_or_default()
}

fn condense_card_fragment_paragraphs(paragraphs: Vec<InputParagraph>, separator: char) -> Vec<InputParagraph> {
  let mut output = Vec::with_capacity(paragraphs.len());
  let mut group = Vec::new();
  let mut transformed_any = false;
  for paragraph in paragraphs {
    if card_paragraph_excluded_from_condense(&paragraph) {
      if !group.is_empty()
        && let Some(paragraph) = condense_paragraph_group(std::mem::take(&mut group), separator)
      {
        transformed_any = true;
        output.push(paragraph);
      }
      output.push(paragraph);
    } else {
      group.push(paragraph);
    }
  }
  if !group.is_empty()
    && let Some(paragraph) = condense_paragraph_group(group, separator)
  {
    transformed_any = true;
    output.push(paragraph);
  }
  if transformed_any { output } else { Vec::new() }
}

fn condense_paragraph_group(paragraphs: Vec<InputParagraph>, separator: char) -> Option<InputParagraph> {
  let mut runs = Vec::new();
  for paragraph in paragraphs {
    let mut paragraph_runs = paragraph
      .runs
      .into_iter()
      .filter(|run| !run.text.is_empty())
      .peekable();
    if paragraph_runs.peek().is_none() {
      continue;
    }
    if !runs.is_empty() {
      runs.push(InputRun {
        text: separator.to_string(),
        styles: crate::rich_text_element::RunStyles::default(),
      });
    }
    runs.extend(paragraph_runs);
  }
  (!runs.is_empty()).then_some(InputParagraph {
    style: ParagraphStyle::Normal,
    runs,
  })
}

fn card_paragraph_excluded_from_condense(paragraph: &InputParagraph) -> bool {
  paragraph.style == flowstate_document::PARAGRAPH_TAG
    || paragraph
      .runs
      .iter()
      .any(|run| run.styles.semantic == flowstate_document::SEMANTIC_CITE)
}

fn selected_fragment_or_enclosing_section(
  document: &Document,
  selection: &crate::rich_text_element::EditorSelection,
) -> crate::rich_text_element::RichClipboardFragment {
  if selection.anchor != selection.head {
    return crate::rich_text_element::selected_rich_fragment(
      document,
      selection.anchor.min(selection.head)..selection.anchor.max(selection.head),
    );
  }
  let caret = selection.head;
  let (start_paragraph, end_paragraph_exclusive) = enclosing_section_bounds(document, caret.paragraph, &[2, 3, 4]).unwrap_or((
    caret.paragraph,
    caret
      .paragraph
      .saturating_add(1)
      .min(document.paragraphs.len()),
  ));
  let end_paragraph = end_paragraph_exclusive.saturating_sub(1);
  crate::rich_text_element::selected_rich_fragment(
    document,
    crate::rich_text_element::DocumentOffset {
      paragraph: start_paragraph,
      byte: 0,
    }..crate::rich_text_element::DocumentOffset {
      paragraph: end_paragraph,
      byte: paragraph_byte_range(document, end_paragraph).len(),
    },
  )
}

fn enclosing_section_bounds(document: &Document, paragraph_ix: usize, section_slots: &[u8]) -> Option<(usize, usize)> {
  document
    .sections
    .iter()
    .filter_map(|section| {
      let SectionKind::Custom(slot) = section.kind;
      if !section_slots.contains(&slot) {
        return None;
      }
      let start = paragraph_index_for_id(document, section.start_paragraph)?;
      let end = section
        .end_paragraph_exclusive
        .and_then(|id| paragraph_index_for_id(document, id))
        .unwrap_or(document.paragraphs.len());
      (start <= paragraph_ix && paragraph_ix < end).then_some((start, end))
    })
    .min_by_key(|(start, end)| end - start)
}
