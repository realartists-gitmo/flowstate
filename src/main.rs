mod rich_text_element;

use gpui::{
  App, Application, Bounds, Context, IntoElement, KeyBinding, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};

use crate::rich_text_element::{
  Backspace, Delete, InsertNewline, MoveDown, MoveLeft, MoveLineEnd, MoveLineStart, MoveRight, MoveUp, RichTextEditor, Save, SelectAll,
  SelectDown, SelectLeft, SelectLineEnd, SelectLineStart, SelectRight, SelectUp, demo_document, load_or_create_document, write_db8,
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
  if std::env::args().any(|arg| arg == "--write-demo-db8") {
    write_db8("data/demo.db8", &demo_document()).expect("failed to write data/demo.db8");
    return;
  }

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
      // Cmd is the platform key on macOS, Ctrl elsewhere. Bind both so
      // Select-All works regardless of OS.
      KeyBinding::new("cmd-a", SelectAll, ctx),
      KeyBinding::new("ctrl-a", SelectAll, ctx),
      KeyBinding::new("cmd-s", Save, ctx),
      KeyBinding::new("ctrl-s", Save, ctx),
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
      |_, cx| {
        let document_path = std::path::PathBuf::from("data/demo.db8");
        let document = load_or_create_document(&document_path).expect("failed to open data/demo.db8");
        let editor = cx.new(|cx| RichTextEditor::new_with_path(document, Some(document_path), cx));
        cx.new(|_| DemoApp { editor })
      },
    )
    .unwrap();
    cx.activate(true);
  });
}
