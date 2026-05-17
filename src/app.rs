use std::{cell::Cell, path::PathBuf, rc::Rc};

use gpui::{
  App, Application, Bounds, Context, Entity, IntoElement, KeyBinding, PromptButton, PromptLevel, Render, Window, WindowBounds, WindowOptions,
  div, prelude::*, px, rgb, size,
};

use crate::rich_text_element::{
  Backspace, ClearHighlight, Copy, Cut, Delete, DeleteWordBackward, DeleteWordForward, Document, InsertNewline, InsertSoftLineBreak, MoveDocumentEnd,
  MoveDocumentStart, MoveDown, MoveLeft, MoveLineEnd, MoveLineStart, MoveRight, MoveUp, MoveWordLeft, MoveWordRight, PageDown, PageUp, Paste,
  Redo, RichTextEditor, Save, SelectAll, SelectDocumentEnd, SelectDocumentStart, SelectDown, SelectLeft, SelectLineEnd, SelectLineStart,
  SelectPageDown, SelectPageUp, SelectRight, SelectUp, SelectWordLeft, SelectWordRight, ClearFormatting, SetHighlightSpoken, SetParagraphAnalytic,
  SetParagraphBlock, SetParagraphHat, SetParagraphPocket, SetParagraphTag, ToggleCite, ToggleEmphasis, ToggleUnderline, Undo, demo_document,
  load_or_create_document, write_db8,
};

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
  let ctx = Some("RichTextEditor");
  cx.bind_keys([
    KeyBinding::new("left", MoveLeft, ctx),
    KeyBinding::new("right", MoveRight, ctx),
    KeyBinding::new("up", MoveUp, ctx),
    KeyBinding::new("down", MoveDown, ctx),
    KeyBinding::new("home", MoveLineStart, ctx),
    KeyBinding::new("end", MoveLineEnd, ctx),
    KeyBinding::new("shift-left", SelectLeft, ctx),
    KeyBinding::new("shift-right", SelectRight, ctx),
    KeyBinding::new("shift-up", SelectUp, ctx),
    KeyBinding::new("shift-down", SelectDown, ctx),
    KeyBinding::new("shift-home", SelectLineStart, ctx),
    KeyBinding::new("shift-end", SelectLineEnd, ctx),
    KeyBinding::new("ctrl-left", MoveWordLeft, ctx),
    KeyBinding::new("ctrl-right", MoveWordRight, ctx),
    KeyBinding::new("alt-left", MoveWordLeft, ctx),
    KeyBinding::new("alt-right", MoveWordRight, ctx),
    KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
    KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
    KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
    KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
    KeyBinding::new("ctrl-backspace", DeleteWordBackward, ctx),
    KeyBinding::new("ctrl-delete", DeleteWordForward, ctx),
    KeyBinding::new("pageup", PageUp, ctx),
    KeyBinding::new("pagedown", PageDown, ctx),
    KeyBinding::new("shift-pageup", SelectPageUp, ctx),
    KeyBinding::new("shift-pagedown", SelectPageDown, ctx),
    KeyBinding::new("ctrl-home", MoveDocumentStart, ctx),
    KeyBinding::new("ctrl-end", MoveDocumentEnd, ctx),
    KeyBinding::new("ctrl-shift-home", SelectDocumentStart, ctx),
    KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, ctx),
    KeyBinding::new("cmd-a", SelectAll, ctx),
    KeyBinding::new("ctrl-a", SelectAll, ctx),
    KeyBinding::new("cmd-c", Copy, ctx),
    KeyBinding::new("ctrl-c", Copy, ctx),
    KeyBinding::new("cmd-x", Cut, ctx),
    KeyBinding::new("ctrl-x", Cut, ctx),
    KeyBinding::new("cmd-v", Paste, ctx),
    KeyBinding::new("ctrl-v", Paste, ctx),
    KeyBinding::new("cmd-s", Save, ctx),
    KeyBinding::new("ctrl-s", Save, ctx),
    KeyBinding::new("cmd-z", Undo, ctx),
    KeyBinding::new("ctrl-z", Undo, ctx),
    KeyBinding::new("cmd-shift-z", Redo, ctx),
    KeyBinding::new("ctrl-shift-z", Redo, ctx),
    KeyBinding::new("ctrl-y", Redo, ctx),
    KeyBinding::new("f4", SetParagraphPocket, ctx),
    KeyBinding::new("f5", SetParagraphHat, ctx),
    KeyBinding::new("f6", SetParagraphBlock, ctx),
    KeyBinding::new("f7", SetParagraphTag, ctx),
    KeyBinding::new("ctrl-f7", SetParagraphAnalytic, ctx),
    KeyBinding::new("f8", ToggleCite, ctx),
    KeyBinding::new("f9", ToggleUnderline, ctx),
    KeyBinding::new("cmd-u", ToggleUnderline, ctx),
    KeyBinding::new("ctrl-u", ToggleUnderline, ctx),
    KeyBinding::new("f10", ToggleEmphasis, ctx),
    KeyBinding::new("f11", SetHighlightSpoken, ctx),
    KeyBinding::new("f12", ClearFormatting, ctx),
    KeyBinding::new("cmd-b", ToggleEmphasis, ctx),
    KeyBinding::new("ctrl-b", ToggleEmphasis, ctx),
    KeyBinding::new("ctrl-shift-h", ClearHighlight, ctx),
    KeyBinding::new("backspace", Backspace, ctx),
    KeyBinding::new("delete", Delete, ctx),
    KeyBinding::new("enter", InsertNewline, ctx),
    KeyBinding::new("shift-enter", InsertSoftLineBreak, ctx),
  ]);
}

