use std::{
  borrow::Cow,
  io,
  path::{Path, PathBuf},
  sync::Arc,
};

use gpui::{
  App, Application, AssetSource, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
  ParentElement, PromptButton, PromptHandle, PromptLevel, PromptResponse, Render, RenderablePromptHandle, Result, SharedString, Styled, Window,
  actions, div, prelude::*, px, relative, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, Theme, ThemeRegistry, h_flex, v_flex};

use crate::app_settings::load_app_settings;
use crate::commands::register_default_keybindings;
use crate::rich_text_element::{
  Document, DocumentExportAdapter, DocumentExportFormat, RichTextEditor, demo_document, set_document_export_adapter, write_db8,
};
use crate::workspace::open_workspace_window;

const PROMPT_CONTEXT: &str = "FlowPrompt";

actions!(flow_prompt, [FlowPromptAccept, FlowPromptCancel]);

/// A reusable GPUI render component for the debate rich text editor.
///
/// GPUI renders application state through entities. This wrapper lets the full
/// editor mount the rich text editor as a child component while still keeping
/// direct access to the underlying `RichTextEditor` entity for save checks,
/// document inspection, or command dispatch.
pub struct RichTextEditorView {
  editor: Entity<RichTextEditor>,
}

#[hotpath::measure_all]
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

#[hotpath::measure_all]
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
#[hotpath::measure]
pub fn register_rich_text_editor_keybindings(cx: &mut App) {
  register_default_keybindings(cx);
}

#[hotpath::measure]
fn install_prompt_renderer(cx: &mut App) {
  cx.bind_keys([
    KeyBinding::new("enter", FlowPromptAccept, Some(PROMPT_CONTEXT)),
    KeyBinding::new("escape", FlowPromptCancel, Some(PROMPT_CONTEXT)),
  ]);
  cx.set_prompt_builder(flow_prompt_renderer);
}

#[hotpath::measure]
fn flow_prompt_renderer(
  level: PromptLevel,
  message: &str,
  detail: Option<&str>,
  actions: &[PromptButton],
  handle: PromptHandle,
  window: &mut Window,
  cx: &mut App,
) -> RenderablePromptHandle {
  let renderer = cx.new(|cx| FlowPromptRenderer {
    level,
    message: message.to_string(),
    detail: detail.map(wrap_prompt_detail),
    actions: actions.to_vec(),
    focus: cx.focus_handle(),
  });
  let prompt = handle.with_view(renderer, window, cx);
  window.refresh();
  prompt
}

struct FlowPromptRenderer {
  level: PromptLevel,
  message: String,
  detail: Option<String>,
  actions: Vec<PromptButton>,
  focus: FocusHandle,
}

#[hotpath::measure_all]
impl FlowPromptRenderer {
  fn accept_index(&self) -> Option<usize> {
    self
      .actions
      .iter()
      .position(|action| matches!(action, PromptButton::Ok(_)))
      .or_else(|| (!self.actions.is_empty()).then_some(0))
  }

  fn cancel_index(&self) -> Option<usize> {
    self
      .actions
      .iter()
      .position(|action| matches!(action, PromptButton::Cancel(_)))
      .or_else(|| self.actions.len().checked_sub(1))
  }

  fn on_accept(&mut self, _: &FlowPromptAccept, _: &mut Window, cx: &mut Context<Self>) {
    if let Some(ix) = self.accept_index() {
      cx.emit(PromptResponse(ix));
    }
  }

  fn on_cancel(&mut self, _: &FlowPromptCancel, _: &mut Window, cx: &mut Context<Self>) {
    if let Some(ix) = self.cancel_index() {
      cx.emit(PromptResponse(ix));
    }
  }
}

#[hotpath::measure_all]
impl Render for FlowPromptRenderer {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let (icon, accent) = match self.level {
      PromptLevel::Info => (IconName::Info, cx.theme().info),
      PromptLevel::Warning => (IconName::TriangleAlert, cx.theme().warning),
      PromptLevel::Critical => (IconName::CircleX, cx.theme().danger),
    };

