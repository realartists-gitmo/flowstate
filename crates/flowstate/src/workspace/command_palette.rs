//! P4-S5/P5-S3: the omni command palette (decision: omni-first menus). One
//! fuzzy-searchable surface over every registered command and the settings
//! quick-toggles, executing through the workspace's single dispatch path so
//! palette, keybindings, menus, and ribbon chips behave identically.
//! Navigation providers ("go to sheet…", "open tub file…") register as
//! features land.

use gpui::{
  App, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
  SharedString, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, h_flex,
  input::{Input, InputEvent, InputState},
  scroll::ScrollableElement,
  v_flex,
};

use crate::workspace::{PaletteEntry, Workspace};

pub struct CommandPalette {
  workspace: WeakEntity<Workspace>,
  search_input: Entity<InputState>,
  entries: Vec<PaletteEntry>,
  filtered: Vec<usize>,
  selected: usize,
  _input_subscription: Subscription,
}

impl CommandPalette {
  pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Run a command…"));
    let _input_subscription = cx.subscribe(&search_input, |palette: &mut Self, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        palette.refresh_filter(cx);
      }
    });
    let entries = workspace
      .upgrade()
      .map(|workspace| workspace.read(cx).palette_entries())
      .unwrap_or_default();
    let filtered = (0..entries.len()).collect();
    Self {
      workspace,
      search_input,
      entries,
      filtered,
      selected: 0,
      _input_subscription,
    }
  }

  pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.search_input.focus_handle(cx).focus(window);
  }

  fn query(&self, cx: &App) -> String {
    self.search_input.read(cx).value().to_string()
  }

  fn refresh_filter(&mut self, cx: &mut Context<Self>) {
    let query = self.query(cx).to_lowercase();
    let mut scored: Vec<(i64, usize)> = self
      .entries
      .iter()
      .enumerate()
      .filter_map(|(ix, entry)| fuzzy_score(&entry.label.to_lowercase(), &query).map(|score| (score, ix)))
      .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    self.filtered = scored.into_iter().map(|(_, ix)| ix).collect();
    self.selected = 0;
    cx.notify();
  }

  fn close(&mut self, cx: &mut Context<Self>) {
    let _ = self.workspace.update(cx, |workspace, cx| workspace.close_command_palette(cx));
  }

  fn run_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(entry_ix) = self.filtered.get(self.selected).copied() else {
      return;
    };
    let action = self.entries[entry_ix].action;
    let workspace = self.workspace.clone();
    let _ = workspace.update(cx, |workspace, cx| {
      workspace.close_command_palette(cx);
      workspace.execute_palette_action(action, window, cx);
    });
  }

  fn run_index(&mut self, list_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    self.selected = list_ix;
    self.run_selected(window, cx);
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    match event.keystroke.key.as_str() {
      "escape" => {
        self.close(cx);
        cx.stop_propagation();
      },
      "up" => {
        if !self.filtered.is_empty() {
          self.selected = self.selected.checked_sub(1).unwrap_or(self.filtered.len() - 1);
          cx.notify();
        }
        cx.stop_propagation();
      },
      "down" => {
        if !self.filtered.is_empty() {
          self.selected = (self.selected + 1) % self.filtered.len();
          cx.notify();
        }
        cx.stop_propagation();
      },
      "enter" => {
        self.run_selected(window, cx);
        cx.stop_propagation();
      },
      _ => {},
    }
  }
}

/// Case-folded subsequence match with contiguity + word-start bonuses; empty
/// queries match everything at registry order.
fn fuzzy_score(label: &str, query: &str) -> Option<i64> {
  if query.is_empty() {
    return Some(0);
  }
  let label_bytes: Vec<char> = label.chars().collect();
  let mut score: i64 = 0;
  let mut label_ix = 0;
  let mut previous_hit: Option<usize> = None;
  for needle in query.chars() {
    let mut found = None;
    while label_ix < label_bytes.len() {
      if label_bytes[label_ix] == needle {
        found = Some(label_ix);
        break;
      }
      label_ix += 1;
    }
    let hit = found?;
    score += 10;
    if previous_hit == Some(hit.wrapping_sub(1)) {
      score += 8;
    }
    if hit == 0 || label_bytes.get(hit.wrapping_sub(1)).is_some_and(|ch| !ch.is_alphanumeric()) {
      score += 6;
    }
    previous_hit = Some(hit);
    label_ix = hit + 1;
  }
  // Shorter labels win ties: exactness beats verbosity.
  Some(score - label_bytes.len() as i64 / 8)
}

impl Focusable for CommandPalette {
  fn focus_handle(&self, cx: &App) -> FocusHandle {
    self.search_input.focus_handle(cx)
  }
}

impl Render for CommandPalette {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let has_hits = !self.filtered.is_empty();
    div()
      .absolute()
      .top_0()
      .right_0()
      .bottom_0()
      .left_0()
      .bg(cx.theme().background.opacity(0.72))
      .flex()
      .items_start()
      .justify_center()
      .pt(px(96.0))
      .occlude()
      .on_key_down(cx.listener(Self::on_key_down))
      .on_mouse_down(
        MouseButton::Left,
        cx.listener(|palette, _, _, cx| {
          palette.close(cx);
          cx.stop_propagation();
        }),
      )
      .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
      .child(
        v_flex()
          .w(px(560.0))
          .max_w_full()
          .max_h(px(440.0))
          .overflow_hidden()
          .rounded_lg()
          .border_1()
          .border_color(cx.theme().border)
          .bg(cx.theme().popover)
          .shadow_lg()
          .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
          .child(
            h_flex()
              .gap_2()
              .items_center()
              .p_3()
              .border_b_1()
              .border_color(cx.theme().border)
              .child(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground))
              .child(Input::new(&self.search_input).w_full().cleanable(true)),
          )
          .child(
            v_flex()
              .flex_1()
              .overflow_y_scrollbar()
              .p_1()
              .when(has_hits, |this| {
                self
                  .filtered
                  .iter()
                  .take(64)
                  .enumerate()
                  .fold(this, |this, (list_ix, entry_ix)| {
                    let entry = &self.entries[*entry_ix];
                    let selected = list_ix == self.selected;
                    let label: SharedString = entry.label.clone().into();
                    let shortcut: Option<SharedString> = entry.shortcut.clone().map(Into::into);
                    this.child(
                      h_flex()
                        .id(("palette-entry", list_ix))
                        .items_center()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .when(selected, |this| this.bg(cx.theme().list_active))
                        .hover(|this| this.bg(cx.theme().list_hover))
                        .on_mouse_down(
                          MouseButton::Left,
                          cx.listener(move |palette, _, window, cx| {
                            palette.run_index(list_ix, window, cx);
                            cx.stop_propagation();
                          }),
                        )
                        .child(div().text_sm().child(label))
                        .when_some(shortcut, |this, shortcut| {
                          this.child(div().text_xs().text_color(cx.theme().muted_foreground).child(shortcut))
                        }),
                    )
                  })
              })
              .when(!has_hits, |this| {
                this.child(
                  div()
                    .p_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No matching commands"),
                )
              }),
          ),
      )
  }
}
