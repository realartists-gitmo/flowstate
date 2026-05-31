use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, rc::Rc, sync::Arc, time::Duration};

use gpui::{App, Context, IntoElement, PathPromptOptions, Pixels, SharedString, Timer, Window, div, prelude::*, px, rgb, size};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, Selectable, Sizable, h_flex, v_virtual_list,
  button::{Button, ButtonVariants},
  input::Input,
  resizable::{h_resizable, resizable_panel},
  scroll::ScrollableElement,
  tree::{TreeItem, tree},
  v_flex,
};

use crate::{app_settings::{flowstate_data_dir, save_tub_root}, rich_text_element::ToolkitTextDrag};

use super::{
  APP_CHROME_BORDER_WIDTH, LeftNavMode, OutlineRowGuides, SIDE_PANEL_COLLAPSED_WIDTH, SidebarTreeAction, SidebarTreeRow, ToolkitSearchFilter,
  Workspace, outline_hierarchy_color, render_sidebar_tree_row,
};

const TOOLKIT_RESULT_LIMIT: usize = 32;

struct TubLoadResult {
  root: PathBuf,
  index: Arc<flowstate_tub::TubIndex>,
  files: Vec<flowstate_tub::TubFile>,
  tree_entries: Vec<flowstate_tub::TubTreeNode>,
}

