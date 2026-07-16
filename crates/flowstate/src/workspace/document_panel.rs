use std::{collections::HashSet, ops::Range, path::PathBuf};

use gpui::{
  App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString, Subscription, WeakEntity, Window, div,
  prelude::*,
};
use gpui_component::ActiveTheme as _;
use gpui_component::dock::{Panel, PanelControl, PanelEvent, PanelInfo, PanelState};
use serde_json::json;
use uuid::Uuid;

use crate::commands::FindInDocumentAction;
use crate::ribbon::EditorRibbon;
use crate::rich_text_element::{DocumentOffset, RichTextEditor};
use crate::workspace::Workspace;
use crate::workspace::document_search_overlay::{DocumentSearchBar, DocumentSearchBarEvent};
use crate::workspace::icons::{AppIcon, icon_button};

pub struct DocumentPanel {
  id: Uuid,
  title: SharedString,
  path: Option<PathBuf>,
  editor: Entity<RichTextEditor>,
  ribbon: Entity<EditorRibbon>,
  search_bar: Entity<DocumentSearchBar>,
  workspace: WeakEntity<Workspace>,
  focus_handle: FocusHandle,
  _search_bar_subscription: Subscription,
  active: bool,
  search_bar_open: bool,
  search_matches: Vec<Range<DocumentOffset>>,
  active_search_match: Option<usize>,
  pub(crate) collapsed_outline_items: Option<HashSet<usize>>,
  pub(crate) outline_revision: u64,
  pub(crate) outline_scrolled_paragraph: Option<usize>,
}

#[hotpath::measure_all]
impl DocumentPanel {
  pub fn new_with_title(
    title: Option<String>,
    path: Option<PathBuf>,
    editor: Entity<RichTextEditor>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let ribbon = cx.new(|_| EditorRibbon::new(editor.clone()));
    let search_bar = cx.new(|cx| DocumentSearchBar::new(window, cx));
    let _search_bar_subscription = cx.subscribe(&search_bar, |panel, _, event: &DocumentSearchBarEvent, cx| match event {
      DocumentSearchBarEvent::QueryChanged
      | DocumentSearchBarEvent::CaseSensitivityChanged
      | DocumentSearchBarEvent::WholeWordsChanged
      | DocumentSearchBarEvent::RegexModeChanged
      | DocumentSearchBarEvent::StyleFilterChanged => {
        panel.refresh_search_matches(cx);
      },
      DocumentSearchBarEvent::PreviousRequested => panel.select_previous_search_match(cx),
      DocumentSearchBarEvent::NextRequested => panel.select_next_search_match(cx),
      DocumentSearchBarEvent::ApplyReplaceCurrentRequested => panel.apply_replace_current(cx),
      DocumentSearchBarEvent::ApplyReplaceRequested => panel.apply_replace(cx),
      DocumentSearchBarEvent::CloseRequested => panel.close_search_bar(cx),
    });
    let title = title
      .map(Into::into)
      .unwrap_or_else(|| title_for_path(path.as_ref()));

    Self {
      id: Uuid::new_v4(),
      title,
      path,
      editor,
      ribbon,
      search_bar,
      workspace,
      focus_handle: cx.focus_handle(),
      _search_bar_subscription,
      active: false,
      search_bar_open: false,
      search_matches: Vec::new(),
      active_search_match: None,
      collapsed_outline_items: None,
      outline_revision: 0,
      outline_scrolled_paragraph: None,
    }
  }

  pub fn id(&self) -> Uuid {
    self.id
  }

  pub fn editor(&self) -> Entity<RichTextEditor> {
    self.editor.clone()
  }

  pub fn ribbon(&self) -> Entity<EditorRibbon> {
    self.ribbon.clone()
  }

  pub fn search_bar(&self) -> Entity<DocumentSearchBar> {
    self.search_bar.clone()
  }

  pub fn search_bar_open(&self) -> bool {
    self.search_bar_open
  }

  pub fn search_bar_focused(&self, window: &Window, cx: &App) -> bool {
    self.search_bar_open && self.search_bar.read(cx).input_focused(window, cx)
  }

