use gpui::{Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render, Window, div, prelude::*, px};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, h_flex,
  input::{Input, InputState},
};

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
}

impl EventEmitter<()> for DocumentSearchBar {}

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
      .h(px(34.0))
      .border_b_1()
      .border_color(cx.theme().border)
      .bg(cx.theme().background)
      .px_2()
      .flex()
      .items_center()
      .child(
        h_flex()
          .w_full()
          .gap_2()
          .items_center()
          .child(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground))
          .child(Input::new(&self.search_input).w(px(280.0)).cleanable(true)),
      )
  }
}
