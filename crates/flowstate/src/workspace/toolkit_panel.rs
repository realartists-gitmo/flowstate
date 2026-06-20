use std::{
  collections::{BTreeMap, BTreeSet, HashSet},
  path::{Path, PathBuf},
  rc::Rc,
  sync::Arc,
  time::Duration,
};

use gpui::{App, Context, IntoElement, PathPromptOptions, Pixels, Timer, Window, div, point, prelude::*, px, size};
use gpui_component::{
  ActiveTheme as _, Icon, IconName, Selectable as _, Sizable,
  button::{Button, ButtonVariants},
  h_flex,
  input::Input,
  menu::{DropdownMenu as _, PopupMenuItem},
  resizable::{h_resizable, resizable_panel},
  tree::{TreeItem, tree},
  v_flex, v_virtual_list,
};

use crate::{
  app_settings::{flowstate_data_dir, load_document_theme, save_tub_root},
  rich_text_element::{
    DocumentTheme, InputParagraph, InputRun, ParagraphStyle, RichTextDocumentElement, RunSemanticStyle, RunStyles, ToolkitTextDrag,
    document_from_input,
  },
};

use super::{
  APP_CHROME_BORDER_WIDTH, LeftNavMode, OutlineRowGuides, SIDE_PANEL_COLLAPSED_WIDTH, SidebarTreeAction, SidebarTreeRow, ToolkitSearchFilter,
  ToolkitTool, Workspace, outline_hierarchy_color, render_sidebar_tree_row,
};

const TOOLKIT_RESULT_LIMIT: usize = 32;
const TUB_FILE_SEARCH_LIMIT: usize = 200;
const TOOLKIT_PREVIEW_PARAGRAPH_LIMIT: usize = 8;
const TOOLKIT_PREVIEW_FALLBACK_LINE_LIMIT: usize = 160;
const TOOLKIT_PREVIEW_ZOOM: f32 = 0.78;