/// Regenerate the bundled demo document. Kept in the library so other tooling
/// can call the same maintenance path as the standalone binary.
pub fn write_demo_document() -> anyhow::Result<()> {
  write_db8("data/demo.db8", &demo_document())?;
  Ok(())
}

/// Run the rich text processor by itself for focused component development.
pub fn run_standalone(document_path: PathBuf) {
  Application::new().run(|cx: &mut App| {
    gpui_component::init(cx);
    register_rich_text_editor_keybindings(cx);
    open_standalone_window(document_path, cx);
    cx.activate(true);
  });
}

fn open_standalone_window(document_path: PathBuf, cx: &mut App) {
  let bounds = Bounds::centered(None, size(px(900.0), px(700.0)), cx);
  cx.open_window(
    WindowOptions {
      window_bounds: Some(WindowBounds::Windowed(bounds)),
      ..Default::default()
    },
    |window, cx| {
      window.set_window_title("Odrenrir - Debate Processor");
      let document =
        load_or_create_document(&document_path).unwrap_or_else(|error| panic!("failed to open {}: {error}", document_path.display()));
      let view = cx.new(|cx| RichTextEditorView::new(document, Some(document_path), cx));
      install_unsaved_close_prompt(view.clone(), window, cx);
      view
    },
  )
  .unwrap();
}

fn install_unsaved_close_prompt(view: Entity<RichTextEditorView>, window: &mut Window, cx: &mut App) {
  let prompt_open = Rc::new(Cell::new(false));
  let allow_close = Rc::new(Cell::new(false));
  let window_handle = window.window_handle();

  window.on_window_should_close(cx, move |window, cx| {
    if allow_close.get() {
      return true;
    }

    let editor = view.read(cx).editor();
    let has_unsaved_changes = editor.update(cx, |editor, _| editor.has_unsaved_changes());
    if !has_unsaved_changes {
      return true;
    }

    if prompt_open.get() {
      return false;
    }
    prompt_open.set(true);

    let answer = window.prompt(
      PromptLevel::Warning,
      "Save changes before closing?",
      Some("This document has unsaved changes."),
      &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
      cx,
    );
    let prompt_open = prompt_open.clone();
    let allow_close = allow_close.clone();

    cx.spawn(async move |cx| {
      let should_close = match answer.await {
        Ok(0) => match editor.update(cx, |editor, cx| editor.save(cx)) {
          Ok(Ok(())) => true,
          Ok(Err(error)) => {
            eprintln!("failed to save before close: {error}");
            let detail = error.to_string();
            let _ = window_handle.update(cx, |_, window, cx| {
              window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
            });
            false
          },
          Err(error) => {
            eprintln!("failed to access editor before close: {error}");
            false
          },
        },
        Ok(1) => match editor.update(cx, |editor, _| editor.discard_recovery_file()) {
          Ok(()) => true,
          Err(error) => {
            eprintln!("failed to access editor before close: {error}");
            false
          },
        },
        _ => false,
      };

      prompt_open.set(false);
      if should_close {
        allow_close.set(true);
        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
      }
    })
    .detach();

    false
  });
}
