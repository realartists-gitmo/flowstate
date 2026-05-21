use std::path::{Path, PathBuf};

use gpui::{App, Application, Context, Entity, IntoElement, Render, Window, div, prelude::*, rgb};
use gpui_component::{Theme, ThemeRegistry};

use crate::app_settings::load_app_settings;
use crate::commands::register_default_keybindings;
use crate::rich_text_element::{Document, RichTextEditor, demo_document, write_db8};
use crate::workspace::open_workspace_window;

/// A reusable GPUI render component for the debate rich text editor.
///
/// GPUI renders application state through entities. This wrapper lets the full
/// editor mount the rich text editor as a child component while still keeping
/// direct access to the underlying `RichTextEditor` entity for save checks,
/// document inspection, or command dispatch.
pub struct RichTextEditorView {
  editor: Entity<RichTextEditor>,
}

impl RichTextEditorView {
  /// Create a new editor entity from a loaded document.
  pub fn new(document: Document, document_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
    let editor = cx.new(|cx| RichTextEditor::new_with_path(document, document_path, cx));
    Self { editor }
  }

  /// Wrap an editor entity that was created by a parent application.
  pub fn from_editor(editor: Entity<RichTextEditor>) -> Self {
    Self { editor }
  }

  /// Expose the child editor entity so host applications can read or update it.
  pub fn editor(&self) -> Entity<RichTextEditor> {
    self.editor.clone()
  }
}

impl Render for RichTextEditorView {
  fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size_full()
      .bg(rgb(0xffffff))
      .child(self.editor.clone())
  }
}

/// Register the default editing shortcuts expected by `RichTextEditor`.
///
/// Host applications should call this once during GPUI app startup. The
/// keybindings target the `RichTextEditor` key context, so they only fire when
/// the rich text component has focus.
pub fn register_rich_text_editor_keybindings(cx: &mut App) {
  register_default_keybindings(cx);
}

/// Regenerate the bundled demo document. Kept in the library so other tooling
/// can call the same maintenance path as the standalone binary.
pub fn write_demo_document() -> anyhow::Result<()> {
  write_db8("data/demo.db8", &demo_document())?;
  Ok(())
}

/// Run the rich text processor by itself for focused component development.
pub fn run_standalone(document_path: Option<PathBuf>) {
  Application::new()
    .with_assets(gpui_component_assets::Assets)
    .run(|cx: &mut App| {
      gpui_component::init(cx);
      init_theme_registry(cx);
      apply_saved_theme(cx);
      register_rich_text_editor_keybindings(cx);
      open_workspace_window(document_path, cx);
      cx.activate(true);
    });
}

fn init_theme_registry(cx: &mut App) {
  let themes_dir = vendored_themes_dir();
  if let Err(error) = ThemeRegistry::watch_dir(themes_dir, cx, apply_saved_theme) {
    eprintln!("failed to load GPUI component themes: {error}");
  }
}

fn vendored_themes_dir() -> PathBuf {
  let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
  manifest_dir
    .join("vendor")
    .join("gpui-component")
    .join("themes")
}

fn apply_saved_theme(cx: &mut App) {
  if let Some(theme_name) = load_app_settings().theme_name
    && let Some(theme) = ThemeRegistry::global(cx)
      .themes()
      .get(theme_name.as_str())
      .cloned()
  {
    let mode = theme.mode;
    Theme::global_mut(cx).apply_config(&theme);
    Theme::change(mode, None, cx);
  }

  apply_global_ui_font(cx);
}

fn apply_global_ui_font(cx: &mut App) {
  let mono_font_family = Theme::global(cx).mono_font_family.clone();
  Theme::global_mut(cx).font_family = mono_font_family;
}
