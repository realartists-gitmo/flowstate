use flowstate_flow::{DebateStyleKey, FormatKind, all_debate_style_templates};
use gpui::{
  AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Subscription,
  Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants, Toggle};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::{ActiveTheme as _, Disableable, Icon, IconName, PixelsExt, Selectable, Sizable, StyledExt, h_flex, v_flex};

use crate::flow::FlowEditor;

type DebateStyleSelectDelegate = SearchableVec<SharedString>;

pub struct FlowRibbon {
  editor: Entity<FlowEditor>,
  focus_handle: FocusHandle,
  height: gpui::Pixels,
  style_select: Entity<SelectState<DebateStyleSelectDelegate>>,
  title_input: Entity<InputState>,
  syncing_title: bool,
  _subscriptions: Vec<Subscription>,
}

#[hotpath::measure_all]
impl FlowRibbon {
  pub fn new(editor: Entity<FlowEditor>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let style_labels = all_debate_style_templates()
      .iter()
      .map(|template| SharedString::from(template.label))
      .collect::<Vec<_>>();
    let style_select = cx.new(|cx| {
      let mut select = SelectState::new(SearchableVec::new(style_labels), None, window, cx).searchable(true);
      select.set_selected_value(&SharedString::from(editor.read(cx).selected_style_label()), window, cx);
      select
    });
    let title_input = cx.new(|cx| {
      InputState::new(window, cx)
        .placeholder("flow name")
        .default_value(editor.read(cx).selected_flow_title())
    });
    let style_editor = editor.clone();
    let style_subscription = cx.subscribe_in(
      &style_select,
      window,
      move |_, _, event: &SelectEvent<DebateStyleSelectDelegate>, _, cx| {
        if let SelectEvent::Confirm(Some(label)) = event
          && let Some(style) = style_key_for_label(label)
        {
          style_editor.update(cx, |editor, cx| editor.set_selected_style(style, cx));
        }
      },
    );
    let title_editor = editor.clone();
    let title_subscription = cx.subscribe(&title_input, move |ribbon, input, event: &InputEvent, cx| match event {
      InputEvent::Change => {
        if ribbon.syncing_title {
          return;
        }
        let value = input.read(cx).value().to_string();
        title_editor.update(cx, |editor, cx| editor.set_selected_flow_title(value, cx));
      },
      InputEvent::Blur => {
        title_editor.update(cx, |editor, cx| editor.resolve_pending(cx));
      },
      InputEvent::Focus | InputEvent::PressEnter { .. } | InputEvent::BackspaceEmpty | InputEvent::DeleteEmpty => {},
    });

    Self {
      editor,
      focus_handle: cx.focus_handle(),
      height: default_flow_ribbon_height(),
      style_select,
      title_input,
      syncing_title: false,
      _subscriptions: vec![style_subscription, title_subscription],
    }
  }

  pub fn set_height(&mut self, height: gpui::Pixels, cx: &mut Context<Self>) {
    if self.height != height {
      self.height = height;
      cx.notify();
    }
  }

  fn sync_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let style_label = SharedString::from(self.editor.read(cx).selected_style_label());
    if self
      .style_select
      .read(cx)
      .selected_value()
      .is_none_or(|selected| selected != &style_label)
    {
      self
        .style_select
        .update(cx, |select, cx| select.set_selected_value(&style_label, window, cx));
    }

