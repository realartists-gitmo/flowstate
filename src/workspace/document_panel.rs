use std::path::PathBuf;

use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString, WeakEntity, Window, div, prelude::*};
use gpui_component::ActiveTheme as _;
use gpui_component::dock::{Panel, PanelControl, PanelEvent, PanelInfo, PanelState};
use serde_json::json;
use uuid::Uuid;

use crate::app_settings::load_ribbon_mode;
use crate::ribbon::EditorRibbon;
use crate::rich_text_element::RichTextEditor;
use crate::workspace::Workspace;
use crate::workspace::icons::{AppIcon, icon_button};

pub struct DocumentPanel {
  id: Uuid,
  title: SharedString,
  path: Option<PathBuf>,
  editor: Entity<RichTextEditor>,
  ribbon: Entity<EditorRibbon>,
  workspace: WeakEntity<Workspace>,
  focus_handle: FocusHandle,
  active: bool,
}

#[hotpath::measure_all]
impl DocumentPanel {
  pub fn new_with_title(
    title: Option<String>,
    path: Option<PathBuf>,
    editor: Entity<RichTextEditor>,
    workspace: WeakEntity<Workspace>,
    cx: &mut Context<Self>,
  ) -> Self {
    let ribbon_mode = load_ribbon_mode();
    let ribbon = cx.new(|_| EditorRibbon::new_with_mode(editor.clone(), ribbon_mode));
    let title = title
      .map(Into::into)
      .unwrap_or_else(|| title_for_path(path.as_ref()));

    Self {
      id: Uuid::new_v4(),
      title,
      path,
      editor,
      ribbon,
      workspace,
      focus_handle: cx.focus_handle(),
      active: false,
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

  pub fn title_text(&self) -> SharedString {
    self.title.clone()
  }

  pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.title = title_for_path(Some(&path));
    self.path = Some(path);
    self.editor.update(cx, |editor, cx| {
      editor.set_document_display_name(self.title.clone(), cx);
    });
    cx.notify();
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
      let _ = self.workspace.update(cx, |workspace, cx| {
        workspace.set_active_document(panel_id, editor, cx);
      });
    }
  }

  #[hotpath::measure]
  fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let panel_id = self.id;
    let _ = self.workspace.update(cx, |workspace, cx| {
      workspace.remove_document_panel(panel_id, window, cx);
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
      .bg(cx.theme().background)
      .child(self.editor.clone())
  }
}