struct TubLoadResult {
  root: PathBuf,
  index: Arc<flowstate_tub::TubIndex>,
  files: Vec<flowstate_tub::TubFile>,
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
    } else if self.active_toolkit_tool.is_some() {
      px(380.0)
    } else {
      px(40.0)
    };
    let toolkit_range_end = if self.toolkit_collapsed || self.active_toolkit_tool.is_none() {
      toolkit_width
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
            self.render_toolkit_rail_area(cx).into_any_element()
          }),
      )
  }

  fn render_toolkit_rail_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
    if self.active_toolkit_tool == Some(ToolkitTool::Tub) {
      return self.render_toolkit_expanded(cx).into_any_element();
    }

    self.render_toolkit_icon_bar(cx).into_any_element()
  }

  fn render_toolkit_icon_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let tub_selected = self.active_toolkit_tool == Some(ToolkitTool::Tub);

    v_flex()
      .h_full()
      .w(px(40.0))
      .flex_none()
      .items_center()
      .gap_1()
      .py_2()
      .bg(cx.theme().background)
      .border_l(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .child(
        Button::new("toolkit-global-file-search")
          .icon(
            Icon::default()
              .path("icons/file-search-corner.svg")
              .text_color(cx.theme().link),
          )
          .xsmall()
          .ghost()
          .tooltip("Search files")
          .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.open_file_search_overlay(window, cx);
          })),
      )
      .child(
        Button::new("toolkit-tub-tool")
          .icon(
            Icon::default()
              .path("icons/notebook-text.svg")
              .text_color(if tub_selected { cx.theme().link } else { cx.theme().muted_foreground }),
          )
          .xsmall()
          .ghost()
          .selected(tub_selected)
          .tooltip("Tub index")
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.toggle_toolkit_tool(ToolkitTool::Tub, cx);
          })),
      )
      .child(div().flex_1())
      .child(
        Button::new("collapse-toolkit-rail")
          .icon(Icon::new(IconName::PanelRightClose).text_color(cx.theme().muted_foreground))
          .xsmall()
          .ghost()
          .tooltip("Hide toolkit")
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.toggle_toolkit(cx);
          })),
      )
  }

  pub(super) fn load_tub_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
    self.tub_root = Some(root.clone());
    self.tub_index = None;
    self.tub_files.clear();
    self.tub_tree_items.clear();
    self.tub_tree_entries.clear();
    self.tub_file_search_generation = self.tub_file_search_generation.wrapping_add(1);
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
    self.tub_tree_items.clear();
    self.tub_tree_entries.clear();
    self.tub_file_search_generation = self.tub_file_search_generation.wrapping_add(1);
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
          anyhow::Ok(TubLoadResult { root, index, files })
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
            workspace.tub_root = Some(result.root);
            workspace.tub_watcher = result.index.start_watcher().ok();
            workspace.tub_index = Some(result.index);
            workspace.tub_files = result.files;
            workspace.rebuild_tub_tree_cache();
            workspace.refresh_tub_file_search(cx);
            workspace.tub_status = "Tub indexed".into();
            workspace.toolkit_status = if workspace
              .toolkit_search_input
              .read(cx)
              .value()
              .trim()
              .is_empty()
            {
              "Search DB8 blocks, tags, and analytics.".into()
            } else {
              "Ready".into()
            };
            workspace.refresh_toolkit_search(cx);
            workspace.start_tub_watch_poll(cx);
            if rescan_pending && let Some(root) = workspace.tub_root.clone() {
              workspace.spawn_tub_scan(root, true, cx);
            }
          },
          Err(error) => {
            workspace.tub_index = None;
            workspace.tub_watcher = None;
            workspace.tub_files.clear();
            workspace.tub_tree_items.clear();
            workspace.tub_tree_entries.clear();
            workspace.tub_file_search_generation = workspace.tub_file_search_generation.wrapping_add(1);
            workspace
              .tub_tree
              .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
            workspace.toolkit_hits.clear();
            workspace.tub_status = format!("Tub unavailable: {error}").into();
            workspace.toolkit_status = "Tub unavailable".into();
            if rescan_pending && let Some(root) = workspace.tub_root.clone() {
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
            let has_relevant_event = watcher.drain_has_db8_change();
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

  pub(super) fn refresh_tub_file_search(&mut self, cx: &mut Context<Self>) {
    let query = self
      .tub_file_search_input
      .read(cx)
      .value()
      .trim()
      .to_string();
    let Some(root) = self.tub_root.clone() else {
      self.tub_tree_entries.clear();
      self
        .tub_tree
        .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
      cx.notify();
      return;
    };

    if query.is_empty() {
      if self.tub_tree_items.is_empty() && !self.tub_files.is_empty() {
        self.rebuild_tub_tree_cache();
      }
      self.tub_tree_entries = tub_tree_entries_for_files(&root, &self.tub_files, &self.tub_expanded_dirs);
      self
        .tub_tree
        .update(cx, |tree, cx| tree.set_items(self.tub_tree_items.clone(), cx));
      cx.notify();
      return;
    }

    self.tub_file_search_generation = self.tub_file_search_generation.wrapping_add(1);
    let files = filter_tub_files_by_name(&self.tub_files, &query, TUB_FILE_SEARCH_LIMIT);
    let expanded_dirs = expanded_tub_search_dirs(&root, &files);
    let tree_items = build_tub_tree_items(&root, &files, &expanded_dirs);
    self.tub_tree_entries = tub_tree_entries_for_files(&root, &files, &expanded_dirs);
    self
      .tub_tree
      .update(cx, |tree, cx| tree.set_items(tree_items, cx));
    cx.notify();
  }

  fn rebuild_tub_tree_cache(&mut self) {
    let Some(root) = self.tub_root.as_deref() else {
      self.tub_tree_items.clear();
      self.tub_tree_entries.clear();
      return;
    };
    self.tub_tree_items = build_tub_tree_items(root, &self.tub_files, &self.tub_expanded_dirs);
    self.tub_tree_entries = tub_tree_entries_for_files(root, &self.tub_files, &self.tub_expanded_dirs);
  }

  pub(super) fn refresh_toolkit_search(&mut self, cx: &mut Context<Self>) {
    let query = self.toolkit_search_input.read(cx).value().to_string();
    let Some(index) = self.tub_index.clone() else {
      self.toolkit_hits.clear();
      self.toolkit_status = "Select a tub to search evidence.".into();
      cx.notify();
      return;
    };
    let query_is_empty = query.trim().is_empty();

    self.toolkit_search_generation = self.toolkit_search_generation.wrapping_add(1);
    let generation = self.toolkit_search_generation;
    let kinds = self.toolkit_search_filter.kinds().to_vec();
    self.toolkit_status = if query_is_empty {
      "Loading tub cards...".into()
    } else {
      "Searching...".into()
    };
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
            workspace.toolkit_status = if query_is_empty {
              if hit_count == 0 {
                "No DB8 evidence cards".into()
              } else {
                format!("{hit_count} tub cards").into()
              }
            } else if hit_count == 0 {
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

  fn toggle_toolkit_hit_expanded(&mut self, hit_ix: usize, cx: &mut Context<Self>) {
    let Some(hit) = self.toolkit_hits.get(hit_ix) else {
      return;
    };
    let key = toolkit_hit_key(hit);
    if !self.expanded_toolkit_hits.insert(key.clone()) {
      self.expanded_toolkit_hits.remove(&key);
    }
    cx.notify();
  }

  fn insert_toolkit_hit(&mut self, hit_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(hit) = self.toolkit_hits.get(hit_ix).cloned() else {
      return;
    };
    if let Some(editor) = self.active_editor.clone() {
      let paragraphs = toolkit_hit_insert_paragraphs(&hit);
      editor.update(cx, |editor, cx| editor.insert_toolkit_text_at_caret(paragraphs, cx));
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
    if let Some(paragraph_ix) = hit.paragraph_start {
      self.open_document_path_at_paragraph(hit.path, paragraph_ix, window, cx);
    } else {
      self.open_document_path(hit.path, window, cx);
    }
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
    let (preview_theme, _) = self
      .active_editor
      .as_ref()
      .map(|editor| {
        let editor = editor.read(cx);
        (editor.document_theme(), editor.invisibility_mode())
      })
      .unwrap_or_else(|| (load_document_theme(), false));

    let result_list = if self.tub_root.is_none() {
      self.render_centered_tub_picker("toolkit-select-tub-folder", "Select a tub to search evidence", cx)
    } else if self.toolkit_hits.is_empty() {
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
      let item_sizes = Rc::new(
        self
          .toolkit_hits
          .iter()
          .map(|hit| {
            let expanded =
              toolkit_hit_preview_can_expand(hit, toolkit_hit_preview_text(hit)) && self.expanded_toolkit_hits.contains(&toolkit_hit_key(hit));
            size(px(1.0), toolkit_hit_virtual_height(hit, expanded, &preview_theme))
          })
          .collect::<Vec<_>>(),
      );
      let preview_theme_for_rows = preview_theme.clone();
      v_virtual_list(cx.entity(), "toolkit-hit-virtual-list", item_sizes, move |workspace, range, _, cx| {
        range
          .filter_map(|ix| {
            workspace.toolkit_hits.get(ix).map(|hit| {
              let expanded = toolkit_hit_preview_can_expand(hit, toolkit_hit_preview_text(hit))
                && workspace
                  .expanded_toolkit_hits
                  .contains(&toolkit_hit_key(hit));
              workspace.render_toolkit_hit(ix, hit, toolkit_hit_card_height(hit, expanded, &preview_theme_for_rows), cx)
            })
          })
          .collect::<Vec<_>>()
      })
      .track_scroll(&self.toolkit_results_scroll_handle)
      .into_any_element()
    };

    v_flex()
      .size_full()
      .h_full()
      .min_h_0()
      .bg(cx.theme().background)
      .border_l(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .child(
        h_flex()
          .h(px(34.0))
          .flex_none()
          .items_center()
          .gap_2()
          .px_2()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(
            h_flex()
              .flex_none()
              .min_w_0()
              .items_center()
              .gap_2()
              .child(
                Button::new("toolkit-panel-file-search")
                  .icon(
                    Icon::default()
                      .path("icons/file-search-corner.svg")
                      .text_color(cx.theme().sidebar_primary),
                  )
                  .xsmall()
                  .ghost()
                  .tooltip("Search files")
                  .on_click(cx.listener(|workspace, _, window, cx| {
                    workspace.open_file_search_overlay(window, cx);
                  })),
              )
              .child(
                Button::new("toolkit-tub-tool")
                  .icon(
                    Icon::default()
                      .path("icons/notebook-text.svg")
                      .text_color(cx.theme().sidebar_primary),
                  )
                  .xsmall()
                  .ghost()
                  .selected(true)
                  .tooltip("Close tub panel")
                  .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.toggle_toolkit_tool(ToolkitTool::Tub, cx);
                  })),
              ),
          )
          .child(div().flex_1())
          .child(
            div()
              .min_w_0()
              .truncate()
              .text_sm()
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .text_color(cx.theme().sidebar_primary)
              .child("Toolkit"),
          )
          .when(self.tub_root.is_some(), |this| {
            this.child(
              Button::new("toolkit-change-tub")
                .icon(Icon::new(IconName::FolderOpen).text_color(cx.theme().sidebar_primary))
                .xsmall()
                .ghost()
                .tooltip("Switch tub folder")
                .on_click(cx.listener(|workspace, _, window, cx| {
                  workspace.prompt_select_tub(window, cx);
                })),
            )
          }),
      )
      .when(self.tub_root.is_some(), |this| {
        this.child(
          v_flex()
            .flex_none()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
              Input::new(&self.toolkit_search_input)
                .xsmall()
                .w_full()
                .cleanable(true)
                .prefix(
                  Icon::new(IconName::Search)
                    .xsmall()
                    .text_color(cx.theme().muted_foreground),
                )
                .suffix(self.render_toolkit_filter_menu(cx))
                .text_color(cx.theme().foreground)
                .placeholder_color(cx.theme().muted_foreground),
            ),
        )
      })
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .w_full()
          .overflow_hidden()
          .p_2()
          .child(result_list),
      )
  }

  fn render_centered_tub_picker(&self, id: &'static str, label: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
    div()
      .size_full()
      .min_h(px(160.0))
      .flex()
      .items_center()
      .justify_center()
      .child(
        Button::new(id)
          .icon(Icon::new(IconName::FolderOpen).text_color(cx.theme().primary_foreground))
          .label(label)
          .small()
          .tooltip("Select debate tub folder")
          .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.prompt_select_tub(window, cx);
          })),
      )
      .into_any_element()
  }

  fn render_toolkit_filter_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    let selected_filter = self.toolkit_search_filter;
    let filters = [
      ToolkitSearchFilter::All,
      ToolkitSearchFilter::Blocks,
      ToolkitSearchFilter::Tags,
      ToolkitSearchFilter::Analytics,
    ];

    Button::new("toolkit-filter-menu")
      .label(self.toolkit_search_filter.label())
      .xsmall()
      .ghost()
      .tooltip("Search filter")
      .dropdown_menu(move |menu, _, _| {
        filters
          .into_iter()
          .fold(menu.min_w(px(140.0)), |menu, filter| {
            let workspace = workspace.clone();
            menu.item(
              PopupMenuItem::new(filter.label())
                .checked(filter == selected_filter)
                .on_click(move |_, _, cx| {
                  let _ = workspace.update(cx, |workspace, cx| {
                    workspace.set_toolkit_filter(filter, cx);
                  });
                }),
            )
          })
      })
  }

  fn render_toolkit_hit(&self, ix: usize, hit: &flowstate_tub::SearchHit, card_height: Pixels, cx: &mut Context<Self>) -> gpui::AnyElement {
    let open = cx.listener(move |workspace, _, window, cx| workspace.open_toolkit_hit(ix, window, cx));
    let insert = cx.listener(move |workspace, _, window, cx| workspace.insert_toolkit_hit(ix, window, cx));
    let toggle_expanded = cx.listener(move |workspace, _, _, cx| workspace.toggle_toolkit_hit_expanded(ix, cx));
    let title = if hit.title.is_empty() {
      hit.file_name.clone()
    } else {
      hit.title.clone()
    };
    let preview_text = toolkit_hit_preview_text(hit);
    let can_expand = toolkit_hit_preview_can_expand(hit, preview_text);
    let expanded = can_expand && self.expanded_toolkit_hits.contains(&toolkit_hit_key(hit));
    let paragraphs = toolkit_hit_insert_paragraphs(hit);
    let drag = ToolkitTextDrag {
      title: title.clone(),
      text: hit.insert_text.clone(),
      paragraphs,
      cursor_offset: point(px(0.0), px(0.0)),
    };
    let (preview_theme, preview_invisibility_mode) = self
      .active_editor
      .as_ref()
      .map(|editor| {
        let editor = editor.read(cx);
        (editor.document_theme(), editor.invisibility_mode())
      })
      .unwrap_or_else(|| (load_document_theme(), false));
    let preview_document = toolkit_preview_document(hit, preview_text, preview_theme, expanded);
    let preview_bg = preview_document.theme.document_background_color;
    let hover_group = format!("toolkit-hit-hover-{ix}");
    let overlay_bg = cx.theme().popover.opacity(0.92);
    let overlay_border = cx.theme().border.opacity(0.64);
    let card_radius = cx.theme().radius;
    let preview_radius = card_radius.max(px(1.0)) - px(1.0);
    let expand_icon = if expanded { IconName::Minimize } else { IconName::Maximize };
    let expand_tooltip = if expanded { "Collapse preview" } else { "Expand preview" };

    div()
      .id(("toolkit-hit", ix))
      .group(hover_group.clone())
      .w_full()
      .h(card_height)
      .relative()
      .rounded(card_radius)
      .border_1()
      .border_color(cx.theme().border)
      .bg(preview_bg)
      .p(px(1.0))
      .overflow_hidden()
      .block_mouse_except_scroll()
      .on_drag(drag, |drag, cursor_offset, _, cx| {
        cx.stop_propagation();
        let mut drag = drag.clone();
        drag.cursor_offset = cursor_offset;
        cx.new(|_| drag)
      })
      .child(
        div()
          .w_full()
          .h_full()
          .relative()
          .rounded(preview_radius)
          .overflow_hidden()
          .bg(preview_bg)
          .child(
            div()
              .id(("toolkit-preview-scroll-area", ix))
              .w_full()
              .h_full()
              .bg(preview_bg)
              .child(RichTextDocumentElement::new(preview_document).with_invisibility_mode(preview_invisibility_mode)),
          ),
      )
      .child(
        h_flex()
          .absolute()
          .right_2()
          .bottom_2()
          .invisible()
          .group_hover(hover_group, |this| this.visible().opacity(0.92))
          .items_center()
          .gap_1()
          .rounded(px(4.0))
          .border_1()
          .border_color(overlay_border)
          .bg(overlay_bg)
          .p_1()
          .when(can_expand, |this| {
            this.child(
              Button::new(("toolkit-expand-hit", ix))
                .icon(Icon::new(expand_icon).text_color(cx.theme().foreground))
                .xsmall()
                .ghost()
                .tooltip(expand_tooltip)
                .on_click(toggle_expanded),
            )
          })
          .child(
            Button::new(("toolkit-open-hit", ix))
              .icon(Icon::new(IconName::ExternalLink).text_color(cx.theme().foreground))
              .xsmall()
              .ghost()
              .tooltip("Open")
              .on_click(open),
          )
          .child(
            Button::new(("toolkit-insert-hit", ix))
              .icon(Icon::new(IconName::Plus).text_color(cx.theme().primary_foreground))
              .xsmall()
              .tooltip("Insert")
              .on_click(insert),
          ),
      )
      .into_any_element()
  }

  fn render_tub_tree(&self, nav_width: Pixels, cx: &mut Context<Self>) -> gpui::AnyElement {
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
      let icon_color = outline_hierarchy_color(depth, cx);
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
      render_sidebar_tree_row(
        SidebarTreeRow {
          row_id: ("tub-tree-item", ix),
          toggle_id: ("tub-toggle", ix),
          label_id: ("tub-label", ix),
          label: entry.item().label.clone(),
          nav_width,
          depth,
          is_folder,
          is_expanded,
          is_active,
          has_search_match: false,
          guide,
          icon: Some(icon),
          icon_color: Some(icon_color),
          toggle_action: Some(toggle_action),
          label_action,
          stop_icon_mouse_down: !is_folder,
          stop_label_mouse_down: !is_folder,
          context_menu_action: None,
        },
        window,
        cx,
      )
    })
    .into_any_element()
  }

  pub(super) fn render_tub_nav(&self, nav_width: Pixels, cx: &mut Context<Self>) -> gpui::AnyElement {
    let file_search_active = !self
      .tub_file_search_input
      .read(cx)
      .value()
      .trim()
      .is_empty();
    let tree_list = if self.tub_root.is_none() {
      self.render_centered_tub_picker("left-nav-select-tub-folder", "Select a tub folder", cx)
    } else if self.tub_tree_entries.is_empty() {
      div()
        .h(px(120.0))
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(if file_search_active { "No matching tub files" } else { "No tub files" })
        .into_any_element()
    } else {
      self.render_tub_tree(nav_width, cx)
    };

    v_flex()
      .size_full()
      .h_full()
      .gap_2()
      .p_2()
      .bg(cx.theme().sidebar)
      .text_color(cx.theme().sidebar_foreground)
      .child(self.render_left_nav_header("Tub", cx))
      .when(self.tub_root.is_some(), |this| {
        this
          .child(
            h_flex()
              .w_full()
              .items_center()
              .justify_between()
              .gap_2()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(
                div().min_w_0().truncate().child(
                  self
                    .tub_root
                    .as_ref()
                    .map(|root| root.display().to_string())
                    .unwrap_or_default(),
                ),
              )
              .child(
                Button::new("left-nav-change-tub")
                  .icon(Icon::new(IconName::FolderOpen).text_color(cx.theme().sidebar_primary))
                  .xsmall()
                  .ghost()
                  .tooltip("Switch tub folder")
                  .on_click(cx.listener(|workspace, _, window, cx| {
                    workspace.prompt_select_tub(window, cx);
                  })),
              ),
          )
          .child(
            div().w_full().flex_none().child(
              Input::new(&self.tub_file_search_input)
                .xsmall()
                .w_full()
                .cleanable(true)
                .prefix(
                  Icon::new(IconName::Search)
                    .xsmall()
                    .text_color(cx.theme().muted_foreground),
                )
                .text_color(cx.theme().sidebar_foreground)
                .placeholder_color(cx.theme().muted_foreground)
                .bg(cx.theme().sidebar)
                .border_color(cx.theme().sidebar_border),
            ),
          )
      })
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
    let (swap_icon_path, swap_tooltip, swap_mode) = match self.left_nav_mode {
      LeftNavMode::Outline => ("icons/archive.svg", "Swap to tub", LeftNavMode::Tub),
      LeftNavMode::Tub => ("icons/table-of-contents.svg", "Swap to outline", LeftNavMode::Outline),
    };
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_2()
      .child(
        div()
          .text_sm()
          .font_weight(gpui::FontWeight::SEMIBOLD)
          .text_color(cx.theme().sidebar_primary)
          .child(title),
      )
      .child(
        h_flex()
          .items_center()
          .gap_1()
          .child(
            Button::new("left-nav-swap-mode")
              .icon(
                Icon::default()
                  .path(swap_icon_path)
                  .xsmall()
                  .text_color(cx.theme().sidebar_primary),
              )
              .xsmall()
              .ghost()
              .tooltip(swap_tooltip)
              .on_click(cx.listener(move |workspace, _, _, cx| {
                workspace.left_nav_mode = swap_mode;
                cx.notify();
              })),
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

fn toolkit_preview_document(
  hit: &flowstate_tub::SearchHit,
  fallback_text: &str,
  mut theme: DocumentTheme,
  expanded: bool,
) -> crate::rich_text_element::DocumentProjection {
  theme.zoom_factor *= TOOLKIT_PREVIEW_ZOOM;
  theme.pageless_inset_x = px(10.0);
  theme.pageless_inset_top = px(8.0);
  theme.pageless_inset_bottom = px(12.0);

  let paragraphs = if hit.preview_paragraphs.is_empty() {
    toolkit_fallback_paragraphs(fallback_text, (!expanded).then_some(TOOLKIT_PREVIEW_FALLBACK_LINE_LIMIT))
  } else if expanded {
    hit.preview_paragraphs.clone()
  } else {
    hit
      .preview_paragraphs
      .iter()
      .take(TOOLKIT_PREVIEW_PARAGRAPH_LIMIT)
      .cloned()
      .collect()
  };

  document_from_input(theme, paragraphs)
}

fn toolkit_hit_preview_text(hit: &flowstate_tub::SearchHit) -> &str {
  if hit.insert_text.trim().is_empty() {
    hit.snippet.as_str()
  } else {
    hit.insert_text.as_str()
  }
}

fn toolkit_hit_preview_can_expand(hit: &flowstate_tub::SearchHit, fallback_text: &str) -> bool {
  if !hit.preview_paragraphs.is_empty() {
    return hit.preview_paragraphs.len() > TOOLKIT_PREVIEW_PARAGRAPH_LIMIT;
  }

  fallback_text
    .replace("\r\n", "\n")
    .replace('\r', "\n")
    .lines()
    .count()
    > TOOLKIT_PREVIEW_FALLBACK_LINE_LIMIT
}

fn toolkit_hit_virtual_height(hit: &flowstate_tub::SearchHit, expanded: bool, theme: &DocumentTheme) -> Pixels {
  toolkit_hit_card_height(hit, expanded, theme) + px(12.0)
}

fn toolkit_hit_card_height(hit: &flowstate_tub::SearchHit, expanded: bool, theme: &DocumentTheme) -> Pixels {
  let can_expand = toolkit_hit_preview_can_expand(hit, toolkit_hit_preview_text(hit));
  if !expanded && can_expand {
    return px(258.0);
  }

  let estimated = toolkit_hit_preview_estimated_height(hit, expanded, theme);
  if expanded {
    estimated.clamp(px(258.0), px(720.0))
  } else {
    estimated.clamp(px(36.0), px(258.0))
  }
}

fn toolkit_hit_preview_estimated_height(hit: &flowstate_tub::SearchHit, expanded: bool, theme: &DocumentTheme) -> Pixels {
  let preview_paragraphs;
  let paragraphs = if hit.preview_paragraphs.is_empty() {
    preview_paragraphs = toolkit_fallback_paragraphs(toolkit_hit_preview_text(hit), (!expanded).then_some(TOOLKIT_PREVIEW_FALLBACK_LINE_LIMIT));
    preview_paragraphs.as_slice()
  } else {
    let limit = if expanded {
      hit.preview_paragraphs.len()
    } else {
      TOOLKIT_PREVIEW_PARAGRAPH_LIMIT.min(hit.preview_paragraphs.len())
    };
    &hit.preview_paragraphs[..limit]
  };

  let mut height = px(8.0) + px(12.0);
  for paragraph in paragraphs {
    height += toolkit_estimated_paragraph_height(paragraph, theme);
  }
  height
}

fn toolkit_estimated_paragraph_height(paragraph: &InputParagraph, theme: &DocumentTheme) -> Pixels {
  let zoom = (theme.zoom_factor * TOOLKIT_PREVIEW_ZOOM).max(0.01);
  let (base_font_size, spacing_before, spacing_after, border_pad_x, border_pad_y) = match paragraph.style {
    ParagraphStyle::Normal => (theme.body_font_size * zoom, px(0.0), theme.paragraph_after, px(0.0), px(0.0)),
    ParagraphStyle::Custom(slot) => theme.custom_paragraph_styles.get(&(slot & 0x7f)).map_or(
      (theme.body_font_size * zoom, px(0.0), theme.paragraph_after, px(0.0), px(0.0)),
      |style| {
        let border = style.border;
        let border_pad_x = border.map_or(px(0.0), |border| border.width + border.space_x);
        let border_pad_y = border.map_or(px(0.0), |border| border.width + border.space_y);
        (
          style.font_size * zoom,
          style.spacing_before,
          style.spacing_after,
          border_pad_x,
          border_pad_y,
        )
      },
    ),
  };

  let max_run_font_size = paragraph.runs.iter().fold(base_font_size, |max_size, run| {
    let run_size = match run.styles.semantic {
      RunSemanticStyle::Plain => base_font_size,
      RunSemanticStyle::Custom(slot) => theme
        .custom_semantic_styles
        .get(&(slot & 0x7f))
        .and_then(|style| style.font_size)
        .map_or(base_font_size, |font_size| font_size * zoom),
    };
    max_size.max(run_size)
  });
  let line_height = (max_run_font_size + max_run_font_size * theme.line_gap_fraction) * theme.line_spacing;
  let content_width = (px(344.0) - px(10.0) * 2.0 - border_pad_x * 2.0).max(px(1.0));
  let avg_char_width = (max_run_font_size * 0.50).max(px(1.0));
  let chars_per_line = ((content_width / avg_char_width).floor() as usize).max(1);
  let text_len = paragraph
    .runs
    .iter()
    .map(|run| run.text.chars().count())
    .sum::<usize>()
    .max(1);
  let forced_lines = paragraph
    .runs
    .iter()
    .map(|run| run.text.matches('\n').count())
    .sum::<usize>();
  let estimated_lines = text_len
    .div_ceil(chars_per_line)
    .saturating_add(forced_lines)
    .max(1);

  spacing_before + border_pad_y + line_height * estimated_lines as f32 + border_pad_y + spacing_after
}

fn toolkit_hit_key(hit: &flowstate_tub::SearchHit) -> String {
  format!("{}:{}", hit.file_id, hit.unit_id)
}

fn toolkit_hit_insert_paragraphs(hit: &flowstate_tub::SearchHit) -> Vec<InputParagraph> {
  if !hit.preview_paragraphs.is_empty() {
    return hit.preview_paragraphs.clone();
  }
  let text = if hit.insert_text.trim().is_empty() {
    hit.snippet.as_str()
  } else {
    hit.insert_text.as_str()
  };
  toolkit_fallback_paragraphs(text, None)
}

fn toolkit_fallback_paragraphs(text: &str, line_limit: Option<usize>) -> Vec<InputParagraph> {
  let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
  let mut truncated = false;
  let mut paragraphs = normalized
    .lines()
    .enumerate()
    .filter_map(|(ix, line)| {
      if line_limit.is_some_and(|limit| ix >= limit) {
        truncated = true;
        return None;
      }
      Some(toolkit_preview_fallback_paragraph(line.trim_end()))
    })
    .collect::<Vec<_>>();

  if paragraphs.is_empty() {
    paragraphs.push(toolkit_preview_fallback_paragraph("No preview text"));
  } else if truncated {
    paragraphs.push(toolkit_preview_fallback_paragraph("..."));
  }

  paragraphs
}

fn toolkit_preview_fallback_paragraph(text: &str) -> InputParagraph {
  InputParagraph {
    style: ParagraphStyle::Normal,
    runs: vec![InputRun {
      text: if text.is_empty() { " ".to_string() } else { text.to_string() },
      styles: RunStyles::default(),
    }],
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
      child_dirs
        .entry(current.clone())
        .or_default()
        .insert(next.clone());
      current = next;
    }
    files_by_parent
      .entry(relative_parent)
      .or_default()
      .push(file);
  }

  for files in files_by_parent.values_mut() {
    files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
  }

  build_tub_tree_dir_items(root, Path::new(""), &dirs, &child_dirs, &files_by_parent, expanded_dirs)
}

fn tub_tree_entries_for_files(
  root: &Path,
  files: &[flowstate_tub::TubFile],
  expanded_dirs: &std::collections::HashSet<PathBuf>,
) -> Vec<flowstate_tub::TubTreeNode> {
  build_tub_tree_items(root, files, expanded_dirs)
    .into_iter()
    .flat_map(|item| tub_tree_item_entries(item, 0))
    .collect()
}

fn tub_tree_item_entries(item: TreeItem, depth: usize) -> Vec<flowstate_tub::TubTreeNode> {
  let mut entries = Vec::new();
  let path = PathBuf::from(item.id.as_ref());
  let is_dir = item.is_folder();
  let expanded = item.is_expanded();
  entries.push(flowstate_tub::TubTreeNode {
    path: path.clone(),
    display_path: path.to_string_lossy().replace('\\', "/"),
    name: item.label.to_string(),
    is_dir,
    depth,
    expanded,
    file_kind: None,
  });
  if expanded {
    for child in item.children {
      entries.extend(tub_tree_item_entries(child, depth + 1));
    }
  }
  entries
}

fn expanded_tub_search_dirs(root: &Path, files: &[flowstate_tub::TubFile]) -> HashSet<PathBuf> {
  let mut expanded = HashSet::new();
  expanded.insert(root.to_path_buf());
  for file in files {
    let mut current = root.to_path_buf();
    for component in Path::new(&file.parent_display_path).components() {
      current = current.join(component.as_os_str());
      expanded.insert(current.clone());
    }
  }
  expanded
}

fn filter_tub_files_by_name(files: &[flowstate_tub::TubFile], query: &str, limit: usize) -> Vec<flowstate_tub::TubFile> {
  let query = query.trim().to_ascii_lowercase();
  if query.is_empty() {
    return files.iter().take(limit).cloned().collect();
  }

  let mut ranked = files
    .iter()
    .filter_map(|file| {
      let file_name = file.file_name.to_ascii_lowercase();
      let display_path = file.display_path.to_ascii_lowercase();
      let rank = if file_name.starts_with(&query) {
        0
      } else if file_name.contains(&query) {
        1
      } else if display_path.contains(&query) {
        2
      } else {
        return None;
      };
      Some((rank, file.display_path.len(), file))
    })
    .collect::<Vec<_>>();
  ranked.sort_by(|left, right| {
    left
      .0
      .cmp(&right.0)
      .then_with(|| left.1.cmp(&right.1))
      .then_with(|| left.2.display_path.cmp(&right.2.display_path))
  });
  ranked
    .into_iter()
    .take(limit)
    .map(|(_, _, file)| file.clone())
    .collect()
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
  let children = child_dirs.get(relative_dir).cloned().unwrap_or_default();

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
    items.extend(
      files
        .iter()
        .map(|file| TreeItem::new(file.path.to_string_lossy().to_string(), file.file_name.clone())),
    );
  }

  items
}
