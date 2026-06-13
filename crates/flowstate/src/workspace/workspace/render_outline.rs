#[hotpath::measure_all]
impl Workspace {
  fn render_left_nav(&mut self, nav_width: Pixels, cx: &mut Context<Self>) -> AnyElement {
    if self.left_nav_mode == LeftNavMode::Tub {
      return self.render_tub_nav(nav_width, cx);
    }
    if self.active_flow.is_some() {
      return self.render_flow_nav(cx);
    }
    self.refresh_outline_tree(cx);
    self.refresh_outline_viewport(cx);
    let workspace = cx.entity().downgrade();
    let active_outline_paragraph = self.active_outline_paragraph(cx);
    self.scroll_outline_item_into_view(active_outline_paragraph, cx);
    let outline_guides = self
      .outline_cache
      .as_ref()
      .map(|cache| cache.row_guides.clone())
      .unwrap_or_else(|| Rc::new(Vec::new()));
    let search_match_outline_paragraphs = self.search_match_outline_paragraphs(cx);
    let outline_levels: HashMap<usize, usize> = self
      .outline_cache
      .as_ref()
      .map(|cache| {
        let mut levels = HashMap::new();
        fn collect_levels(node: &OutlineNode, levels: &mut HashMap<usize, usize>) {
          levels.insert(node.paragraph_ix, node.level);
          for child in &node.children {
            collect_levels(child, levels);
          }
        }
        for node in cache.nodes.iter() {
          collect_levels(node, &mut levels);
        }
        levels
      })
      .unwrap_or_default();
    v_flex()
      .size_full()
      .h_full()
      .gap_1()
      .p_2()
      .bg(cx.theme().sidebar)
      .text_color(cx.theme().sidebar_foreground)
      .child(self.render_left_nav_header("Outline", cx))
      .child(
        div()
          .flex_1()
          .w_full()
          .overflow_hidden()
          .child(tree(&self.outline_tree, move |ix, entry, _selected, window, cx| {
            let paragraph_ix = outline_paragraph_ix(entry.item().id.as_ref());
            let is_folder = entry.is_folder();
            let is_expanded = entry.is_expanded();
            let is_active_outline = paragraph_ix == active_outline_paragraph;
            let has_search_match = paragraph_ix.is_some_and(|paragraph_ix| search_match_outline_paragraphs.contains(&paragraph_ix));
            let depth = entry.depth();
            let guide = outline_guides.get(ix).cloned().unwrap_or_default();
            let toggle_action: SidebarTreeAction = Rc::new({
              let workspace = workspace.clone();
              move |_: &mut Window, cx: &mut App| {
                if let Some(paragraph_ix) = paragraph_ix {
                  let _ = workspace.update(cx, |workspace, cx| workspace.toggle_outline_item(paragraph_ix, cx));
                }
              }
            });
            let label_action: SidebarTreeAction = {
              let workspace = workspace.clone();
              Rc::new(move |window: &mut Window, cx: &mut App| {
                if let Some(paragraph_ix) = paragraph_ix {
                  let _ = workspace.update(cx, |workspace, cx| workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx));
                }
              })
            };
            let outline_level = paragraph_ix.and_then(|ix| outline_levels.get(&ix).copied()).unwrap_or(3);
            let context_menu_action: Option<ContextMenuAction> = Some(Rc::new({
              let workspace = workspace.clone();
              move |position, window, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                  workspace.show_outline_context_menu(outline_level, position, window, cx);
                });
              }
            }));
            render_sidebar_tree_row(
              SidebarTreeRow {
                row_id: ("outline-tree-item", ix),
                toggle_id: ("outline-toggle", ix),
                label_id: ("outline-label", ix),
                label: entry.item().label.clone(),
                nav_width,
                depth,
                is_folder,
                is_expanded,
                is_active: is_active_outline,
                has_search_match,
                guide,
                icon: None,
                icon_color: None,
                toggle_action: Some(toggle_action),
                label_action,
                stop_icon_mouse_down: true,
                stop_label_mouse_down: true,
                context_menu_action,
              },
              window,
              cx,
            )
          })),
      )
      .into_any_element()
  }

  fn search_match_outline_paragraphs(&self, cx: &App) -> HashSet<usize> {
    let Some(active_document_id) = self.active_document_id else {
      return HashSet::new();
    };
    let Some(cache) = self.outline_cache.as_ref() else {
      return HashSet::new();
    };
    let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == active_document_id)
    else {
      return HashSet::new();
    };

    let mut outline_paragraphs = HashSet::new();
    let panel = panel.read(cx);
    for paragraph_ix in panel.search_match_paragraphs() {
      if let Some(outline_paragraph) = active_visible_outline_paragraph_from_visible(&cache.visible_paragraphs, paragraph_ix) {
        outline_paragraphs.insert(outline_paragraph);
      }
    }
    outline_paragraphs
  }

  fn render_flow_nav(&mut self, cx: &mut Context<Self>) -> AnyElement {
    v_flex()
      .size_full()
      .h_full()
      .p_2()
      .bg(cx.theme().sidebar)
      .text_color(cx.theme().sidebar_foreground)
      .child(
        div()
          .w_full()
          .h_full()
          .flex()
          .items_center()
          .justify_center()
          .text_sm()
          .child("Flow"),
      )
      .into_any_element()
  }
}

