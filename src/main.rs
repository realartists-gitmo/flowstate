mod rich_text_element;

use std::{cell::Cell, rc::Rc};

use gpui::{
  App, Application, Bounds, Context, IntoElement, KeyBinding, PromptButton, PromptLevel, Render, Window, WindowBounds, WindowOptions, div,
  prelude::*, px, rgb, size,
};

use crate::rich_text_element::{
  Backspace, ClearHighlight, Copy, Cut, Delete, DeleteWordBackward, DeleteWordForward, InsertNewline, MoveDocumentEnd, MoveDocumentStart,
  MoveDown, MoveLeft, MoveLineEnd, MoveLineStart, MoveRight, MoveUp, MoveWordLeft, MoveWordRight, PageDown, PageUp, Paste, Redo, RichTextEditor,
  Save, SelectAll, SelectDocumentEnd, SelectDocumentStart, SelectDown, SelectLeft, SelectLineEnd, SelectLineStart, SelectPageDown, SelectPageUp,
  SelectRight, SelectUp, SelectWordLeft, SelectWordRight, ToggleEmphasis, ToggleUnderline, Undo, demo_document, load_or_create_document,
  write_db8,
};

struct DemoApp {
  editor: gpui::Entity<RichTextEditor>,
}

impl Render for DemoApp {
  fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size_full()
      .bg(rgb(0xffffff))
      .child(self.editor.clone())
  }
}

fn main() {
  let mut args = std::env::args().skip(1);
  let document_path = match args.next() {
    Some(arg) if arg == "--write-demo-db8" => {
      if args.next().is_some() {
        eprintln!("usage: debateprocessor [path/to/document.db8]");
        std::process::exit(2);
      }
      write_db8("data/demo.db8", &demo_document()).expect("failed to write data/demo.db8");
      return;
    },
    Some(arg) if arg == "-h" || arg == "--help" => {
      eprintln!("usage: debateprocessor [path/to/document.db8]");
      return;
    },
    Some(arg) => {
      if args.next().is_some() {
        eprintln!("usage: debateprocessor [path/to/document.db8]");
        std::process::exit(2);
      }
      std::path::PathBuf::from(arg)
    },
    None => std::path::PathBuf::from("data/demo.db8"),
  };

  Application::new().run(|cx: &mut App| {
    gpui_component::init(cx);

    // Register editing keybindings. Each binding fires its action only when
    // a `RichTextEditor`-contexted element has focus.
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
      // Cmd is the platform key on macOS, Ctrl elsewhere. Bind both so
      // Select-All works regardless of OS.
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
      KeyBinding::new("cmd-u", ToggleUnderline, ctx),
      KeyBinding::new("ctrl-u", ToggleUnderline, ctx),
      KeyBinding::new("cmd-b", ToggleEmphasis, ctx),
      KeyBinding::new("ctrl-b", ToggleEmphasis, ctx),
      KeyBinding::new("ctrl-shift-h", ClearHighlight, ctx),
      KeyBinding::new("backspace", Backspace, ctx),
      KeyBinding::new("delete", Delete, ctx),
      KeyBinding::new("enter", InsertNewline, ctx),
    ]);

    let bounds = Bounds::centered(None, size(px(900.0), px(700.0)), cx);
    cx.open_window(
      WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..Default::default()
      },
      |window, cx| {
        let document = load_or_create_document(&document_path).unwrap_or_else(|error| panic!("failed to open {}: {error}", document_path.display()));
        let editor = cx.new(|cx| RichTextEditor::new_with_path(document, Some(document_path), cx));
        let prompt_open = Rc::new(Cell::new(false));
        let allow_close = Rc::new(Cell::new(false));
        let window_handle = window.window_handle();
        let editor_for_close = editor.clone();
        let prompt_open_for_close = prompt_open.clone();
        let allow_close_for_close = allow_close.clone();

        window.on_window_should_close(cx, move |window, cx| {
          if allow_close_for_close.get() {
            return true;
          }

          let has_unsaved_changes = editor_for_close.update(cx, |editor, _| editor.has_unsaved_changes());
          if !has_unsaved_changes {
            return true;
          }

          if prompt_open_for_close.get() {
            return false;
          }
          prompt_open_for_close.set(true);

          let answer = window.prompt(
            PromptLevel::Warning,
            "Save changes before closing?",
            Some("This document has unsaved changes."),
            &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
            cx,
          );
          let editor = editor_for_close.clone();
          let prompt_open = prompt_open_for_close.clone();
          let allow_close = allow_close_for_close.clone();

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
              Ok(1) => true,
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

        cx.new(|_| DemoApp { editor })
      },
    )
    .unwrap();
    cx.activate(true);
  });
}
