use std::{path::PathBuf, sync::Arc};

use gpui::{
  App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
  SharedString, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, h_flex,
  input::{Input, InputEvent, InputState},
  list::ListItem,
  scroll::ScrollableElement,
  v_flex,
};

use crate::{
  file_search::{DocumentFileSearch, FileSearchHit, default_global_search_root},
  workspace::Workspace,
};

const RESULT_LIMIT: usize = 10;

pub struct FileSearchOverlay {
  workspace: WeakEntity<Workspace>,
  search_input: Entity<InputState>,
  search: Option<Arc<DocumentFileSearch>>,
  hits: Vec<FileSearchHit>,
  selected: usize,
  search_generation: u64,
  loading: bool,
  error: Option<SharedString>,
  _input_subscription: Subscription,
}

impl FileSearchOverlay {
  pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("file_name.db8, file_name.docx, or file_name.fl0"));
    let _input_subscription = cx.subscribe(&search_input, |overlay, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        overlay.refresh_results(cx);
      }
    });

    let mut overlay = Self {
      workspace,
      search_input,
      search: None,
      hits: Vec::new(),
      selected: 0,
      search_generation: 0,
      loading: true,
      error: None,
      _input_subscription,
    };
    overlay.rebuild_index(cx);
    overlay
  }

  pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
    self.search_input.focus_handle(cx).focus(window);
  }

  fn rebuild_index(&mut self, cx: &mut Context<Self>) {
    self.loading = true;
    self.error = None;
    self.search = None;
    self.hits.clear();
    cx.notify();

    cx.spawn(async move |overlay, cx| {
      let search = cx
        .background_executor()
        .spawn(async move { DocumentFileSearch::new(default_global_search_root()) })
        .await;

      let _ = overlay.update(cx, |overlay, cx| {
        overlay.loading = false;
        match search {
          Ok(search) => {
            overlay.search = Some(Arc::new(search));
            overlay.refresh_results(cx);
          },
          Err(error) => {
            overlay.error = Some(format!("File search unavailable: {error}").into());
            overlay.search = None;
            overlay.hits.clear();
            cx.notify();
          },
        }
      });
    })
    .detach();
  }

  fn refresh_results(&mut self, cx: &mut Context<Self>) {
    let query = self.query(cx);
    let Some(search) = self.search.clone() else {
      self.hits.clear();
      self.selected = 0;
      cx.notify();
      return;
    };
    self.search_generation = self.search_generation.wrapping_add(1);
    let generation = self.search_generation;
    cx.spawn(async move |overlay, cx| {
      let hits = cx
        .background_executor()
        .spawn(async move { search.search(&query, RESULT_LIMIT) })
        .await;
      let _ = overlay.update(cx, |overlay, cx| {
        if overlay.search_generation != generation {
          return;
        }
        overlay.hits = hits;
        overlay.selected = 0;
        cx.notify();
      });
    })
    .detach();
  }

  fn query(&self, cx: &App) -> String {
    self.search_input.read(cx).value().to_string()
  }

  fn select_previous(&mut self, cx: &mut Context<Self>) {
    self.selected = self.selected.saturating_sub(1);
    cx.notify();
  }

  fn select_next(&mut self, cx: &mut Context<Self>) {
    if self.selected + 1 < self.hits.len() {
      self.selected += 1;
      cx.notify();
    }
  }

  fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(hit) = self.hits.get(self.selected).cloned() else {
      return;
    };
    self.open_path(hit.path, window, cx);
  }

  fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
    let _ = self.workspace.update(cx, |workspace, cx| {
      workspace.open_document_path(path, window, cx);
      workspace.close_file_search_overlay(cx);
    });
  }

  fn close(&mut self, cx: &mut Context<Self>) {
    let _ = self.workspace.update(cx, |workspace, cx| {
      workspace.close_file_search_overlay(cx);
    });
  }

  fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    match event.keystroke.key.as_str() {
      "escape" => {
        self.close(cx);
        cx.stop_propagation();
      },
      "up" => {
        self.select_previous(cx);
        cx.stop_propagation();
      },
      "down" => {
        self.select_next(cx);
        cx.stop_propagation();
      },
      "enter" => {
        self.open_selected(window, cx);
        cx.stop_propagation();
      },
      _ => {},
    }
  }
}

impl EventEmitter<()> for FileSearchOverlay {}

impl Focusable for FileSearchOverlay {
  fn focus_handle(&self, cx: &App) -> FocusHandle {
    self.search_input.focus_handle(cx)
  }
}

impl Render for FileSearchOverlay {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let query = self.query(cx);
    let has_hits = !self.hits.is_empty();
    let workspace = cx.entity().downgrade();

    div()
      .absolute()
      .top_0()
      .right_0()
      .bottom_0()
      .left_0()
      .bg(cx.theme().background.opacity(0.72))
      .flex()
      .items_center()
      .justify_center()
      .occlude()
      .on_key_down(cx.listener(Self::on_key_down))
      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
      .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
      .child(
        v_flex()
          .w(px(680.0))
          .max_w_full()
          .max_h(px(520.0))
          .overflow_hidden()
          .rounded_lg()
          .border_1()
          .border_color(cx.theme().border)
          .bg(cx.theme().popover)
          .shadow_lg()
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
              .p_2()
              .when(has_hits, |this| {
                self.hits.iter().enumerate().fold(this, |this, (ix, hit)| {
                  let selected = ix == self.selected;
                  let path = hit.path.clone();
                  let file_name = hit
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| hit.path.display().to_string());
                  let parent = hit
                    .path
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default();
                  let workspace = workspace.clone();
                  this.child(
                    ListItem::new(("db8-search-hit", ix))
                      .selected(selected)
                      .rounded(px(6.0))
                      .on_click(move |_, window, cx| {
                        let path = path.clone();
                        let _ = workspace.update(cx, |overlay, cx| {
                          overlay.open_path(path, window, cx);
                        });
                      })
                      .child(
                        v_flex()
                          .gap_1()
                          .py_1()
                          .child(
                            div()
                              .text_sm()
                              .font_weight(gpui::FontWeight::MEDIUM)
                              .child(file_name),
                          )
                          .child(
                            div()
                              .text_xs()
                              .text_color(cx.theme().muted_foreground)
                              .child(parent),
                          ),
                      ),
                  )
                })
              })
              .when(!has_hits, |this| {
                let message = self.error.clone().unwrap_or_else(|| {
                  if self.loading {
                    "Indexing documents..."
                  } else if query.trim().is_empty() {
                    "No .db8, .docx, or .fl0 files indexed"
                  } else {
                    "No matching .db8, .docx, or .fl0 files"
                  }
                  .into()
                });
                this.child(
                  div()
                    .h(px(120.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message),
                )
              }),
          ),
      )
  }
}