type SidebarTreeAction = Rc<dyn Fn(&mut Window, &mut App)>;
type ContextMenuAction = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;

struct SidebarTreeRow {
  row_id: (&'static str, usize),
  toggle_id: (&'static str, usize),
  label_id: (&'static str, usize),
  label: SharedString,
  nav_width: Pixels,
  depth: usize,
  is_folder: bool,
  is_expanded: bool,
  is_active: bool,
  has_search_match: bool,
  guide: OutlineRowGuides,
  icon: Option<IconName>,
  icon_color: Option<Hsla>,
  toggle_action: Option<SidebarTreeAction>,
  label_action: SidebarTreeAction,
  stop_icon_mouse_down: bool,
  stop_label_mouse_down: bool,
  context_menu_action: Option<ContextMenuAction>,
}

fn render_sidebar_tree_row(row: SidebarTreeRow, window: &mut Window, cx: &mut App) -> ListItem {
  let hierarchy_color = outline_hierarchy_color(row.depth, cx);
  let guide_depths = row.guide.ancestor_depths;
  let label_width = outline_label_width(row.nav_width, row.depth);
  let label = truncate_outline_label(row.label.as_ref(), outline_label_text_width(label_width, window), window, cx);
  let icon_color = row.icon_color.unwrap_or(hierarchy_color);
  let icon = row.icon.clone();
  let has_icon = icon.is_some();
  let stop_icon_mouse_down = row.stop_icon_mouse_down;
  let stop_label_mouse_down = row.stop_label_mouse_down;
  let label_action = row.label_action.clone();
  let search_highlight_color = cx.theme().warning.opacity(0.22);
  let icon_action = if has_icon {
    if row.is_folder {
      row.toggle_action.clone()
    } else {
      Some(label_action.clone())
    }
  } else {
    None
  };

  ListItem::new(row.row_id)
    .w_full()
    .min_w_0()
    .overflow_hidden()
    .pl(px(4.0))
    .pr_1()
    .py_0()
    .text_xs()
    .child(
      h_flex()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .items_center()
        .gap_1()
        .children((0..row.depth).map(|guide_depth| {
          let has_guide = guide_depths.contains(&guide_depth);
          let guide_color = outline_hierarchy_color(guide_depth, cx);
          div()
            .relative()
            .w(px(12.0))
            .h(px(20.0))
            .flex_none()
            .when(has_guide, |this| {
              this.child(
                div()
                  .absolute()
                  .top_0()
                  .bottom_0()
                  .left(px(11.5))
                  .w(px(0.5))
                  .bg(guide_color.opacity(0.68)),
              )
            })
            .into_any_element()
        }))
        .child(
          div()
            .relative()
            .w(px(20.0))
            .h(px(20.0))
            .flex_none()
            .when(row.guide.extends_from_toggle, |this| {
              this.child(
                div()
                  .absolute()
                  .top(if row.is_folder { px(18.0) } else { px(0.0) })
                  .bottom_0()
                  .left(px(11.5))
                  .w(px(0.5))
                  .bg(hierarchy_color.opacity(0.68)),
              )
            })
            .when(!has_icon && row.is_folder, |this| {
              let icon_path = if row.is_expanded {
                "icons/caret-down.svg"
              } else {
                "icons/caret-right.svg"
              };
              this.child(
                Button::new(row.toggle_id)
                  .xsmall()
                  .ghost()
                  .absolute()
                  .top_0()
                  .left_0()
                  .disabled(!row.is_folder)
                  .child(
                    Icon::default()
                      .path(icon_path)
                      .with_size(gpui_component::Size::Small)
                      .text_color(hierarchy_color),
                  )
                  .on_click({
                    let toggle_action = row.toggle_action.clone();
                    move |_, window, cx| {
                      cx.stop_propagation();
                      if let Some(action) = &toggle_action {
                        action(window, cx);
                      }
                    }
                  }),
              )
            })
            .when_some(icon, |this, icon| {
              this.child(
                div()
                  .absolute()
                  .top_0()
                  .left(px(1.5))
                  .w(px(20.0))
                  .h(px(20.0))
                  .flex()
                  .items_center()
                  .justify_center()
                  .child(Icon::new(icon).xsmall().text_color(icon_color))
                  .on_mouse_down(MouseButton::Left, {
                    let icon_action = icon_action.clone();
                    move |_, window, cx| {
                      if let Some(action) = &icon_action {
                        action(window, cx);
                      }
                      if stop_icon_mouse_down {
                        cx.stop_propagation();
                      }
                    }
                  }),
              )
            }),
        )
        .child(
          div()
            .id(row.label_id)
            .relative()
            .flex_1()
            .min_w_0()
            .px_1()
            .overflow_hidden()
            .text_color(if row.is_active {
              cx.theme().sidebar_accent_foreground
            } else {
              hierarchy_color
            })
            .whitespace_nowrap()
            .rounded(cx.theme().radius)
            .when(row.has_search_match, |this| {
              this.child(
                div()
                  .absolute()
                  .top_0()
                  .left_0()
                  .right_0()
                  .bottom_0()
                  .bg(search_highlight_color)
                  .rounded(cx.theme().radius),
              )
            })
            .when(row.is_active, |this| {
              this.child(
                div()
                  .absolute()
                  .top_0()
                  .left_0()
                  .right_0()
                  .bottom_0()
                  .bg(
                    cx.theme()
                      .sidebar_accent
                      .opacity(if row.has_search_match { 0.55 } else { 1.0 }),
                  )
                  .border_1()
                  .border_color(hierarchy_color)
                  .rounded(cx.theme().radius),
              )
            })
            .when(!row.is_active && !row.has_search_match, |this| {
              this.hover(|style| style.bg(cx.theme().list_hover))
            })
            .child(label)
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
              if stop_label_mouse_down {
                cx.stop_propagation();
              }
            })
            .when_some(row.context_menu_action.clone(), |this, action| {
              this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                cx.stop_propagation();
                action(event.position, window, cx);
              })
            })
            .on_click({
              move |_, window, cx| {
                label_action(window, cx);
              }
            }),
        ),
    )
}



#[hotpath::measure]
fn outline_hierarchy_color(depth: usize, cx: &App) -> Hsla {
  let anchor = cx.theme().link.mix(cx.theme().foreground, 0.72);
  match depth % 5 {
    0 => anchor,
    1 => anchor.mix(cx.theme().primary, 0.72),
    2 => anchor.mix(cx.theme().info, 0.72),
    3 => anchor.mix(cx.theme().accent_foreground, 0.76),
    _ => anchor.mix(cx.theme().foreground, 0.82),
  }
}


