mod rich_text_element;

use gpui::{App, Application, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size};

use crate::rich_text_element::{RichTextEditor, demo_document};

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
  Application::new().run(|cx: &mut App| {
    let bounds = Bounds::centered(None, size(px(900.0), px(700.0)), cx);
    cx.open_window(
      WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..Default::default()
      },
      |_, cx| {
        let editor = cx.new(|cx| RichTextEditor::new(demo_document(), cx));
        cx.new(|_| DemoApp { editor })
      },
    )
    .unwrap();
    cx.activate(true);
  });
}
