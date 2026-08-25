use std::cell::RefCell;

use gpui::{App, ApplicationHandle};
use gpui_component::{Theme, ThemeMode};
use wasm_bindgen::prelude::wasm_bindgen;

use crate::app::{AppAssets, install_prompt_renderer, register_rich_text_editor_keybindings};
use crate::workspace::open_workspace_window;

thread_local! {
  static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

#[wasm_bindgen(start)]
pub fn start() {
  gpui_platform::web_init();
  let application = gpui_platform::single_threaded_web()
    .with_assets(AppAssets::default())
    .run_embedded(|cx: &mut App| {
      gpui_component::init(cx);
      crate::web_fonts::register(cx);
      Theme::change(ThemeMode::Dark, None, cx);
      register_rich_text_editor_keybindings(cx);
      install_prompt_renderer(cx);
      open_workspace_window(None, cx);
      cx.activate(true);
    });
  APPLICATION.with(|slot| *slot.borrow_mut() = Some(application));
}
