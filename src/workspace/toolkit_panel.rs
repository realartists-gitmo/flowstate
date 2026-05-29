use gpui::{Context, IntoElement, Pixels, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::{ActiveTheme as _, IconName, Sizable, v_flex};

use super::{APP_CHROME_BORDER_WIDTH, SIDE_PANEL_COLLAPSED_WIDTH, Workspace};

impl Workspace {
  /// Renders the main editor area and the right-side Toolkit panel as one
  /// resizable horizontal split. Keeping this next to `render_toolkit` makes
  /// the panel's width, collapse state, and visible contents easier to evolve.
  pub(super) fn render_content_area(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
    let toolkit_width = if self.toolkit_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH
    } else {
      px(40.0)
    };
    let toolkit_range_end = if self.toolkit_collapsed {
      SIDE_PANEL_COLLAPSED_WIDTH
    } else {
      px(40.0)
    };

    h_resizable("workspace-content-resizable")
      .with_state(&self.content_resizable_state)
      .child(
        resizable_panel()
          .size(px(560.0))
          .size_range(px(360.0)..Pixels::MAX)
          .child(self.render_document_pane(cx)),
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
            self.render_toolkit_icon_bar(cx).into_any_element()
          }),
      )
  }

  fn render_toolkit_icon_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let open_file_search = cx.listener(|workspace, _, window, cx| workspace.open_file_search_overlay(window, cx));

    v_flex()
      .size_full()
      .h_full()
      .items_center()
      .gap_1()
      .py_2()
      .bg(cx.theme().background)
      .border_l(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .child(
        Button::new("toolkit-global-db8-search")
          .icon(IconName::Search)
          .xsmall()
          .ghost()
          .tooltip("Find DB8 file")
          .on_click(open_file_search),
      )
  }

  #[allow(dead_code)]
  fn render_toolkit_expanded(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let open_file_search = cx.listener(|workspace, _, window, cx| workspace.open_file_search_overlay(window, cx));
    v_flex()
      .size_full()
      .h_full()
      .gap_2()
      .p_3()
      .bg(cx.theme().background)
      .border_l(APP_CHROME_BORDER_WIDTH)
      .border_color(cx.theme().border)
      .child(
        div()
          .w_full()
          .flex()
          .flex_row()
          .items_center()
          .justify_between()
          .child(
            Button::new("collapse-toolkit-panel")
              .icon(IconName::PanelRightClose)
              .xsmall()
              .ghost()
              .tooltip("Collapse toolkit")
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.toggle_toolkit(cx);
              })),
          )
          .child(
            div()
              .text_sm()
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .child("Toolkit"),
          ),
      )
      // Both of these need moving. Toolkit is for rendering live views of elements from other files. Todo:
      .child(
        Button::new("toolkit-global-db8-search")
          .icon(IconName::Search)
          .label("Find DB8 File")
          .small()
          .on_click(open_file_search),
      )
      .child(
        div()
          .text_sm()
          .text_color(cx.theme().muted_foreground)
          .child("Search, file tools, and document utilities will live here."),
      )
  }
}