  pub fn search_match_paragraphs(&self) -> impl Iterator<Item = usize> + '_ {
    self
      .search_matches
      .iter()
      .map(|range| range.start.paragraph)
  }

  /// R3-A: recovered-work panels must stay pathless; tests pin it here.
  #[cfg_attr(not(test), allow(dead_code, reason = "headless-test seam"))]
  pub fn path(&self) -> Option<&PathBuf> {
    self.path.as_ref()
  }

  pub fn title_text(&self) -> SharedString {
    self.title.clone()
  }

  /// W-S3: live handoff rebinds the panel to its adopting window's
  /// workspace (actions route through this pointer).
  pub fn set_workspace(&mut self, workspace: WeakEntity<Workspace>) {
    self.workspace = workspace;
  }

  pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.title = title_for_path(Some(&path));
    self.path = Some(path);
    self.editor.update(cx, |editor, cx| {
      editor.set_document_display_name(self.title.clone(), cx);
    });
    cx.notify();
  }

  pub fn open_search_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.search_bar_open = true;
    self.search_bar.update(cx, |search_bar, cx| {
      search_bar.focus_search(window, cx);
    });
    self.refresh_search_matches(cx);
    cx.notify();
  }

  pub fn close_search_bar(&mut self, cx: &mut Context<Self>) {
    self.search_bar_open = false;
    self.search_matches.clear();
    self.active_search_match = None;
    self
      .editor
      .update(cx, |editor, cx| editor.clear_search_highlights(cx));
    self.update_search_bar_count(cx);
    cx.notify();
  }

  fn refresh_search_matches(&mut self, cx: &mut Context<Self>) {
    let (query, case_sensitive, whole_words, use_regex) = {
      let search_bar = self.search_bar.read(cx);
      (
        search_bar.query(cx),
        search_bar.case_sensitive(),
        search_bar.whole_words(),
        search_bar.use_regex(),
      )
    };
    if query.is_empty() {
      self
        .search_bar
        .update(cx, |bar, cx| bar.set_regex_error(None, cx));
      self.search_matches.clear();
      self.active_search_match = None;
    } else {
      // R12-B: regex mode. A pattern that fails to compile surfaces its
      // diagnostic in the bar and clears the matches — never a silent
      // "0 of 0". Whole-words is inert here (regex users spell `\b`).
      let (matches, regex_error) = if use_regex {
        match self.editor.read(cx).find_text_regex(&query, case_sensitive) {
          Ok(matches) => (matches, None),
          Err(error) => (Vec::new(), Some(error)),
        }
      } else {
        (
          self
            .editor
            .read(cx)
            .find_text_with_options(&query, case_sensitive, whole_words),
          None,
        )
      };
      self
        .search_bar
        .update(cx, |bar, cx| bar.set_regex_error(regex_error, cx));
      self.search_matches = matches;
      self.search_matches.retain(|range| {
        let search_bar = self.search_bar.read(cx);
        let paragraphs = &self.editor.read(cx).document().paragraphs;
        let Some(paragraph) = paragraphs.get(range.start.paragraph) else {
          return false;
        };
        if !search_bar.paragraph_style_enabled(paragraph.style) {
          return false;
        }
        if search_bar.has_active_run_filters() {
          return search_bar.run_style_matches_for_range(paragraph, range);
        }
        true
      });
      self.active_search_match = (!self.search_matches.is_empty()).then_some(0);
    }
    self.editor.update(cx, |editor, cx| {
      editor.set_search_highlights(self.search_matches.clone(), self.active_search_match, cx);
    });
    self.update_search_bar_count(cx);
    cx.notify();
  }

  /// R12-B: per-match capture expansions for the CURRENT matches when the
  /// bar is in regex mode and the template references captures (`$`).
  /// `None` = literal replace (regex off, or nothing to expand).
  fn regex_replace_overrides(&self, template: &str, cx: &Context<Self>) -> Option<Vec<Option<String>>> {
    let search_bar = self.search_bar.read(cx);
    if !search_bar.use_regex() || !template.contains('$') {
      return None;
    }
    let query = search_bar.query(cx);
    let case_sensitive = search_bar.case_sensitive();
    Some(
      self
        .editor
        .read(cx)
        .regex_search_replacement_expansions(&query, case_sensitive, template),
    )
  }

  fn apply_replace_current(&mut self, cx: &mut Context<Self>) {
    self.refresh_search_matches(cx);
    if self.search_matches.is_empty() || self.active_search_match.is_none() {
      return;
    }
    let replacement = self.search_bar.read(cx).replacement(cx);
    let effective = self
      .regex_replace_overrides(&replacement, cx)
      .zip(self.active_search_match)
      .and_then(|(overrides, active)| overrides.get(active).cloned().flatten())
      .unwrap_or_else(|| replacement.clone());
    let replaced = self
      .editor
      .update(cx, |editor, cx| editor.replace_active_search_highlight(&effective, cx));
    if replaced {
      self.refresh_search_matches(cx);
    }
  }

  fn apply_replace(&mut self, cx: &mut Context<Self>) {
    // Recompute immediately before replacing. Undo/redo can change the
    // document without going through the search bar, so cached match offsets
    // may be stale after a large replace + undo cycle.
    self.refresh_search_matches(cx);
    if self.search_matches.is_empty() {
      return;
    }
    let replacement = self.search_bar.read(cx).replacement(cx);
    let overrides = self.regex_replace_overrides(&replacement, cx).unwrap_or_default();
    let replaced = self.editor.update(cx, |editor, cx| {
      editor.replace_all_search_highlights_with(&replacement, overrides, cx)
    });
    if replaced > 0 {
      self.refresh_search_matches(cx);
    }
  }

  fn select_previous_search_match(&mut self, cx: &mut Context<Self>) {
    let count = self.search_matches.len();
    if count == 0 {
      self.active_search_match = None;
    } else {
      self.active_search_match = Some(
        self
          .active_search_match
          .map_or(count - 1, |ix| ix.checked_sub(1).unwrap_or(count - 1)),
      );
    }
    self.jump_to_active_search_match(cx);
  }

  fn select_next_search_match(&mut self, cx: &mut Context<Self>) {
    let count = self.search_matches.len();
    if count == 0 {
      self.active_search_match = None;
    } else {
      self.active_search_match = Some(self.active_search_match.map_or(0, |ix| (ix + 1) % count));
    }
    self.jump_to_active_search_match(cx);
  }

  fn jump_to_active_search_match(&mut self, cx: &mut Context<Self>) {
    self.update_search_bar_count(cx);
    self.editor.update(cx, |editor, cx| {
      editor.set_active_search_highlight(self.active_search_match, cx);
    });
  }

  fn update_search_bar_count(&self, cx: &mut Context<Self>) {
    self.search_bar.update(cx, |search_bar, cx| {
      search_bar.set_match_position(self.active_search_match, self.search_matches.len(), cx);
    });
  }

  fn on_find_in_document(&mut self, _: &FindInDocumentAction, window: &mut Window, cx: &mut Context<Self>) {
    self.open_search_bar(window, cx);
  }

  pub fn is_dirty(&self, cx: &App) -> bool {
    self.editor.read(cx).has_unsaved_changes()
  }

  fn display_title(&self, cx: &App) -> SharedString {
    if self.is_dirty(cx) {
      format!("{} *", self.title).into()
    } else {
      self.title.clone()
    }
  }
}

