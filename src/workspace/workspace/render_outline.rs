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
            let depth = entry.depth();
            let guide = outline_guides.get(ix).cloned().unwrap_or_default();
            let workspace = workspace.clone();
            let toggle_action: SidebarTreeAction = Rc::new({
              let workspace = workspace.clone();
              move |_: &mut Window, cx: &mut App| {
                if let Some(paragraph_ix) = paragraph_ix {
                  let _ = workspace.update(cx, |workspace, cx| workspace.toggle_outline_item(paragraph_ix, cx));
                }
              }
            });
            let label_action: SidebarTreeAction = Rc::new(move |window: &mut Window, cx: &mut App| {
              if let Some(paragraph_ix) = paragraph_ix {
                let _ = workspace.update(cx, |workspace, cx| workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx));
              }
            });
            render_sidebar_tree_row(SidebarTreeRow {
              row_id: ("outline-tree-item", ix),
              toggle_id: ("outline-toggle", ix),
              label_id: ("outline-label", ix),
              label: entry.item().label.clone(),
              nav_width,
              depth,
              is_folder,
              is_expanded,
              is_active: is_active_outline,
              guide,
              icon: None,
              icon_color: None,
              toggle_action: Some(toggle_action),
              label_action,
              stop_icon_mouse_down: true,
              stop_label_mouse_down: true,
            }, window, cx)
          })),
      )
      .into_any_element()
  }

  fn render_flow_nav(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let workspace = cx.entity().downgrade();
    let items = self
      .active_flow
      .as_ref()
      .map(|editor| editor.read(cx).outline_items())
      .unwrap_or_default();
    let len = items.len();

    v_flex()
      .size_full()
      .h_full()
      .gap_1()
      .p_2()
      .bg(cx.theme().sidebar)
      .text_color(cx.theme().sidebar_foreground)
      .child(
        div()
          .w_full()
          .flex()
          .flex_row()
          .items_center()
          .justify_between()
          .child(
            div()
              .text_sm()
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .text_color(cx.theme().sidebar_primary)
              .child("Flows"),
          )
          .child(
            Button::new("collapse-flow-outline-panel")
              .icon(Icon::new(IconName::PanelLeftClose).text_color(cx.theme().sidebar_foreground))
              .xsmall()
              .ghost()
              .tooltip("Collapse outline")
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.toggle_outline(cx);
              })),
          ),
      )
      .child(
        v_flex()
          .flex_1()
          .w_full()
          .gap_1()
          .overflow_y_scrollbar()
          .children(items.into_iter().map(|item| {
            let flow_id = item.id.clone();
            let label = item.label.clone();
            let selected = item.selected;
            let source_index = item.index;
            let target_index = item.index;
            let colors = crate::flow::editor::affirmative_flow_colors(cx);
            let workspace_for_click = workspace.clone();
            div()
              .id(("flow-outline-drop-row", source_index))
              .w_full()
              .on_drag(
                FlowOutlineDrag {
                  flow_id: flow_id.clone(),
                  label: label.clone(),
                  source_index,
                },
                |drag, _, _, cx| {
                  cx.stop_propagation();
                  cx.new(|_| drag.clone())
                },
              )
              .drag_over::<FlowOutlineDrag>(|this, _, _, cx| {
                this.border_t_2().border_color(cx.theme().drag_border)
              })
              .on_drop(cx.listener(move |workspace, drag: &FlowOutlineDrag, window, cx| {
                let new_index = flow_drop_index(drag.source_index, target_index);
                if let Some(editor) = workspace.active_flow.clone() {
                  editor.update(cx, |editor, cx| editor.move_flow_to_index(drag.flow_id.clone(), new_index, window, cx));
                }
                cx.notify();
              }))
              .child(
                ListItem::new(("flow-outline-item", source_index))
                  .selected(selected)
                  .rounded(cx.theme().radius)
                  .on_click(move |_, window, cx| {
                    let _ = workspace_for_click.update(cx, |workspace, cx| {
                      if let Some(editor) = workspace.active_flow.clone() {
                        editor.update(cx, |editor, cx| editor.select_flow(flow_id.clone(), window, cx));
                      }
                    });
                  })
                  .child(
                    h_flex()
                      .w_full()
                      .min_w_0()
                      .items_center()
                      .gap_2()
                      .child(div().w(px(3.0)).h(px(20.0)).rounded(px(2.0)).bg(colors.border))
                      .child(
                        div()
                          .flex_1()
                          .min_w_0()
                          .text_xs()
                          .truncate()
                          .text_color(if selected { cx.theme().sidebar_foreground } else { cx.theme().muted_foreground })
                          .child(label),
                      )
                      .child(Icon::new(IconName::Menu).xsmall().text_color(cx.theme().muted_foreground)),
                  ),
              )
              .into_any_element()
          }))
          .child(
            div()
              .id("flow-outline-drop-end")
              .h(px(22.0))
              .rounded(cx.theme().radius)
              .drag_over::<FlowOutlineDrag>(|this, _, _, cx| {
                this.border_1().border_color(cx.theme().drag_border)
              })
              .on_drop(cx.listener(move |workspace, drag: &FlowOutlineDrag, window, cx| {
                if len > 0
                  && let Some(editor) = workspace.active_flow.clone()
                {
                  editor.update(cx, |editor, cx| editor.move_flow_to_index(drag.flow_id.clone(), usize::MAX, window, cx));
                  cx.notify();
                }
              })),
          ),
      )
      .into_any_element()
  }

}

type SidebarTreeAction = Rc<dyn Fn(&mut Window, &mut App)>;

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
  guide: OutlineRowGuides,
  icon: Option<IconName>,
  icon_color: Option<Hsla>,
  toggle_action: Option<SidebarTreeAction>,
  label_action: SidebarTreeAction,
  stop_icon_mouse_down: bool,
  stop_label_mouse_down: bool,
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
            .text_color(if row.is_active { cx.theme().sidebar_accent_foreground } else { hierarchy_color })
            .whitespace_nowrap()
            .rounded(cx.theme().radius)
            .when(row.is_active, |this| {
              this.child(
                div()
                  .absolute()
                  .top_0()
                  .left_0()
                  .right_0()
                  .bottom_0()
                  .bg(cx.theme().sidebar_accent)
                  .border_1()
                  .border_color(hierarchy_color)
                  .rounded(cx.theme().radius),
              )
            })
            .when(!row.is_active, |this| this.hover(|style| style.bg(cx.theme().list_hover)))
            .child(label)
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
              if stop_label_mouse_down {
                cx.stop_propagation();
              }
            })
            .on_click({
              move |_, window, cx| {
                label_action(window, cx);
              }
            }),
        ),
    )
}

#[derive(Clone)]
struct FlowOutlineDrag {
  flow_id: String,
  label: String,
  source_index: usize,
}

#[hotpath::measure_all]
impl Render for FlowOutlineDrag {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .id("flow-outline-drag")
      .w(px(180.0))
      .items_center()
      .gap_2()
      .rounded(cx.theme().radius)
      .border_1()
      .border_color(cx.theme().drag_border)
      .bg(cx.theme().popover.opacity(0.92))
      .px_2()
      .py_1()
      .text_xs()
      .text_color(cx.theme().popover_foreground)
      .child(Icon::new(IconName::Menu).xsmall())
      .child(div().flex_1().truncate().child(self.label.clone()))
  }
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

#[hotpath::measure]
fn flow_drop_index(source_index: usize, target_index: usize) -> usize {
  if source_index < target_index {
    target_index.saturating_sub(1)
  } else {
    target_index
  }
}
