use std::cell::RefCell;

use gpui::{App, AppContext as _, ApplicationHandle, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_flowtext::{RichTextEditor, demo_document};
use wasm_bindgen::prelude::wasm_bindgen;

thread_local! {
  static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

#[wasm_bindgen(start)]
pub fn start() {
  gpui_platform::web_init();
  let application = gpui_platform::single_threaded_web().run_embedded(|cx: &mut App| {
    gpui_component::init(cx);
    let bounds = Bounds::centered(None, size(px(1024.0), px(768.0)), cx);
    cx.open_window(
      WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..Default::default()
      },
      |_window, cx| cx.new(|cx| RichTextEditor::new_with_path(demo_document(), None, cx)),
    )
    .expect("failed to open Flowstate web window");
    cx.activate(true);
  });
  APPLICATION.with(|slot| *slot.borrow_mut() = Some(application));
}