#[hotpath::measure]
fn title_for_path(path: Option<&PathBuf>) -> SharedString {
  path
    .and_then(|path| path.file_name())
    .map(|name| name.to_string_lossy().to_string())
    .unwrap_or_else(|| "Untitled.db8".to_string())
    .into()
}

impl EventEmitter<PanelEvent> for DocumentPanel {}

#[hotpath::measure_all]
impl Focusable for DocumentPanel {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

#[hotpath::measure_all]
impl Panel for DocumentPanel {
  fn panel_name(&self) -> &'static str {
    "DocumentPanel"
  }

  #[hotpath::measure]
  fn tab_name(&self, cx: &App) -> Option<SharedString> {
    Some(self.display_title(cx))
  }

  #[hotpath::measure]
  fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.display_title(cx).clone()
  }

  #[hotpath::measure]
  fn title_suffix(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> Option<impl IntoElement> {
    let workspace = self.workspace.clone();
    let panel_id = self.id;
    Some(
      icon_button(("close-document-panel", panel_id.as_u128() as u64), AppIcon::Close)
        .tooltip("Close document")
        .on_click(move |_, window, cx| {
          let _ = workspace.update(cx, |workspace, cx| workspace.close_document_panel(panel_id, window, cx));
        }),
    )
  }

  #[hotpath::measure]
  fn closable(&self, _: &App) -> bool {
    false
  }

  #[hotpath::measure]
  fn zoomable(&self, _: &App) -> Option<PanelControl> {
    Some(PanelControl::Both)
  }

  #[hotpath::measure]
  fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
    self.active = active;
    if active {
      let editor = self.editor.clone();
      let panel_id = self.id;
      let workspace = self.workspace.clone();
      // Dock plumbing may invoke this while the Workspace lease is held
      // (same double-lease shape as the fixed share-dialog panic) — defer.
      cx.defer(move |cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.set_active_document(panel_id, editor, cx);
        });
      });
    }
  }

  #[hotpath::measure]
  fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let panel_id = self.id;
    let workspace = self.workspace.clone();
    window.defer(cx, move |window, cx| {
      let _ = workspace.update(cx, |workspace, cx| {
        workspace.remove_document_panel(panel_id, window, cx);
      });
    });
  }

  #[hotpath::measure]
  fn dump(&self, _: &App) -> PanelState {
    PanelState {
      panel_name: self.panel_name().to_string(),
      children: Vec::new(),
      info: PanelInfo::panel(json!({
        "id": self.id,
        "path": self.path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "title": self.title.to_string(),
      })),
    }
  }

  #[hotpath::measure]
  fn inner_padding(&self, _: &App) -> bool {
    false
  }
}

#[hotpath::measure_all]
impl Render for DocumentPanel {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size_full()
      .flex()
      .flex_col()
      .bg(cx.theme().background)
      .on_action(cx.listener(Self::on_find_in_document))
      .when(self.search_bar_open, |this| this.child(self.search_bar.clone()))
      .child(div().flex_1().overflow_hidden().child(self.editor.clone()))
  }
}
