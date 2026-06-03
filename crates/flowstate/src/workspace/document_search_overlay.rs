use gpui::{
  Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render, Window, div, prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, Sizable as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputState},
};

#[derive(Clone, Copy, Debug)]
pub enum DocumentSearchBarEvent {
  CloseRequested,
}

pub struct DocumentSearchBar {
  search_input: Entity<InputState>,
}

#[hotpath::measure_all]
impl DocumentSearchBar {
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in document"));
    Self { search_input }
  }

  pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.search_input.focus_handle(cx).focus(window);
  }

  pub fn query(&self, cx: &gpui::App) -> String {
    self.search_input.read(cx).value().to_string()
  }

  fn close(&self, cx: &mut Context<Self>) {
    cx.emit(DocumentSearchBarEvent::CloseRequested);
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
    if event.keystroke.key == "escape" {
      self.close(cx);
      cx.stop_propagation();
    }
  }
}

impl EventEmitter<DocumentSearchBarEvent> for DocumentSearchBar {}

#[hotpath::measure_all]
impl Focusable for DocumentSearchBar {
  fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
    self.search_input.focus_handle(cx)
  }
}

#[hotpath::measure_all]
impl Render for DocumentSearchBar {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .w_full()
      .h(px(26.0))
      .border_b_1()
      .border_color(cx.theme().border)
      .bg(cx.theme().background)
      .px_2()
      .flex()
      .items_center()
      .on_key_down(cx.listener(Self::on_key_down))
      .child(
        h_flex()
          .w_full()
          .gap_1()
          .items_center()
          .child(
            Icon::new(IconName::Search)
              .with_size(px(12.0))
              .text_color(cx.theme().muted_foreground),
          )
          .child(
            Input::new(&self.search_input)
              .xsmall()
              .w(px(220.0))
              .cleanable(true),
          )
          .child(div().flex_1())
          .child(
            Button::new("close-document-search")
              .icon(IconName::WindowClose)
              .xsmall()
              .ghost()
              .tooltip("Close search")
              .on_click(cx.listener(|bar, _, _, cx| bar.close(cx))),
          ),
      )
  }
}