#[hotpath::measure_all]
impl Workspace {
  /// Renders the main editor area and the right-side Toolkit panel as one
  /// resizable horizontal split. The expanded panel is a live evidence browser:
  /// search results are miniature scrollable windows that can be opened,
  /// inserted, or dragged into the editor.
  pub(super) fn render_content_area(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
    let toolkit_width = if self.toolkit_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH
    } else {
      px(380.0)
    };
    let toolkit_range_end = if self.toolkit_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH
    } else {
      px(620.0)
    };

    h_resizable("workspace-content-resizable")
      .with_state(&self.content_resizable_state)
      .child(
        resizable_panel()
          .size(px(560.0))
          .size_range(px(120.0)..Pixels::MAX)
          .child(
            div()
              .size_full()
              .min_w_0()
              .overflow_hidden()
              .child(self.render_document_pane(cx)),
          ),
      )
      .child(
        resizable_panel()
          .size(toolkit_width)
          .size_range(toolkit_width..toolkit_range_end)
          .grow(false)
          .child(if self.toolkit_collapsed {
            self
              .render_collapsed_side_panel("Show toolkit", IconName::PanelRightOpen, |workspace, cx| workspace.toggle_toolkit(cx), cx)
              .into_any_element()
          } else {
            self.render_toolkit_expanded(cx).into_any_element()
          }),
      )
  }

  pub(super) fn load_tub_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
    self.tub_root = Some(root.clone());
    self.tub_index = None;
    self.tub_files.clear();
    self.tub_tree_entries.clear();
    self.tub_watcher = None;
    self.tub_scan_pending = false;
    self
      .tub_tree
      .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
    self.tub_expanded_dirs.insert(root.clone());
    self.tub_status = format!("Indexing {}", root.display()).into();
    self.toolkit_status = "Indexing tub...".into();
    cx.notify();
    self.spawn_tub_scan(root, false, cx);
  }

  pub fn active_tub_index_for_search(&self) -> Option<Arc<flowstate_tub::TubIndex>> {
    self.tub_index.clone()
  }

  pub(super) fn prompt_select_tub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
      files: false,
      directories: true,
      multiple: false,
      prompt: Some("Select debate tub folder".into()),
    });
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let Ok(Ok(Some(paths))) = paths.await else {
        return;
      };
      let Some(path) = paths.into_iter().next() else {
        return;
      };
      let _ = window_handle.update(cx, |_, _, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.set_tub_root(path, cx);
        });
      });
    })
    .detach();
  }

  fn set_tub_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
    let _ = save_tub_root(Some(root.clone()));
    self.tub_root = Some(root.clone());
    self.tub_index = None;
    self.tub_files.clear();
    self.tub_tree_entries.clear();
    self.tub_watcher = None;
    self.tub_scan_pending = false;
    self
      .tub_tree
      .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
    self.tub_expanded_dirs.clear();
    self.tub_expanded_dirs.insert(root.clone());
    self.spawn_tub_scan(root, true, cx);
  }

  fn refresh_tub(&mut self, cx: &mut Context<Self>) {
    let Some(root) = self.tub_root.clone() else {
      self.tub_status = "No tub selected".into();
      cx.notify();
      return;
    };
    self.spawn_tub_scan(root, true, cx);
  }

  fn spawn_tub_scan(&mut self, root: PathBuf, persist_root: bool, cx: &mut Context<Self>) {
    if self.tub_scan_in_flight {
      self.tub_scan_pending = true;
      self.tub_status = "Indexing queued".into();
      cx.notify();
      return;
    }
    let data_dir = flowstate_data_dir().join("tub");
    let expanded_dirs = self.tub_expanded_dirs.clone();
    let requested_root = root.clone();
    self.tub_scan_in_flight = true;
    self.tub_status = format!("Indexing {}", root.display()).into();
    self.toolkit_status = "Indexing tub...".into();
    self.toolkit_hits.clear();
    cx.notify();

    cx.spawn(async move |workspace, cx| {
      let result = cx
        .background_executor()
        .spawn(async move {
          let index = Arc::new(flowstate_tub::TubIndex::open(&root, &data_dir)?);
          let files = index.scan_and_index()?;
          let tree_entries = index.tree_entries(&expanded_dirs)?;
          anyhow::Ok(TubLoadResult {
            root,
            index,
            files,
            tree_entries,
          })
        })
        .await;

      let _ = workspace.update(cx, |workspace, cx| {
        workspace.tub_scan_in_flight = false;
        let rescan_pending = std::mem::take(&mut workspace.tub_scan_pending);
        if workspace
          .tub_root
          .as_ref()
          .is_some_and(|current_root| current_root != &requested_root)
        {
          if let Some(root) = workspace.tub_root.clone() {
            workspace.spawn_tub_scan(root, true, cx);
          }
          cx.notify();
          return;
        }
        match result {
          Ok(result) => {
            if persist_root {
              let _ = save_tub_root(Some(result.root.clone()));
            }
            let file_count = result.files.len();
            let tree_items = build_tub_tree_items(&result.root, &result.files, &workspace.tub_expanded_dirs);
            let tree_entries = result
              .index
              .tree_entries_for_files(&result.files, &workspace.tub_expanded_dirs)
              .unwrap_or(result.tree_entries);
            workspace.tub_root = Some(result.root);
            workspace.tub_watcher = result.index.start_watcher().ok();
            workspace.tub_index = Some(result.index);
            workspace.tub_files = result.files;
            workspace.tub_tree_entries = tree_entries;
            workspace
              .tub_tree
              .update(cx, |tree, cx| tree.set_items(tree_items, cx));
            workspace.tub_status = format!("{file_count} tub files indexed").into();
            workspace.toolkit_status = if workspace.toolkit_search_input.read(cx).value().trim().is_empty() {
              "Search DB8 blocks, tags, and analytics.".into()
            } else {
              "Ready".into()
            };
            workspace.refresh_toolkit_search(cx);
            workspace.start_tub_watch_poll(cx);
            if rescan_pending
              && let Some(root) = workspace.tub_root.clone()
            {
              workspace.spawn_tub_scan(root, true, cx);
            }
          },
          Err(error) => {
            workspace.tub_index = None;
            workspace.tub_watcher = None;
            workspace.tub_files.clear();
            workspace.tub_tree_entries.clear();
            workspace
              .tub_tree
              .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
            workspace.toolkit_hits.clear();
            workspace.tub_status = format!("Tub unavailable: {error}").into();
            workspace.toolkit_status = "Tub unavailable".into();
            if rescan_pending
              && let Some(root) = workspace.tub_root.clone()
            {
              workspace.spawn_tub_scan(root, true, cx);
            }
          },
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn start_tub_watch_poll(&mut self, cx: &mut Context<Self>) {
    if self.tub_watch_polling || self.tub_watcher.is_none() {
      return;
    }
    self.tub_watch_polling = true;
    cx.spawn(async move |workspace, cx| {
      loop {
        Timer::after(Duration::from_millis(900)).await;
        let keep_polling = workspace
          .update(cx, |workspace, cx| {
            let Some(watcher) = &workspace.tub_watcher else {
              workspace.tub_watch_polling = false;
              return false;
            };
            let has_relevant_event = watcher
              .drain_events()
              .into_iter()
              .any(|event| event.is_ok());
            if has_relevant_event {
              if workspace.tub_scan_in_flight {
                workspace.tub_scan_pending = true;
                workspace.tub_status = "Indexing queued".into();
              } else {
                workspace.refresh_tub(cx);
              }
            }
            true
          })
          .unwrap_or(false);
        if !keep_polling {
          break;
        }
      }
    })
    .detach();
  }

  fn remember_tub_dir_toggle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    if !self.tub_expanded_dirs.insert(path.clone()) {
      self.tub_expanded_dirs.remove(&path);
    }
    cx.notify();
  }

  pub(super) fn refresh_toolkit_search(&mut self, cx: &mut Context<Self>) {
    let query = self.toolkit_search_input.read(cx).value().to_string();
    let Some(index) = self.tub_index.clone() else {
      self.toolkit_hits.clear();
      self.toolkit_status = "Select a tub to search evidence.".into();
      cx.notify();
      return;
    };
    if query.trim().is_empty() {
      self.toolkit_hits.clear();
      self.toolkit_status = "Search DB8 blocks, tags, and analytics.".into();
      cx.notify();
      return;
    }

    self.toolkit_search_generation = self.toolkit_search_generation.wrapping_add(1);
    let generation = self.toolkit_search_generation;
    let kinds = self.toolkit_search_filter.kinds().to_vec();
    self.toolkit_status = "Searching...".into();
    cx.notify();

    cx.spawn(async move |workspace, cx| {
      let hits = cx
        .background_executor()
        .spawn(async move { index.search_content(&query, &kinds, TOOLKIT_RESULT_LIMIT) })
        .await;
      let _ = workspace.update(cx, |workspace, cx| {
        if workspace.toolkit_search_generation != generation {
          return;
        }
        match hits {
          Ok(hits) => {
            let hit_count = hits.len();
            workspace.toolkit_hits = hits;
            workspace.toolkit_status = if hit_count == 0 {
              "No matching DB8 evidence".into()
            } else {
              format!("{hit_count} results").into()
            };
          },
          Err(error) => {
            workspace.toolkit_hits.clear();
            workspace.toolkit_status = format!("Search failed: {error}").into();
          },
        }
        cx.notify();
      });
    })
    .detach();
  }

  fn set_toolkit_filter(&mut self, filter: ToolkitSearchFilter, cx: &mut Context<Self>) {
    self.toolkit_search_filter = filter;
    self.refresh_toolkit_search(cx);
  }

  fn insert_toolkit_hit(&mut self, hit_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(hit) = self.toolkit_hits.get(hit_ix).cloned() else {
      return;
    };
    if let Some(editor) = self.active_editor.clone() {
      editor.update(cx, |editor, cx| editor.insert_plain_text_from_toolkit(&hit.insert_text, cx));
      return;
    }
    if let Some(editor) = self.active_flow.clone() {
      editor.update(cx, |editor, cx| editor.insert_toolkit_text(&hit.title, &hit.insert_text, window, cx));
    }
  }

  fn open_toolkit_hit(&mut self, hit_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(hit) = self.toolkit_hits.get(hit_ix).cloned() else {
      return;
    };
    self.open_document_path(hit.path, window, cx);
  }

  fn open_tub_tree_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
    self.active_tub_path = Some(path.clone());
    let name = path
      .file_name()
      .map(|name| name.to_string_lossy().to_string())
      .unwrap_or_else(|| path.display().to_string());
    self.tub_status = format!("Opening {name}").into();
    cx.notify();
    self.open_document_path(path, window, cx);
  }

  fn render_toolkit_expanded(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let open_file_search = cx.listener(|workspace, _, window, cx| workspace.open_file_search_overlay(window, cx));
    let choose_tub = cx.listener(|workspace, _, window, cx| workspace.prompt_select_tub(window, cx));
    let refresh_tub = cx.listener(|workspace, _, _, cx| workspace.refresh_tub(cx));
    let result_list = if self.toolkit_hits.is_empty() {
      div()
        .h(px(120.0))
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(self.toolkit_status.clone())
        .into_any_element()
    } else {
      let item_sizes = Rc::new(vec![size(px(1.0), px(270.0)); self.toolkit_hits.len()]);
      v_virtual_list(cx.entity(), "toolkit-result-list", item_sizes, |workspace, range, _, cx| {
        range
          .filter_map(|ix| {
            workspace
              .toolkit_hits
              .get(ix)
              .cloned()
              .map(|hit| workspace.render_toolkit_hit(ix, &hit, cx))
          })
          .collect::<Vec<_>>()
      })
      .into_any_element()
    };

    v_flex()
      .size_full()
      .h_full()
      .bg(cx.theme().background)
      .border_l(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .child(
        h_flex()
          .h(px(34.0))
          .flex_none()
          .items_center()
          .justify_between()
          .gap_2()
          .px_2()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(
            h_flex()
              .items_center()
              .gap_2()
              .child(Icon::new(IconName::Search).xsmall().text_color(cx.theme().muted_foreground))
              .child(
                div()
                  .text_sm()
                  .font_weight(gpui::FontWeight::SEMIBOLD)
                  .text_color(cx.theme().foreground)
                  .child("Toolkit"),
              ),
          )
          .child(
            h_flex()
              .items_center()
              .gap_1()
              .child(
                Button::new("toolkit-pick-tub")
                  .label("Tub")
                  .xsmall()
                  .ghost()
                  .tooltip("Choose tub folder")
                  .on_click(choose_tub),
              )
              .child(
                Button::new("toolkit-refresh-tub")
                  .label("Refresh")
                  .xsmall()
                  .ghost()
                  .tooltip("Refresh tub index")
                  .on_click(refresh_tub),
              )
              .child(
                Button::new("collapse-toolkit-panel")
                  .icon(Icon::new(IconName::PanelRightClose).text_color(cx.theme().muted_foreground))
                  .xsmall()
                  .ghost()
                  .tooltip("Collapse toolkit")
                  .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.toggle_toolkit(cx);
                  })),
              ),
          ),
      )
      .child(
        v_flex()
          .flex_none()
          .gap_2()
          .p_2()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(Input::new(&self.toolkit_search_input).w_full().cleanable(true))
          .child(
            h_flex()
              .w_full()
              .items_center()
              .gap_1()
              .children([
                self.render_toolkit_filter_button(ToolkitSearchFilter::All, cx).into_any_element(),
                self.render_toolkit_filter_button(ToolkitSearchFilter::Blocks, cx).into_any_element(),
                self.render_toolkit_filter_button(ToolkitSearchFilter::Tags, cx).into_any_element(),
                self.render_toolkit_filter_button(ToolkitSearchFilter::Analytics, cx).into_any_element(),
              ]),
          )
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap_2()
              .child(
                div()
                  .flex_1()
                  .min_w_0()
                  .truncate()
                  .text_xs()
                  .text_color(cx.theme().muted_foreground)
                  .child(self.toolkit_status.clone()),
              )
              .child(
                Button::new("toolkit-global-file-search")
                  .icon(Icon::new(IconName::FolderOpen).text_color(cx.theme().link))
                  .xsmall()
                  .ghost()
                  .tooltip("Filename search")
                  .on_click(open_file_search),
              ),
          ),
      )
      .child(
        div()
          .flex_1()
          .min_h_0()
          .w_full()
          .overflow_hidden()
          .p_2()
          .child(result_list),
      )
  }

  fn render_toolkit_filter_button(&self, filter: ToolkitSearchFilter, cx: &mut Context<Self>) -> impl IntoElement {
    Button::new(("toolkit-filter", filter as usize))
      .label(filter.label())
      .xsmall()
      .ghost()
      .selected(self.toolkit_search_filter == filter)
      .on_click(cx.listener(move |workspace, _, _, cx| {
        workspace.set_toolkit_filter(filter, cx);
      }))
  }

  fn render_toolkit_hit(&self, ix: usize, hit: &flowstate_tub::SearchHit, cx: &mut Context<Self>) -> gpui::AnyElement {
    let open = cx.listener(move |workspace, _, window, cx| workspace.open_toolkit_hit(ix, window, cx));
    let insert = cx.listener(move |workspace, _, window, cx| workspace.insert_toolkit_hit(ix, window, cx));
    let title = if hit.title.is_empty() {
      hit.file_name.clone()
    } else {
      hit.title.clone()
    };
    let drag = ToolkitTextDrag {
      title: title.clone(),
      text: hit.insert_text.clone(),
    };
    let heading_path = if hit.heading_path.is_empty() {
      hit.display_path.clone()
    } else {
      hit.heading_path.join(" / ")
    };
    let cite = hit.cite.clone().unwrap_or_default();
    let preview_text = if hit.insert_text.trim().is_empty() {
      hit.snippet.as_str()
    } else {
      hit.insert_text.as_str()
    };
    let preview_lines = toolkit_preview_lines(preview_text);

    div()
      .id(("toolkit-hit", ix))
      .w_full()
      .h(px(258.0))
      .rounded(px(6.0))
      .border_1()
      .border_color(cx.theme().border)
      .bg(cx.theme().popover)
      .overflow_hidden()
      .on_drag(drag, |drag, _, _, cx| {
        cx.stop_propagation();
        cx.new(|_| drag.clone())
      })
      .child(
        v_flex()
          .gap_1()
          .p_2()
          .child(
            h_flex()
              .items_start()
              .gap_2()
              .child(Icon::new(hit_icon(hit.unit_kind)).xsmall().text_color(cx.theme().link))
              .child(
                v_flex()
                  .flex_1()
                  .min_w_0()
                  .gap_0p5()
                  .child(
                    div()
                      .text_sm()
                      .font_weight(gpui::FontWeight::SEMIBOLD)
                      .text_color(cx.theme().foreground)
                      .truncate()
                      .child(title),
                  )
                  .child(
                    div()
                      .text_xs()
                      .truncate()
                      .text_color(cx.theme().muted_foreground)
                      .child(heading_path),
                  ),
              ),
          )
          .when(!cite.is_empty(), |this| {
            this.child(
              div()
                .text_xs()
                .text_color(cx.theme().info)
                .truncate()
                .child(cite),
            )
          })
          .child(
            div()
              .h(px(154.0))
              .w_full()
              .overflow_y_scrollbar()
              .rounded(px(4.0))
              .border_1()
              .border_color(rgb(0xd1d5db))
              .bg(rgb(0xffffff))
              .p_3()
              .text_xs()
              .line_height(px(16.0))
              .font_family("Arial")
              .text_color(rgb(0x111827))
              .children(preview_lines.into_iter().map(|line| {
                div()
                  .w_full()
                  .min_h(px(16.0))
                  .whitespace_normal()
                  .child(line)
              })),
          )
          .child(
            h_flex()
              .items_center()
              .justify_end()
              .gap_1()
              .pt_1()
              .child(
                Button::new(("toolkit-open-hit", ix))
                  .label("Open")
                  .xsmall()
                  .ghost()
                  .on_click(open),
              )
              .child(
                Button::new(("toolkit-insert-hit", ix))
                  .label("Insert")
                  .xsmall()
                  .on_click(insert),
              ),
          ),
      )
      .into_any_element()
  }

  pub(super) fn render_tub_nav(&self, nav_width: Pixels, cx: &mut Context<Self>) -> gpui::AnyElement {
    let choose_tub = cx.listener(|workspace, _, window, cx| workspace.prompt_select_tub(window, cx));
    let refresh_tub = cx.listener(|workspace, _, _, cx| workspace.refresh_tub(cx));
    let indexed_file_count = self.tub_files.len();
    let tub_status = if indexed_file_count == 0 {
      self.tub_status.clone()
    } else {
      format!("{} total files", indexed_file_count).into()
    };
    let tree_list = if self.tub_tree_entries.is_empty() {
      div()
        .h(px(120.0))
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child("No tub files")
        .into_any_element()
    } else {
      let workspace = cx.entity().downgrade();
      let active_tub_path = self.active_tub_path.clone();
      tree(&self.tub_tree, move |ix, entry, _selected, window, cx| {
        let path = PathBuf::from(entry.item().id.as_ref());
        let is_folder = entry.is_folder();
        let is_expanded = entry.is_expanded();
        let is_active = !is_folder && active_tub_path.as_ref() == Some(&path);
        let depth = entry.depth();
        let guide = OutlineRowGuides {
          ancestor_depths: (0..depth).collect(),
          extends_from_toggle: is_folder && is_expanded,
        };
        let icon = if is_folder && is_expanded {
          IconName::FolderOpen
        } else if is_folder {
          IconName::FolderClosed
        } else {
          IconName::File
        };
        let icon_color = if is_folder {
          outline_hierarchy_color(depth, cx)
        } else {
          cx.theme().muted_foreground
        };
        let workspace_for_toggle = workspace.clone();
        let toggle_path = path.clone();
        let toggle_action: SidebarTreeAction = Rc::new(move |_: &mut Window, cx: &mut App| {
          let path = toggle_path.clone();
          let _ = workspace_for_toggle.update(cx, |workspace, cx| workspace.remember_tub_dir_toggle(path, cx));
        });
        let workspace_for_label = workspace.clone();
        let label_path = path.clone();
        let label_action: SidebarTreeAction = Rc::new(move |window: &mut Window, cx: &mut App| {
          let path = label_path.clone();
          let _ = workspace_for_label.update(cx, |workspace, cx| {
            if is_folder {
              workspace.remember_tub_dir_toggle(path, cx);
            } else {
              workspace.open_tub_tree_file(path, window, cx);
            }
          });
        });
        render_sidebar_tree_row(SidebarTreeRow {
          row_id: ("tub-tree-item", ix),
          toggle_id: ("tub-toggle", ix),
          label_id: ("tub-label", ix),
          label: entry.item().label.clone(),
          nav_width,
          depth,
          is_folder,
          is_expanded,
          is_active,
          guide,
          icon: Some(icon),
          icon_color: Some(icon_color),
          toggle_action: Some(toggle_action),
          label_action,
          stop_icon_mouse_down: !is_folder,
          stop_label_mouse_down: !is_folder,
        }, window, cx)
      })
      .into_any_element()
    };

    v_flex()
      .size_full()
      .h_full()
      .gap_2()
      .p_2()
      .bg(cx.theme().sidebar)
      .text_color(cx.theme().sidebar_foreground)
      .child(self.render_left_nav_header("Tub", cx))
      .child(
        h_flex()
          .w_full()
          .gap_1()
          .child(
            Button::new("tub-pick-root")
              .label("Choose")
              .xsmall()
              .on_click(choose_tub),
          )
          .child(
            Button::new("tub-refresh")
              .label("Refresh")
              .xsmall()
              .ghost()
              .on_click(refresh_tub),
          ),
      )
      .child(
        div()
          .text_xs()
          .text_color(cx.theme().muted_foreground)
          .truncate()
          .child(tub_status),
      )
      .child(
        div()
          .flex_1()
          .w_full()
          .min_h_0()
          .overflow_hidden()
          .child(tree_list),
      )
      .into_any_element()
  }

  pub(super) fn render_left_nav_header(&self, title: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_2()
      .child(
        h_flex()
          .items_center()
          .gap_1()
          .child(
            Button::new("left-nav-outline-mode")
              .label("Outline")
              .xsmall()
              .ghost()
              .selected(self.left_nav_mode == LeftNavMode::Outline)
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.left_nav_mode = LeftNavMode::Outline;
                cx.notify();
              })),
          )
          .child(
            Button::new("left-nav-tub-mode")
              .label("Tub")
              .xsmall()
              .ghost()
              .selected(self.left_nav_mode == LeftNavMode::Tub)
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.left_nav_mode = LeftNavMode::Tub;
                cx.notify();
              })),
          ),
      )
      .child(
        h_flex()
          .items_center()
          .gap_1()
          .child(
            div()
              .text_sm()
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .text_color(cx.theme().sidebar_primary)
              .child(title),
          )
          .child(
            Button::new("collapse-left-panel")
              .icon(Icon::new(IconName::PanelLeftClose).text_color(cx.theme().sidebar_foreground))
              .xsmall()
              .ghost()
              .tooltip("Collapse left panel")
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.toggle_outline(cx);
              })),
          ),
      )
  }

}

