#[hotpath::measure_all]
impl Workspace {
  fn render_left_nav(&mut self, nav_width: Pixels, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
    if self.left_nav_mode == LeftNavMode::Tub {
      return self.render_tub_nav(nav_width, cx);
    }
    if self.active_flow.is_some() {
      return self.render_flow_nav(nav_width, window, cx);
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
                hierarchy_color: None,
                guide_colors: None,
                incoming_branch: None,
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

  fn render_flow_nav(&mut self, nav_width: Pixels, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
    let Some(editor) = self.active_flow.clone() else {
      return div().into_any_element();
    };
    let projection = editor.read(cx).document().projection().clone();
    let active_sheet = editor.read(cx).active_sheet();
    let active_cell = editor.read(cx).active_cell();
    let mut rows = Vec::new();
    for sheet in &projection.sheets {
      let sheet_id = sheet.id;
      let sheet_expanded = editor.read(cx).outline_item_expanded(sheet_id);
      let toggle_editor = editor.clone();
      let activate_editor = editor.clone();
      let key = sheet_id.as_u128() as usize;
      rows.push(
        render_sidebar_tree_row(
          SidebarTreeRow {
            row_id: ("flow-outline-sheet", key),
            toggle_id: ("flow-outline-sheet-toggle", key),
            label_id: ("flow-outline-sheet-label", key),
            label: sheet.name.clone().into(),
            nav_width,
            depth: 0,
            is_folder: !sheet.cells.is_empty(),
            is_expanded: sheet_expanded,
            is_active: active_sheet == Some(sheet_id) && active_cell.is_none(),
            has_search_match: false,
            guide: OutlineRowGuides::default(),
            hierarchy_color: None,
            guide_colors: None,
            incoming_branch: None,
            icon: None,
            icon_color: None,
            toggle_action: Some(Rc::new(move |_, cx| {
              toggle_editor.update(cx, |editor, cx| editor.toggle_outline_item(sheet_id, cx));
            })),
            label_action: Rc::new(move |_, cx| activate_editor.update(cx, |editor, cx| editor.activate_sheet(sheet_id, cx))),
            stop_icon_mouse_down: true,
            stop_label_mouse_down: true,
            context_menu_action: None,
          },
          window,
          cx,
        )
        .into_any_element(),
      );
      if !sheet_expanded {
        continue;
      }
      let Some(definition) = projection.format.sheet_type(sheet.sheet_type_id) else {
        continue;
      };
      let roots: Vec<_> = sheet.cells.iter().filter(|cell| cell.parent_id.is_none()).map(|cell| cell.id).collect();
      for cell_id in roots {
        append_flow_outline_cell_rows(
          sheet,
          definition,
          cell_id,
          &editor,
          active_cell,
          nav_width,
          window,
          &[0],
          true,
          cx,
          &mut rows,
        );
      }
    }
    v_flex()
      .size_full()
      .h_full()
      .p_2()
      .bg(cx.theme().sidebar)
      .text_color(cx.theme().sidebar_foreground)
      .child(self.render_left_nav_header("Flow", cx))
      .child(div().flex_1().w_full().overflow_y_scrollbar().children(rows))
      .into_any_element()
  }
}

fn append_flow_outline_cell_rows(
  sheet: &flowstate_flow::Sheet,
  definition: &flowstate_flow::SheetTypeDefinition,
  cell_id: flowstate_flow::CellId,
  editor: &Entity<FlowEditor>,
  active_cell: Option<flowstate_flow::CellId>,
  nav_width: Pixels,
  window: &mut Window,
  ancestor_depths: &[usize],
  is_final_child: bool,
  cx: &mut Context<Workspace>,
  rows: &mut Vec<AnyElement>,
) {
  let Some(cell) = sheet.cells.iter().find(|cell| cell.id == cell_id) else {
    return;
  };
  let Some(column_depth) = definition.columns.iter().position(|column| column.id == cell.column_id) else {
    return;
  };
  let depth = column_depth + 1;
  let children: Vec<_> = sheet
    .cells
    .iter()
    .filter(|candidate| candidate.parent_id == Some(cell_id))
    .map(|candidate| candidate.id)
    .collect();
  let expanded = editor.read(cx).outline_item_expanded(cell_id);
  let side_color = match definition.columns[column_depth].side {
    flowstate_flow::ArgumentSide::One => cx.theme().primary,
    flowstate_flow::ArgumentSide::Two => cx.theme().info,
  };
  let guide_colors = (0..depth)
    .map(|guide_depth| {
      if guide_depth == 0 {
        outline_hierarchy_color(0, cx)
      } else {
        match definition.columns[guide_depth - 1].side {
          flowstate_flow::ArgumentSide::One => cx.theme().primary,
          flowstate_flow::ArgumentSide::Two => cx.theme().info,
        }
      }
    })
    .collect();
  let label = cell
    .summary_text()
    .map(|text| if text.trim().is_empty() { "(empty)".into() } else { text })
    .unwrap_or_else(|_| "(invalid rich text)".into());
  let activate_editor = editor.clone();
  let toggle_editor = editor.clone();
  let incoming_branch = cell.parent_id.map(|_| IncomingBranch {
    parent_depth: depth.saturating_sub(1),
    is_final_child,
  });
  let guide = OutlineRowGuides {
    ancestor_depths: ancestor_depths.to_vec(),
    // Flow branches are rendered in their parent's indentation slot. The
    // document-outline toggle extension uses a different coordinate and would
    // leave a short duplicate segment below every expanded flow parent.
    extends_from_toggle: false,
  };
  let key = cell_id.as_u128() as usize;
  rows.push(
    render_sidebar_tree_row(
      SidebarTreeRow {
        row_id: ("flow-outline-cell", key),
        toggle_id: ("flow-outline-toggle", key),
        label_id: ("flow-outline-label", key),
        label: label.into(),
        nav_width,
        depth,
        is_folder: !children.is_empty(),
        is_expanded: expanded,
        is_active: active_cell == Some(cell_id),
        has_search_match: false,
        guide,
        hierarchy_color: Some(side_color),
        guide_colors: Some(guide_colors),
        incoming_branch,
        icon: None,
        icon_color: None,
        toggle_action: Some(Rc::new(move |_, cx| {
          toggle_editor.update(cx, |editor, cx| editor.toggle_outline_item(cell_id, cx));
        })),
        label_action: Rc::new(move |_, cx| activate_editor.update(cx, |editor, cx| editor.activate_cell(cell_id, cx))),
        stop_icon_mouse_down: true,
        stop_label_mouse_down: true,
        context_menu_action: None,
      },
      window,
      cx,
    )
    .into_any_element(),
  );
  if expanded {
    let mut child_ancestors = ancestor_depths.to_vec();
    if !is_final_child {
      child_ancestors.push(depth.saturating_sub(1));
    }
    for (index, child_id) in children.iter().copied().enumerate() {
      append_flow_outline_cell_rows(
        sheet,
        definition,
        child_id,
        editor,
        active_cell,
        nav_width,
        window,
        &child_ancestors,
        index + 1 == children.len(),
        cx,
        rows,
      );
    }
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
  hierarchy_color: Option<Hsla>,
  guide_colors: Option<Vec<Hsla>>,
  incoming_branch: Option<IncomingBranch>,
  icon: Option<IconName>,
  icon_color: Option<Hsla>,
  toggle_action: Option<SidebarTreeAction>,
  label_action: SidebarTreeAction,
  stop_icon_mouse_down: bool,
  stop_label_mouse_down: bool,
  context_menu_action: Option<ContextMenuAction>,
}

#[derive(Clone, Copy)]
struct IncomingBranch {
  parent_depth: usize,
  is_final_child: bool,
}

fn render_sidebar_tree_row(row: SidebarTreeRow, window: &mut Window, cx: &mut App) -> ListItem {
  let hierarchy_color = row.hierarchy_color.unwrap_or_else(|| outline_hierarchy_color(row.depth, cx));
  let guide_colors = row.guide_colors;
  let guide_depths = row.guide.ancestor_depths;
  let incoming_branch = row.incoming_branch;
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
          let incoming_branch = incoming_branch.filter(|branch| branch.parent_depth == guide_depth);
          let guide_color = guide_colors
            .as_ref()
            .and_then(|colors| colors.get(guide_depth))
            .copied()
            .unwrap_or_else(|| outline_hierarchy_color(guide_depth, cx));
          div()
            .relative()
            .w(px(12.0))
            .h(px(20.0))
            .flex_none()
            .when(has_guide && incoming_branch.is_none(), |this| {
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
            .when_some(incoming_branch, |this, branch| {
              if branch.is_final_child {
                this.child(
                  div()
                    .absolute()
                    .top_0()
                    .left(px(11.0))
                    .w(px(9.0))
                    .h(px(10.5))
                    .border_l_1()
                    .border_b_1()
                    .border_color(guide_color.opacity(0.68))
                    .rounded_bl(cx.theme().radius.min(px(10.0))),
                )
              } else {
                this
                  .child(
                    div()
                      .absolute()
                      .top_0()
                      .bottom_0()
                      .left(px(11.5))
                      .w(px(0.5))
                      .bg(guide_color.opacity(0.68)),
                  )
                  .child(
                    div()
                      .absolute()
                      .top(px(10.0))
                      .left(px(11.5))
                      .w(px(8.5))
                      .h(px(0.5))
                      .bg(guide_color.opacity(0.68)),
                  )
              }
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
                  .when(incoming_branch.is_none(), |button| button.ghost())
                  .when(incoming_branch.is_some(), |button| {
                    button.custom(ButtonCustomVariant::new(cx).foreground(hierarchy_color))
                  })
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