    let title = self.editor.read(cx).selected_flow_title();
    if self.title_input.read(cx).value().as_ref() != title {
      self.syncing_title = true;
      self
        .title_input
        .update(cx, |input, cx| input.set_value(title, window, cx));
      self.syncing_title = false;
    }
  }

  fn render_setup_group(&mut self, metrics: FlowRibbonMetrics, cx: &mut Context<Self>) -> impl IntoElement {
    let editor = self.editor.clone();
    let selected_style = editor.read(cx).selected_style();
    let ld_toc = editor.read(cx).ld_toc_circuit();
    let has_switch = editor.read(cx).has_switchable_templates();
    let switch_speakers = editor.read(cx).switch_speakers();
    let templates = editor.read(cx).templates();
    let can_write = editor.read(cx).can_write_collaboration();

    let mut controls = Vec::<AnyElement>::new();
    controls.push(
      div()
        .w(px(164.0))
        .child(
          Select::new(&self.style_select)
            .placeholder("Debate style")
            .search_placeholder("Search styles")
            .w_full()
            .disabled(!can_write),
        )
        .into_any_element(),
    );
    if selected_style == DebateStyleKey::LincolnDouglas {
      controls.push(
        Toggle::new("flow-ribbon-ld-toc")
          .label("TOC")
          .xsmall()
          .checked(ld_toc)
          .disabled(!can_write)
          .on_click({
            let editor = editor.clone();
            move |_, _, cx| {
              editor.update(cx, |editor, cx| editor.toggle_ld_toc_circuit(cx));
            }
          })
          .into_any_element(),
      );
    }
    if has_switch {
      controls.push(
        Toggle::new("flow-ribbon-switch-speakers")
          .label("Switch")
          .xsmall()
          .checked(switch_speakers)
          .disabled(!can_write)
          .on_click({
            let editor = editor.clone();
            move |_, _, cx| {
              editor.update(cx, |editor, cx| editor.toggle_switch_speakers(cx));
            }
          })
          .into_any_element(),
      );
    }
    controls.extend(templates.into_iter().enumerate().map(|(ix, template)| {
      let label = template.name.clone();
      flow_chip(("flow-ribbon-add-template", ix), metrics, cx)
        .icon(IconName::Plus)
        .label(label)
        .tooltip(if can_write { "Add flow" } else { "Viewers cannot edit flows" })
        .disabled(!can_write)
        .on_click({
          let editor = editor.clone();
          move |_, window, cx| {
            editor.update(cx, |editor, cx| editor.add_flow(template.clone(), window, cx));
          }
        })
        .into_any_element()
    }));
    flow_ribbon_group("Setup", controls, metrics, cx)
  }

  fn render_title_group(&mut self, metrics: FlowRibbonMetrics, cx: &mut Context<Self>) -> impl IntoElement {
    let state = self.editor.read(cx).command_state();
    flow_ribbon_group(
      "Flow",
      vec![
        div()
          .w(px(220.0))
          .child(
            Input::new(&self.title_input)
              .appearance(false)
              .bordered(true)
              .focus_bordered(true)
              .w_full()
              .disabled(!state.can_write),
          )
          .into_any_element(),
        flow_chip("flow-ribbon-delete-flow", metrics, cx)
          .icon(IconName::Delete)
          .danger()
          .tooltip("Delete flow")
          .disabled(!state.can_write || !state.has_flow)
          .on_click({
            let editor = self.editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.delete_selected_flow(window, cx));
            }
          })
          .into_any_element(),
      ],
      metrics,
      cx,
    )
  }

  fn render_edit_group(&self, metrics: FlowRibbonMetrics, cx: &mut Context<Self>) -> impl IntoElement {
    let state = self.editor.read(cx).command_state();
    let editor = self.editor.clone();
    flow_ribbon_group(
      "Edit",
      vec![
        flow_chip("flow-ribbon-undo", metrics, cx)
          .icon(IconName::Undo)
          .tooltip("Undo")
          .disabled(!state.can_undo)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.undo_selected(window, cx));
            }
          }),
        flow_chip("flow-ribbon-redo", metrics, cx)
          .icon(IconName::Redo)
          .tooltip("Redo")
          .disabled(!state.can_redo)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.redo_selected(window, cx));
            }
          }),
        flow_chip("flow-ribbon-add-child", metrics, cx)
          .icon(IconName::ArrowRight)
          .tooltip("Add response")
          .disabled(!state.can_write || !state.has_selected_box)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.add_child_to_focus(window, cx));
            }
          }),
        flow_chip("flow-ribbon-add-above", metrics, cx)
          .icon(IconName::ArrowUp)
          .tooltip("Add above")
          .disabled(!state.can_write || !state.has_selected_box)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.add_sibling_to_focus(0, window, cx));
            }
          }),
        flow_chip("flow-ribbon-add-below", metrics, cx)
          .icon(IconName::ArrowDown)
          .tooltip("Add below")
          .disabled(!state.can_write || !state.has_selected_box)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.add_sibling_to_focus(1, window, cx));
            }
          }),
        flow_chip("flow-ribbon-extend", metrics, cx)
          .icon(IconName::ArrowRight)
          .tooltip("Extend")
          .disabled(!state.can_write || !state.can_format)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.extend_focus(window, cx));
            }
          }),
        flow_chip("flow-ribbon-delete-box", metrics, cx)
          .icon(IconName::Delete)
          .tooltip("Delete selected")
          .disabled(!state.can_write || !state.has_selected_box)
          .on_click(move |_, window, cx| {
            editor.update(cx, |editor, cx| editor.delete_focus(window, cx));
          }),
      ]
      .into_iter()
      .map(IntoElement::into_any_element)
      .collect(),
      metrics,
      cx,
    )
  }

  fn render_format_group(&self, metrics: FlowRibbonMetrics, cx: &mut Context<Self>) -> impl IntoElement {
    let state = self.editor.read(cx).command_state();
    let editor = self.editor.clone();
    flow_ribbon_group(
      "Format",
      vec![
        flow_chip("flow-ribbon-bold", metrics, cx)
          .child(Icon::default().path("icons/bold.svg").xsmall())
          .tooltip("Bold")
          .disabled(!state.can_write || !state.can_format)
          .selected(state.selected_bold)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.toggle_format_focus(FormatKind::Bold, window, cx));
            }
          }),
        flow_chip("flow-ribbon-cross", metrics, cx)
          .child(Icon::default().path("icons/strikethrough.svg").xsmall())
          .tooltip("Cross out")
          .disabled(!state.can_write || !state.can_format)
          .selected(state.selected_crossed)
          .on_click({
            let editor = editor.clone();
            move |_, window, cx| {
              editor.update(cx, |editor, cx| editor.toggle_format_focus(FormatKind::Crossed, window, cx));
            }
          }),
        flow_chip("flow-ribbon-fold", metrics, cx)
          .icon(IconName::PanelRightClose)
          .tooltip("Fold")
          .disabled(!state.can_fold)
          .selected(state.selected_folded)
          .on_click(move |_, _, cx| {
            editor.update(cx, |editor, cx| editor.toggle_fold_focus(cx));
          }),
      ]
      .into_iter()
      .map(IntoElement::into_any_element)
      .collect(),
      metrics,
      cx,
    )
  }
}

