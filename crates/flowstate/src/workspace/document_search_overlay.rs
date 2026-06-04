use gpui::{
  Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render, Subscription, Window, div,
  prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
  button::{Button, ButtonVariants as _},
  h_flex,
  input::{Input, InputEvent, InputState},
};

#[derive(Clone, Copy, Debug)]
pub enum DocumentSearchBarEvent {
  QueryChanged,
  CaseSensitivityChanged,
  PreviousRequested,
  NextRequested,
  CloseRequested,
}

pub struct DocumentSearchBar {
  search_input: Entity<InputState>,
  active_match: Option<usize>,
  match_count: usize,
  case_sensitive: bool,
  _input_subscription: Subscription,
}

#[hotpath::measure_all]
impl DocumentSearchBar {
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
    let _input_subscription = cx.subscribe(&search_input, |bar, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        bar.active_match = None;
        bar.match_count = 0;
        cx.emit(DocumentSearchBarEvent::QueryChanged);
        cx.notify();
      }
    });
    Self {
      search_input,
      active_match: None,
      match_count: 0,
      case_sensitive: false,
      _input_subscription,
    }
  }

  pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.search_input.focus_handle(cx).focus(window);
  }

  pub fn query(&self, cx: &gpui::App) -> String {
    self.search_input.read(cx).value().to_string()
  }

  pub fn case_sensitive(&self) -> bool {
    self.case_sensitive
  }

  fn toggle_case_sensitive(&mut self, cx: &mut Context<Self>) {
    self.case_sensitive = !self.case_sensitive;
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::CaseSensitivityChanged);
    cx.notify();
  }

  pub fn set_match_position(&mut self, active_match: Option<usize>, match_count: usize, cx: &mut Context<Self>) {
    self.active_match = active_match;
    self.match_count = match_count;
    cx.notify();
  }

  fn close(&self, cx: &mut Context<Self>) {
    cx.emit(DocumentSearchBarEvent::CloseRequested);
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
    match event.keystroke.key.as_str() {
      "escape" => {
        self.close(cx);
        cx.stop_propagation();
      },
      "up" => {
        cx.emit(DocumentSearchBarEvent::PreviousRequested);
        cx.stop_propagation();
      },
      "down" | "enter" => {
        cx.emit(DocumentSearchBarEvent::NextRequested);
        cx.stop_propagation();
      },
      _ => {},
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
    let count_label = match (self.active_match, self.match_count) {
      (_, 0) => "0 of 0".to_string(),
      (Some(active), count) => format!("{} of {}", active + 1, count),
      (None, count) => format!("0 of {count}"),
    };

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
          .child(
            Button::new("document-search-case-sensitive")
              .child("Aa")
              .xsmall()
              .ghost()
              .selected(self.case_sensitive)
              .tooltip("Match case")
              .on_click(cx.listener(|bar, _, _, cx| bar.toggle_case_sensitive(cx))),
          )
          .child(
            div()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(count_label),
          )
          .child(
            Button::new("previous-document-search-match")
              .icon(IconName::ChevronUp)
              .xsmall()
              .ghost()
              .tooltip("Previous match")
              .on_click(cx.listener(|_, _, _, cx| cx.emit(DocumentSearchBarEvent::PreviousRequested))),
          )
          .child(
            Button::new("next-document-search-match")
              .icon(IconName::ChevronDown)
              .xsmall()
              .ghost()
              .tooltip("Next match")
              .on_click(cx.listener(|_, _, _, cx| cx.emit(DocumentSearchBarEvent::NextRequested))),
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
