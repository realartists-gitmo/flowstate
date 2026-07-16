use gpui::{
  Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, ParentElement, Render, Subscription, Window, div,
  prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Disableable as _, Icon, IconName, Side, Sizable as _,
  button::{Button, ButtonVariants as _},
  checkbox::Checkbox,
  h_flex,
  input::{Input, InputEvent, InputState},
  menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
};

use std::ops::Range;

use crate::rich_text_element::{DocumentOffset, HighlightStyle, Paragraph, ParagraphStyle, RunSemanticStyle};

#[derive(Clone, Copy)]
struct SearchStyleFilter {
  label: &'static str,
  style: ParagraphStyle,
}

const SEARCH_STYLE_FILTERS: &[SearchStyleFilter] = &[
  SearchStyleFilter {
    label: "Normal",
    style: ParagraphStyle::Normal,
  },
  SearchStyleFilter {
    label: "Pocket",
    style: flowstate_document::PARAGRAPH_POCKET,
  },
  SearchStyleFilter {
    label: "Hat",
    style: flowstate_document::PARAGRAPH_HAT,
  },
  SearchStyleFilter {
    label: "Block",
    style: flowstate_document::PARAGRAPH_BLOCK,
  },
  SearchStyleFilter {
    label: "Tag",
    style: flowstate_document::PARAGRAPH_TAG,
  },
  SearchStyleFilter {
    label: "Analytic",
    style: flowstate_document::PARAGRAPH_ANALYTIC,
  },
  SearchStyleFilter {
    label: "Undertag",
    style: flowstate_document::PARAGRAPH_UNDERTAG,
  },
];

#[derive(Clone, Copy)]
struct HighlightFilter {
  label: &'static str,
  style: HighlightStyle,
}

const HIGHLIGHT_FILTERS: &[HighlightFilter] = &[
  HighlightFilter {
    label: "Spoken",
    style: flowstate_document::HIGHLIGHT_SPOKEN,
  },
  HighlightFilter {
    label: "Insert",
    style: flowstate_document::HIGHLIGHT_INSERT,
  },
  HighlightFilter {
    label: "Alternative",
    style: flowstate_document::HIGHLIGHT_ALTERNATIVE,
  },
  HighlightFilter {
    label: "Marked",
    style: flowstate_document::HIGHLIGHT_MARKED,
  },
];

#[derive(Clone, Copy)]
struct SemanticStyleFilter {
  label: &'static str,
  style: RunSemanticStyle,
}

const SEMANTIC_STYLE_FILTERS: &[SemanticStyleFilter] = &[
  SemanticStyleFilter {
    label: "Plain",
    style: RunSemanticStyle::Plain,
  },
  SemanticStyleFilter {
    label: "Cite",
    style: flowstate_document::SEMANTIC_CITE,
  },
  SemanticStyleFilter {
    label: "Emphasis",
    style: flowstate_document::SEMANTIC_EMPHASIS,
  },
  SemanticStyleFilter {
    label: "Underline",
    style: flowstate_document::SEMANTIC_UNDERLINE,
  },
  SemanticStyleFilter {
    label: "Condensed",
    style: flowstate_document::SEMANTIC_CONDENSED,
  },
  SemanticStyleFilter {
    label: "Ultracondensed",
    style: flowstate_document::SEMANTIC_ULTRACONDENSED,
  },
];

#[derive(Clone, Copy, Debug)]
pub enum DocumentSearchBarEvent {
  QueryChanged,
  CaseSensitivityChanged,
  WholeWordsChanged,
  RegexModeChanged,
  StyleFilterChanged,
  PreviousRequested,
  NextRequested,
  ApplyReplaceCurrentRequested,
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
  /// R12-B: interpret the query as a regular expression. Whole-words is
  /// inert in this mode (regex users spell `\b`); replace expands `$1`.
  use_regex: bool,
  /// The pattern diagnostic when the regex fails to compile — rendered in
  /// the bar in place of the match count (silent refusal is a defect).
  regex_error: Option<String>,
  enabled_styles: [bool; SEARCH_STYLE_FILTERS.len()],
  highlight_types: [bool; HIGHLIGHT_FILTERS.len()],
  underline: bool,
  strikethrough: bool,
  semantic_styles: [bool; SEMANTIC_STYLE_FILTERS.len()],
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
      use_regex: false,
      regex_error: None,
      enabled_styles: [true; SEARCH_STYLE_FILTERS.len()],
      highlight_types: [true; HIGHLIGHT_FILTERS.len()],
      underline: true,
      strikethrough: true,
      semantic_styles: [true; SEMANTIC_STYLE_FILTERS.len()],
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