    div()
      .size_full()
      .bg(cx.theme().overlay)
      .occlude()
      .flex()
      .items_center()
      .justify_center()
      .child(
        v_flex()
          .key_context(PROMPT_CONTEXT)
          .track_focus(&self.focus)
          .tab_group()
          .w(px(440.0))
          .max_w(px(560.0))
          .max_w_full()
          .max_h(px(420.0))
          .bg(cx.theme().background)
          .border_1()
          .border_color(cx.theme().border)
          .rounded(cx.theme().radius_lg)
          .shadow_lg()
          .p_5()
          .gap_4()
          .overflow_hidden()
          .on_action(cx.listener(Self::on_accept))
          .on_action(cx.listener(Self::on_cancel))
          .child(
            h_flex()
              .items_start()
              .gap_3()
              .child(
                div()
                  .flex_none()
                  .size_8()
                  .rounded(px(8.0))
                  .bg(accent.opacity(0.14))
                  .flex()
                  .items_center()
                  .justify_center()
                  .child(Icon::new(icon).with_size(px(18.0)).text_color(accent)),
              )
              .child(
                v_flex()
                  .min_w(px(0.0))
                  .flex_1()
                  .gap_1()
                  .child(
                    div()
                      .min_w(px(0.0))
                      .text_lg()
                      .font_semibold()
                      .line_height(relative(1.2))
                      .whitespace_normal()
                      .text_color(cx.theme().foreground)
                      .child(self.message.clone()),
                  )
                  .children(self.detail.clone().map(|detail| {
                    div()
                      .min_w(px(0.0))
                      .max_h(px(260.0))
                      .overflow_y_scrollbar()
                      .text_sm()
                      .line_height(relative(1.45))
                      .whitespace_normal()
                      .text_color(cx.theme().muted_foreground)
                      .child(detail)
                  })),
              ),
          )
          .child(
            h_flex()
              .justify_end()
              .gap_2()
              .children(self.actions.iter().enumerate().map(|(ix, action)| {
                let label = action.label().clone();
                let button = Button::new(("prompt-action", ix as u64))
                  .label(label)
                  .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(PromptResponse(ix));
                  }));

                match action {
                  PromptButton::Ok(_) => button.primary(),
                  PromptButton::Cancel(_) => button,
                  PromptButton::Other(_) => button.outline(),
                }
              })),
          ),
      )
  }
}

#[hotpath::measure]
fn wrap_prompt_detail(detail: &str) -> String {
  const MAX_RUN: usize = 72;
  let mut wrapped = String::with_capacity(detail.len() + detail.len() / MAX_RUN);
  let mut run_len = 0usize;

  for ch in detail.chars() {
    if ch == '\n' {
      wrapped.push(ch);
      run_len = 0;
      continue;
    }
    if ch.is_whitespace() {
      wrapped.push(ch);
      run_len = 0;
      continue;
    }
    if run_len >= MAX_RUN {
      wrapped.push('\n');
      run_len = 0;
    }
    wrapped.push(ch);
    run_len += 1;
  }

  wrapped
}

impl EventEmitter<PromptResponse> for FlowPromptRenderer {}

#[hotpath::measure_all]
impl Focusable for FlowPromptRenderer {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus.clone()
  }
}

/// Regenerate the bundled demo document. Kept in the library so other tooling
/// can call the same maintenance path as the standalone binary.
#[hotpath::measure]
pub fn write_demo_document() -> anyhow::Result<()> {
  write_db8("data/demo.db8", &demo_document())?;
  Ok(())
}

struct FlowstateDocumentExportAdapter;

impl DocumentExportAdapter for FlowstateDocumentExportAdapter {
  fn send_output_directory(&self, source_path: Option<&Path>, recovery_path: Option<&Path>) -> Option<PathBuf> {
    if crate::app_settings::load_send_to_document_directory() {
      source_path
        .and_then(Path::parent)
        .or_else(|| recovery_path.and_then(Path::parent))
        .map(Path::to_path_buf)
    } else {
      crate::app_settings::load_send_custom_directory()
    }
  }

  fn write_document_export(&self, output_path: &Path, document: &Document, format: DocumentExportFormat) -> io::Result<()> {
    match format {
      DocumentExportFormat::Db8 => write_db8(output_path, document),
      DocumentExportFormat::Docx => crate::docx_conversion::write_docx(output_path, document),
      DocumentExportFormat::Pdf => crate::docx_conversion::write_pdf(output_path, document),
    }
  }
}

#[hotpath::measure]
fn install_flowtext_adapters() {
  let _ = set_document_export_adapter(Arc::new(FlowstateDocumentExportAdapter));
}

