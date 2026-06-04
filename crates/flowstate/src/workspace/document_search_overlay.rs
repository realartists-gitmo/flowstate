use gpui::{
  Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render, Subscription, Window, div,
  prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, Sizable as _,
  button::{Button, ButtonVariants as _},
  checkbox::Checkbox,
  h_flex,
  input::{Input, InputEvent, InputState},
};

#[derive(Clone, Copy, Debug)]
pub enum DocumentSearchBarEvent {
  QueryChanged,
  CaseSensitivityChanged,
  WholeWordsChanged,
  PreviousRequested,
  NextRequested,
  ApplyReplaceRequested,
  CloseRequested,
}

pub struct DocumentSearchBar {
  search_input: Entity<InputState>,
  replace_input: Entity<InputState>,
  active_match: Option<usize>,
  match_count: usize,
  case_sensitive: bool,
  whole_words: bool,
  _input_subscription: Subscription,
}

#[hotpath::measure_all]
impl DocumentSearchBar {
  pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
    let replace_input = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
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
      replace_input,
      active_match: None,
      match_count: 0,
      case_sensitive: false,
      whole_words: false,
      _input_subscription,
    }
  }

  pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.search_input.focus_handle(cx).focus(window);
  }

  pub fn query(&self, cx: &gpui::App) -> String {
    self.search_input.read(cx).value().to_string()
  }

  pub fn replacement(&self, cx: &gpui::App) -> String {
    self.replace_input.read(cx).value().to_string()
  }

  pub fn input_focused(&self, window: &Window, cx: &gpui::App) -> bool {
    self
      .search_input
      .read(cx)
      .focus_handle(cx)
      .is_focused(window)
      || self
        .replace_input
        .read(cx)
        .focus_handle(cx)
        .is_focused(window)
  }

  pub fn case_sensitive(&self) -> bool {
    self.case_sensitive
  }

  pub fn whole_words(&self) -> bool {
    self.whole_words
  }

  fn set_case_sensitive(&mut self, case_sensitive: bool, cx: &mut Context<Self>) {
    if self.case_sensitive == case_sensitive {
      return;
    }
    self.case_sensitive = case_sensitive;
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::CaseSensitivityChanged);
    cx.notify();
  }

  fn set_whole_words(&mut self, whole_words: bool, cx: &mut Context<Self>) {
    if self.whole_words == whole_words {
      return;
    }
    self.whole_words = whole_words;
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::WholeWordsChanged);
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

  fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    match event.keystroke.key.as_str() {
      "escape" => {
        self.close(cx);
        cx.stop_propagation();
      },
      "up" => {
        cx.emit(DocumentSearchBarEvent::PreviousRequested);
        cx.stop_propagation();
      },
      "down" => {
        cx.emit(DocumentSearchBarEvent::NextRequested);
        cx.stop_propagation();
      },
      "enter" => {
        if self
          .replace_input
          .read(cx)
          .focus_handle(cx)
          .is_focused(window)
        {
          cx.emit(DocumentSearchBarEvent::ApplyReplaceRequested);
        } else {
          cx.emit(DocumentSearchBarEvent::NextRequested);
        }
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

    let search_bar = cx.entity().downgrade();

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
          .gap_2()
          .items_center()
          .child(
            Icon::new(IconName::Search)
              .with_size(px(12.0))
              .text_color(cx.theme().muted_foreground),
          )
          .child(
            Input::new(&self.search_input)
              .xsmall()
              .w(px(190.0))
              .cleanable(true),
          )
          .child(
            div()
              .ml_1()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(count_label),
          )
          .child(
            h_flex()
              .ml_1()
              .gap_0()
              .items_center()
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
              ),
          )
          .child(
            h_flex()
              .ml_3()
              .gap_1()
              .items_center()
              .child(
                Icon::default()
                  .path("icons/letter-case.svg")
                  .xsmall()
                  .text_color(cx.theme().muted_foreground),
              )
              .child(
                Checkbox::new("document-search-case-sensitive")
                  .checked(self.case_sensitive)
                  .xsmall()
                  .tab_stop(false)
                  .on_click({
                    let search_bar = search_bar.clone();
                    move |checked, _, cx| {
                      let _ = search_bar.update(cx, |bar, cx| bar.set_case_sensitive(*checked, cx));
                    }
                  }),
              ),
          )
          .child(
            h_flex()
              .ml_3()
              .gap_1()
              .items_center()
              .child(
                Icon::default()
                  .path("icons/text-box-edit.svg")
                  .xsmall()
                  .text_color(cx.theme().muted_foreground),
              )
              .child(
                Checkbox::new("document-search-whole-words")
                  .checked(self.whole_words)
                  .xsmall()
                  .tab_stop(false)
                  .on_click(move |checked, _, cx| {
                    let _ = search_bar.update(cx, |bar, cx| bar.set_whole_words(*checked, cx));
                  }),
              ),
          )
          .child(
            div().ml_4().child(
              Input::new(&self.replace_input)
                .xsmall()
                .w(px(190.0))
                .cleanable(true),
            ),
          )
          .child(
            Button::new("apply-document-search-replace")
              .icon(
                Icon::default()
                  .path("icons/replace.svg")
                  .text_color(cx.theme().muted_foreground),
              )
              .xsmall()
              .ghost()
              .tooltip("Replace all matches")
              .on_click(cx.listener(|_, _, _, cx| cx.emit(DocumentSearchBarEvent::ApplyReplaceRequested))),
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