fn toolkit_preview_lines(text: &str) -> Vec<SharedString> {
  let mut lines = Vec::new();
  let mut truncated = false;
  let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
  for (ix, line) in normalized.lines().enumerate() {
    if ix >= 160 {
      truncated = true;
      break;
    }
    let line = line.trim_end();
    lines.push(if line.is_empty() {
      SharedString::from(" ")
    } else {
      SharedString::from(line.to_string())
    });
  }
  if lines.is_empty() {
    lines.push(SharedString::from("No preview text"));
  } else if truncated {
    lines.push(SharedString::from("..."));
  }
  lines
}

fn hit_icon(kind: flowstate_tub::SearchUnitKind) -> IconName {
  match kind {
    flowstate_tub::SearchUnitKind::File => IconName::File,
    flowstate_tub::SearchUnitKind::Pocket | flowstate_tub::SearchUnitKind::Hat => IconName::FolderOpen,
    flowstate_tub::SearchUnitKind::BlockSection => IconName::File,
    flowstate_tub::SearchUnitKind::TagSection => IconName::Search,
    flowstate_tub::SearchUnitKind::Analytic => IconName::File,
    flowstate_tub::SearchUnitKind::Card => IconName::File,
    flowstate_tub::SearchUnitKind::Cite => IconName::File,
    flowstate_tub::SearchUnitKind::Paragraph => IconName::File,
    flowstate_tub::SearchUnitKind::FlowNode => IconName::File,
    flowstate_tub::SearchUnitKind::Document => IconName::File,
  }
}