/// Run the rich text processor by itself for focused component development.
#[hotpath::measure]
pub fn run_standalone(document_path: Option<PathBuf>) {
  Application::new()
    .with_assets(AppAssets)
    .run(|cx: &mut App| {
      gpui_component::init(cx);
      init_theme_registry(cx);
      apply_saved_theme(cx);
      register_rich_text_editor_keybindings(cx);
      install_prompt_renderer(cx);
      install_flowtext_adapters();
      open_workspace_window(document_path, cx);
      cx.activate(true);
    });
}

struct AppAssets;

#[hotpath::measure_all]
impl AssetSource for AppAssets {
  fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
    match path {
      "icons/save.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/save.svg")))),
      "icons/bold.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/bold.svg")))),
      "icons/eraser.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/eraser.svg")))),
      "icons/highlighter.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/highlighter.svg")))),
      "icons/shrink.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/shrink.svg")))),
      "icons/strikethrough.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/strikethrough.svg")))),
      "icons/underline.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/underline.svg")))),
      "icons/archive.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/archive.svg")))),
      "icons/file-search-corner.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/file-search-corner.svg")))),
      "icons/notebook-text.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/notebook-text.svg")))),
      "icons/table-of-contents.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/table-of-contents.svg")))),
      "icons/panel-top-open.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/panel-top-open.svg")))),
      "icons/panel-top-close.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/panel-top-close.svg")))),
      "icons/caret-down.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/caret-down.svg")))),
      "icons/caret-right.svg" => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/caret-right.svg")))),
      _ => gpui_component_assets::Assets.load(path),
    }
  }

  #[hotpath::measure]
  fn list(&self, path: &str) -> Result<Vec<SharedString>> {
    let mut assets = gpui_component_assets::Assets.list(path)?;
    if "icons/save.svg".starts_with(path) {
      assets.push("icons/save.svg".into());
    }
    if "icons/bold.svg".starts_with(path) {
      assets.push("icons/bold.svg".into());
    }
    if "icons/eraser.svg".starts_with(path) {
      assets.push("icons/eraser.svg".into());
    }
    if "icons/highlighter.svg".starts_with(path) {
      assets.push("icons/highlighter.svg".into());
    }
    if "icons/shrink.svg".starts_with(path) {
      assets.push("icons/shrink.svg".into());
    }
    if "icons/strikethrough.svg".starts_with(path) {
      assets.push("icons/strikethrough.svg".into());
    }
    if "icons/underline.svg".starts_with(path) {
      assets.push("icons/underline.svg".into());
    }
    if "icons/archive.svg".starts_with(path) {
      assets.push("icons/archive.svg".into());
    }
    if "icons/file-search-corner.svg".starts_with(path) {
      assets.push("icons/file-search-corner.svg".into());
    }
    if "icons/notebook-text.svg".starts_with(path) {
      assets.push("icons/notebook-text.svg".into());
    }
    if "icons/table-of-contents.svg".starts_with(path) {
      assets.push("icons/table-of-contents.svg".into());
    }
    if "icons/panel-top-open.svg".starts_with(path) {
      assets.push("icons/panel-top-open.svg".into());
    }
    if "icons/panel-top-close.svg".starts_with(path) {
      assets.push("icons/panel-top-close.svg".into());
    }
    if "icons/caret-down.svg".starts_with(path) {
      assets.push("icons/caret-down.svg".into());
    }
    if "icons/caret-right.svg".starts_with(path) {
      assets.push("icons/caret-right.svg".into());
    }
    Ok(assets)
  }
}

#[hotpath::measure]
fn init_theme_registry(cx: &mut App) {
  let themes_dir = vendored_themes_dir();
  if let Err(error) = ThemeRegistry::watch_dir(themes_dir, cx, apply_saved_theme) {
    eprintln!("failed to load GPUI component themes: {error}");
  }
}

#[hotpath::measure]
fn vendored_themes_dir() -> PathBuf {
  let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
  let workspace_dir = manifest_dir
    .parent()
    .and_then(Path::parent)
    .unwrap_or(manifest_dir);
  workspace_dir
    .join("vendor")
    .join("gpui-component")
    .join("themes")
}

#[hotpath::measure]
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

#[hotpath::measure]
fn apply_global_ui_font(cx: &mut App) {
  let mono_font_family = Theme::global(cx).mono_font_family.clone();
  Theme::global_mut(cx).font_family = mono_font_family;
}
