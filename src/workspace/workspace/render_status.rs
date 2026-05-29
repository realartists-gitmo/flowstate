impl Workspace {
  fn next_untitled_title(&self, cx: &App) -> String {
    let used = self
      .document_panels
      .iter()
      .filter_map(|panel| untitled_index(panel.read(cx).title_text().as_ref()))
      .collect::<HashSet<_>>();
    let mut index = 1usize;
    while used.contains(&index) {
      index += 1;
    }
    format!("Untitled{index}.db8")
  }

  fn next_untitled_flow_title(&self, cx: &App) -> String {
    let used = self
      .flow_panels
      .iter()
      .filter_map(|panel| untitled_flow_index(panel.read(cx).title_text().as_ref()))
      .collect::<HashSet<_>>();
    let mut index = 1usize;
    while used.contains(&index) {
      index += 1;
    }
    format!("Untitled{index}.fl0")
  }

  fn render_document_tab_bar_prefix(&self, active_index: usize, tab_count: usize, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    h_flex()
      .h_full()
      .items_center()
      .gap_1()
      .px_1()
      .child(
        icon_button("tab-bar-new-file", AppIcon::NewFile)
          .tooltip("New")
          .dropdown_menu(move |menu, _, _| {
            menu
              .item(file_menu_item(workspace.clone(), "Doc", false, |workspace, window, cx| {
                workspace.new_document(window, cx);
              }))
              .item(file_menu_item(workspace.clone(), "Flow", false, |workspace, window, cx| {
                workspace.new_flow(window, cx);
              }))
          }),
      )
      .child(
        icon_button("tab-bar-save-file", AppIcon::SaveFile)
          .tooltip("Save current file")
          .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.save_active(window, cx);
          })),
      )
      .child(
        icon_button("tab-bar-navigate-left", AppIcon::TabLeft)
          .tooltip("Navigate tab left")
          .disabled(active_index == 0)
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.navigate_active_tab(-1, cx);
          })),
      )
      .child(
        icon_button("tab-bar-navigate-right", AppIcon::TabRight)
          .tooltip("Navigate tab right")
          .disabled(active_index + 1 >= tab_count)
          .on_click(cx.listener(|workspace, _, _, cx| {
            workspace.navigate_active_tab(1, cx);
          })),
      )
  }

  fn render_document_tab_bar_suffix(&self) -> impl IntoElement {
    h_flex().h_full().items_center().gap_1().px_1().child(
      icon_button("tab-bar-multipanel-placeholder", AppIcon::MultiPanel)
        .tooltip("Multi-panel layout")
        .disabled(true),
    )
  }

  fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
    // These buttons call command methods directly for now. When command
    // dispatch grows beyond direct callbacks, keep the buttons mapped to
    // `CommandId::NewDocument` and `CommandId::OpenDemoDocument`.
    let new_doc = cx.listener(|workspace, _, window, cx| workspace.new_document(window, cx));
    let new_flow = cx.listener(|workspace, _, window, cx| workspace.new_flow(window, cx));
    let open_document = cx.listener(|workspace, _, window, cx| workspace.prompt_open_document(window, cx));
    let open_search = cx.listener(|workspace, _, window, cx| workspace.open_file_search_overlay(window, cx));
    v_flex()
      .size_full()
      .items_center()
      .justify_center()
      .gap_3()
      .bg(cx.theme().background)
      .child(
        div()
          .text_xl()
          .font_weight(gpui::FontWeight::SEMIBOLD)
          .text_color(cx.theme().foreground)
          .child("No document open"),
      )
      .child(
        h_flex()
          .gap_2()
          .child(
            Button::new("empty-new-document")
              .icon(Icon::new(IconName::Plus).text_color(cx.theme().primary_foreground))
              .label("New Doc")
              .primary()
              .on_click(new_doc),
          )
          .child(
            Button::new("empty-new-flow")
              .icon(Icon::new(IconName::Plus).text_color(cx.theme().primary_foreground))
              .label("New Flow")
              .primary()
              .on_click(new_flow),
          )
          .child(
            Button::new("empty-open-document")
              .icon(Icon::new(IconName::FolderOpen).text_color(cx.theme().secondary_foreground))
              .label("Open")
              .on_click(open_document),
          )
          .child(
            Button::new("empty-search-document")
              .icon(Icon::new(IconName::Search).text_color(cx.theme().secondary_foreground))
              .label("Search")
              .on_click(open_search),
          ),
      )
  }

  fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
    h_flex()
      .h(px(26.0))
      .w_full()
      .items_center()
      .px_2()
      .border_t(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .bg(cx.theme().background)
      .child(
        div()
          .text_xs()
          .text_color(cx.theme().muted_foreground)
          .child("Bottom bar placeholder"),
      )
  }
}