  pub fn use_regex(&self) -> bool {
    self.use_regex
  }

  /// Panel-fed pattern diagnostic (None = the pattern compiles). No event —
  /// the panel calls this from its own refresh.
  pub fn set_regex_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
    if self.regex_error == error {
      return;
    }
    self.regex_error = error;
    cx.notify();
  }

  pub fn paragraph_style_enabled(&self, style: ParagraphStyle) -> bool {
    SEARCH_STYLE_FILTERS
      .iter()
      .position(|filter| filter.style == style)
      .is_none_or(|ix| self.enabled_styles[ix])
  }

  pub fn has_active_run_filters(&self) -> bool {
    self.highlight_types.iter().any(|&b| b) || self.underline || self.strikethrough || self.semantic_styles.iter().any(|&b| b)
  }

  pub fn run_style_matches_for_range(&self, paragraph: &Paragraph, range: &Range<DocumentOffset>) -> bool {
    let mut run_start = 0usize;
    for run in &paragraph.runs {
      let run_end = run_start + run.len;
      if run_start < range.end.byte && run_end > range.start.byte {
        if self.highlight_types.iter().any(|&b| b)
          && let Some(highlight) = run.styles.highlight
          && self.highlight_type_enabled(highlight)
        {
          return true;
        }
        if self.underline && run.styles.direct_underline {
          return true;
        }
        if self.strikethrough && run.styles.strikethrough {
          return true;
        }
        if self.semantic_styles.iter().any(|&b| b) && self.semantic_style_enabled(run.styles.semantic) {
          return true;
        }
      }
      run_start = run_end;
    }
    false
  }

  fn semantic_style_enabled(&self, style: RunSemanticStyle) -> bool {
    SEMANTIC_STYLE_FILTERS
      .iter()
      .position(|filter| filter.style == style)
      .is_some_and(|ix| self.semantic_styles[ix])
  }

  fn highlight_type_enabled(&self, style: HighlightStyle) -> bool {
    HIGHLIGHT_FILTERS
      .iter()
      .position(|filter| filter.style == style)
      .is_some_and(|ix| self.highlight_types[ix])
  }

  fn toggle_highlight_type(&mut self, ix: usize, cx: &mut Context<Self>) {
    self.highlight_types[ix] = !self.highlight_types[ix];
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::StyleFilterChanged);
    cx.notify();
  }

  fn toggle_underline(&mut self, cx: &mut Context<Self>) {
    self.underline = !self.underline;
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::StyleFilterChanged);
    cx.notify();
  }

  fn toggle_strikethrough(&mut self, cx: &mut Context<Self>) {
    self.strikethrough = !self.strikethrough;
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::StyleFilterChanged);
    cx.notify();
  }

  fn toggle_semantic_style(&mut self, ix: usize, cx: &mut Context<Self>) {
    self.semantic_styles[ix] = !self.semantic_styles[ix];
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::StyleFilterChanged);
    cx.notify();
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

  fn set_use_regex(&mut self, use_regex: bool, cx: &mut Context<Self>) {
    if self.use_regex == use_regex {
      return;
    }
    self.use_regex = use_regex;
    self.regex_error = None;
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::RegexModeChanged);
    cx.notify();
  }

  fn toggle_style_filter(&mut self, style: ParagraphStyle, cx: &mut Context<Self>) {
    let Some(ix) = SEARCH_STYLE_FILTERS
      .iter()
      .position(|filter| filter.style == style)
    else {
      return;
    };
    self.enabled_styles[ix] = !self.enabled_styles[ix];
    self.active_match = None;
    self.match_count = 0;
    cx.emit(DocumentSearchBarEvent::StyleFilterChanged);
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
            Icon::new(IconName::TextSearch)
              .with_size(px(12.0))
              .text_color(cx.theme().muted_foreground),
          )
          .child(
            Input::new(&self.search_input)
              .xsmall()
              .w(px(190.0))
              .cleanable(true),
          )
          .child(match self.regex_error.clone() {
            // R12-B: the pattern diagnostic takes the count slot — a broken
            // regex must never read as "0 matches".
            Some(error) => div()
              .ml_1()
              .text_xs()
              .text_color(cx.theme().danger)
              .max_w(px(240.0))
              .truncate()
              .child(error),
            None => div()
              .ml_1()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(count_label),
          })
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
                Button::new("document-search-case-sensitive-icon")
                  .child(
                    Icon::default()
                      .path("icons/letter-case.svg")
                      .xsmall()
                      .text_color(cx.theme().muted_foreground),
                  )
                  .xsmall()
                  .ghost()
                  .tooltip("Case sensitive")
                  .on_click({
                    let search_bar = search_bar.clone();
                    move |_, _, cx| {
                      let _ = search_bar.update(cx, |bar, cx| bar.set_case_sensitive(!bar.case_sensitive, cx));
                    }
                  }),
              )
              .child(
                Checkbox::new("document-search-case-sensitive")
                  .checked(self.case_sensitive)
                  .xsmall()
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
                Button::new("document-search-whole-words-icon")
                  .child(
                    Icon::default()
                      .path("icons/text-box-edit.svg")
                      .xsmall()
                      .text_color(cx.theme().muted_foreground),
                  )
                  .xsmall()
                  .ghost()
                  .disabled(self.use_regex)
                  .tooltip(if self.use_regex {
                    "Match whole word (off in regex mode — use \\b)"
                  } else {
                    "Match whole word"
                  })
                  .on_click({
                    let search_bar = search_bar.clone();
                    move |_, _, cx| {
                      let _ = search_bar.update(cx, |bar, cx| bar.set_whole_words(!bar.whole_words, cx));
                    }
                  }),
              )
              .child(
                Checkbox::new("document-search-whole-words")
                  .checked(self.whole_words)
                  .xsmall()
                  .disabled(self.use_regex)
                  .on_click({
                    let search_bar = search_bar.clone();
                    move |checked, _, cx| {
                      let _ = search_bar.update(cx, |bar, cx| bar.set_whole_words(*checked, cx));
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
                Button::new("document-search-regex-icon")
                  .child(
                    Icon::default()
                      .path("icons/regex.svg")
                      .xsmall()
                      .text_color(cx.theme().muted_foreground),
                  )
                  .xsmall()
                  .ghost()
                  .tooltip("Regular expression (Replace expands $1, ${name})")
                  .on_click({
                    let search_bar = search_bar.clone();
                    move |_, _, cx| {
                      let _ = search_bar.update(cx, |bar, cx| bar.set_use_regex(!bar.use_regex, cx));
                    }
                  }),
              )
              .child(
                Checkbox::new("document-search-regex")
                  .checked(self.use_regex)
                  .xsmall()
                  .on_click({
                    let search_bar = search_bar.clone();
                    move |checked, _, cx| {
                      let _ = search_bar.update(cx, |bar, cx| bar.set_use_regex(*checked, cx));
                    }
                  }),
              ),
          )
          .child(
            div().ml_3().child(
              Button::new("document-search-style-filter")
                .child(
                  h_flex()
                    .gap_1()
                    .items_center()
                    .child("Styles")
                    .child(Icon::new(IconName::ChevronDown).xsmall()),
                )
                .xsmall()
                .ghost()
                .tooltip("Paragraph styles to search")
                .dropdown_menu({
                  let search_bar = search_bar.clone();
                  move |menu, window, cx| {
                    let sb = search_bar.upgrade().unwrap();
                    let enabled_styles = sb.read(cx).enabled_styles;
                    let highlight_types = sb.read(cx).highlight_types;
                    let underline = sb.read(cx).underline;
                    let strikethrough = sb.read(cx).strikethrough;
                    let semantic_styles = sb.read(cx).semantic_styles;

                    let mut menu = SEARCH_STYLE_FILTERS
                      .iter()
                      .enumerate()
                      .fold(menu.min_w(px(140.0)), |menu, (ix, filter)| {
                        let search_bar = search_bar.clone();
                        menu.item(
                          PopupMenuItem::new(filter.label)
                            .checked(enabled_styles[ix])
                            .keep_open(true)
                            .on_click(move |_, _, cx| {
                              let _ = search_bar.update(cx, |bar, cx| bar.toggle_style_filter(filter.style, cx));
                            }),
                        )
                      })
                      .separator();
                    for (ix, filter) in SEMANTIC_STYLE_FILTERS.iter().enumerate() {
                      let search_bar = search_bar.clone();
                      menu = menu.item(
                        PopupMenuItem::new(filter.label)
                          .checked(semantic_styles[ix])
                          .keep_open(true)
                          .on_click(move |_, _, cx| {
                            let _ = search_bar.update(cx, |bar, cx| bar.toggle_semantic_style(ix, cx));
                          }),
                      );
                    }
                    menu = menu.separator();

                    let any_highlight_checked = highlight_types.iter().any(|&b| b);
                    let highlight_submenu = PopupMenu::build(window, cx, |submenu, _window, _cx| {
                      let mut submenu = submenu.min_w(px(130.0)).check_side(Side::Right);
                      for (ix, filter) in HIGHLIGHT_FILTERS.iter().enumerate() {
                        let search_bar = search_bar.clone();
                        submenu = submenu.item(
                          PopupMenuItem::new(filter.label)
                            .checked(highlight_types[ix])
                            .keep_open(true)
                            .on_click(move |_, _, cx| {
                              let _ = search_bar.update(cx, |bar, cx| bar.toggle_highlight_type(ix, cx));
                            }),
                        );
                      }
                      submenu
                    });
                    let parent = cx.entity().downgrade();
                    highlight_submenu.update(cx, |sub, _| {
                      sub.parent_menu = Some(parent);
                    });

                    menu = menu
                      .item(PopupMenuItem::submenu("Highlight", highlight_submenu).checked(any_highlight_checked))
                      .separator()
                      .item({
                        let search_bar = search_bar.clone();
                        PopupMenuItem::new("Underline")
                          .checked(underline)
                          .keep_open(true)
                          .on_click(move |_, _, cx| {
                            let _ = search_bar.update(cx, |bar, cx| bar.toggle_underline(cx));
                          })
                      })
                      .item({
                        let search_bar = search_bar.clone();
                        PopupMenuItem::new("Strikethrough")
                          .checked(strikethrough)
                          .keep_open(true)
                          .on_click(move |_, _, cx| {
                            let _ = search_bar.update(cx, |bar, cx| bar.toggle_strikethrough(cx));
                          })
                      });

                    menu
                  }
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
            h_flex()
              .gap_0()
              .items_center()
              .child(
                Button::new("apply-document-search-replace-current")
                  .icon(
                    Icon::default()
                      .path("icons/replace.svg")
                      .text_color(cx.theme().muted_foreground),
                  )
                  .xsmall()
                  .ghost()
                  .tooltip("Replace current match")
                  .on_click(cx.listener(|_, _, _, cx| cx.emit(DocumentSearchBarEvent::ApplyReplaceCurrentRequested))),
              )
              .child(
                Button::new("apply-document-search-replace-all")
                  .icon(
                    Icon::default()
                      .path("icons/replace-all.svg")
                      .text_color(cx.theme().muted_foreground),
                  )
                  .xsmall()
                  .ghost()
                  .tooltip("Replace all matches")
                  .on_click(cx.listener(|_, _, _, cx| cx.emit(DocumentSearchBarEvent::ApplyReplaceRequested))),
              ),
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
