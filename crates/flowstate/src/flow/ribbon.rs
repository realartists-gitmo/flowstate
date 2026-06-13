use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Window, div, prelude::*, px};

use crate::flow::FlowEditor;

pub struct FlowRibbon {
  editor: Entity<FlowEditor>,
  focus_handle: FocusHandle,
  height: gpui::Pixels,
}

impl FlowRibbon {
  pub fn new(editor: Entity<FlowEditor>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self {
      editor,
      focus_handle: cx.focus_handle(),
      height: px(112.0),
    }
  }

  pub fn editor(&self) -> Entity<FlowEditor> {
    self.editor.clone()
  }

  pub fn set_height(&mut self, height: gpui::Pixels, cx: &mut Context<Self>) {
    if self.height != height {
      self.height = height;
      cx.notify();
    }
  }
}

impl EventEmitter<()> for FlowRibbon {}

impl Focusable for FlowRibbon {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowRibbon {
  fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    div().w_full().h(self.height)
  }
}