fn build_tub_tree_items(root: &Path, files: &[flowstate_tub::TubFile], expanded_dirs: &std::collections::HashSet<PathBuf>) -> Vec<TreeItem> {
  let mut dirs = BTreeSet::<PathBuf>::new();
  let mut files_by_parent = BTreeMap::<PathBuf, Vec<&flowstate_tub::TubFile>>::new();
  let mut child_dirs = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();

  for file in files {
    let relative_parent = PathBuf::from(&file.parent_display_path);
    let mut current = PathBuf::new();
    for component in relative_parent.components() {
      let next = current.join(component.as_os_str());
      dirs.insert(next.clone());
      child_dirs.entry(current.clone()).or_default().insert(next.clone());
      current = next;
    }
    files_by_parent.entry(relative_parent).or_default().push(file);
  }

  for files in files_by_parent.values_mut() {
    files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
  }

  build_tub_tree_dir_items(root, Path::new(""), &dirs, &child_dirs, &files_by_parent, expanded_dirs)
}

fn build_tub_tree_dir_items(
  root: &Path,
  relative_dir: &Path,
  dirs: &BTreeSet<PathBuf>,
  child_dirs: &BTreeMap<PathBuf, BTreeSet<PathBuf>>,
  files_by_parent: &BTreeMap<PathBuf, Vec<&flowstate_tub::TubFile>>,
  expanded_dirs: &std::collections::HashSet<PathBuf>,
) -> Vec<TreeItem> {
  let mut items = Vec::new();
  let children = child_dirs
    .get(relative_dir)
    .cloned()
    .unwrap_or_default();

  for child in children {
    if !dirs.contains(&child) {
      continue;
    }
    let absolute = root.join(&child);
    let label = child
      .file_name()
      .map(|name| name.to_string_lossy().to_string())
      .unwrap_or_default();
    let child_items = build_tub_tree_dir_items(root, &child, dirs, child_dirs, files_by_parent, expanded_dirs);
    items.push(
      TreeItem::new(absolute.to_string_lossy().to_string(), label)
        .children(child_items)
        .expanded(expanded_dirs.contains(&absolute)),
    );
  }

  if let Some(files) = files_by_parent.get(relative_dir) {
    items.extend(files.iter().map(|file| {
      TreeItem::new(file.path.to_string_lossy().to_string(), file.file_name.clone())
    }));
  }

  items
}