impl EventEmitter<()> for FlowRibbon {}

#[hotpath::measure_all]
impl Focusable for FlowRibbon {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

#[hotpath::measure_all]
impl Render for FlowRibbon {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.sync_controls(window, cx);
    let metrics = FlowRibbonMetrics::from_height(self.height);
    div()
      .w_full()
      .h(metrics.height)
      .min_h(min_flow_ribbon_height())
      .px(metrics.outer_padding_x)
      .child(
        h_flex()
          .w_full()
          .h_full()
          .items_start()
          .gap_2()
          .bg(cx.theme().background)
          .px(metrics.inner_padding_x)
          .pt(metrics.group_padding_top)
          .children([
            self.render_setup_group(metrics, cx).into_any_element(),
            self.render_title_group(metrics, cx).into_any_element(),
            self.render_edit_group(metrics, cx).into_any_element(),
            self.render_format_group(metrics, cx).into_any_element(),
          ]),
      )
  }
}

#[derive(Clone, Copy)]
struct FlowRibbonMetrics {
  height: gpui::Pixels,
  chip_height: gpui::Pixels,
  chip_padding_x: gpui::Pixels,
  chip_gap: gpui::Pixels,
  group_gap: gpui::Pixels,
  group_padding_top: gpui::Pixels,
  outer_padding_x: gpui::Pixels,
  inner_padding_x: gpui::Pixels,
}

#[hotpath::measure_all]
impl FlowRibbonMetrics {
  fn from_height(height: gpui::Pixels) -> Self {
    let height = px(
      height
        .as_f32()
        .clamp(min_flow_ribbon_height().as_f32(), max_flow_ribbon_height().as_f32()),
    );
    let scale = ((height.as_f32() - min_flow_ribbon_height().as_f32())
      / (max_flow_ribbon_height().as_f32() - min_flow_ribbon_height().as_f32()))
    .clamp(0.0, 1.0);
    Self {
      height,
      chip_height: px(20.0 + 10.0 * scale),
      chip_padding_x: px(3.0 + 7.0 * scale),
      chip_gap: px(2.0 + 4.0 * scale),
      group_gap: px(4.0 + 7.0 * scale),
      group_padding_top: px(3.0 + 3.0 * scale),
      outer_padding_x: px(8.0),
      inner_padding_x: px(8.0),
    }
  }
}

#[hotpath::measure]
fn flow_ribbon_group(label: &'static str, controls: Vec<AnyElement>, metrics: FlowRibbonMetrics, cx: &mut App) -> impl IntoElement {
  v_flex()
    .flex_none()
    .gap_0p5()
    .pr(metrics.group_gap)
    .border_r_1()
    .border_color(cx.theme().border.opacity(0.72))
    .child(
      div()
        .text_size(px(10.0))
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(label),
    )
    .child(
      h_flex()
        .items_center()
        .content_start()
        .gap(metrics.chip_gap)
        .flex_wrap()
        .children(controls),
    )
}

#[hotpath::measure]
fn flow_chip(id: impl Into<gpui::ElementId>, metrics: FlowRibbonMetrics, _cx: &mut App) -> Button {
  Button::new(id)
    .xsmall()
    .compact()
    .outline()
    .h(metrics.chip_height)
    .px(metrics.chip_padding_x)
    .rounded(px(6.0))
}

#[hotpath::measure]
fn style_key_for_label(label: &SharedString) -> Option<DebateStyleKey> {
  all_debate_style_templates()
    .into_iter()
    .find(|template| template.label == label.as_ref())
    .map(|template| template.key)
}

#[hotpath::measure]
fn default_flow_ribbon_height() -> gpui::Pixels {
  px(112.0)
}

#[hotpath::measure]
fn min_flow_ribbon_height() -> gpui::Pixels {
  px(56.0)
}

#[hotpath::measure]
fn max_flow_ribbon_height() -> gpui::Pixels {
  px(158.0)
}
