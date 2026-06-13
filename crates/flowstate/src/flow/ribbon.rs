use flowstate_flow::RelativePosition;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Subscription, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};

use crate::flow::{AnnotationTool, FlowEditor};

pub struct FlowRibbon {
  editor: Entity<FlowEditor>,
  focus_handle: FocusHandle,
  height: gpui::Pixels,
}

struct SheetNameInputState {
  input: Entity<InputState>,
  _subscription: Subscription,
}

impl FlowRibbon {
  pub fn new(editor: Entity<FlowEditor>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self {
      editor,
      focus_handle: cx.focus_handle(),
      height: px(112.0),
    }
  }

  pub fn editor(&self) -> Entity<FlowEditor> {
    self.editor.clone()
  }

  pub fn set_height(&mut self, height: gpui::Pixels, cx: &mut Context<Self>) {
    if self.height != height {
      self.height = height;
      cx.notify();
    }
  }
}

impl EventEmitter<()> for FlowRibbon {}

impl Focusable for FlowRibbon {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowRibbon {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let editor = self.editor.clone();
    let undo_editor = editor.clone();
    let redo_editor = editor.clone();
    let create_aff_editor = editor.clone();
    let create_neg_editor = editor.clone();
    let response_editor = editor.clone();
    let first_argument_editor = editor.clone();
    let previous_sheet_editor = editor.clone();
    let next_sheet_editor = editor.clone();
    let delete_sheet_editor = editor.clone();
    let above_editor = editor.clone();
    let below_editor = editor.clone();
    let delete_editor = editor.clone();
    let strike_editor = editor;
    let marker_editor = self.editor.clone();
    let eraser_editor = self.editor.clone();
    let visibility_editor = self.editor.clone();
    let clear_editor = self.editor.clone();
    let has_active_sheet = self.editor.read(cx).active_sheet().is_some();
    let has_active_cell = self.editor.read(cx).active_cell().is_some();
    let sheet_name_input = self.editor.read(cx).active_sheet().and_then(|sheet_id| {
      let sheet_name = self
        .editor
        .read(cx)
        .document()
        .projection()
        .sheets
        .iter()
        .find(|sheet| sheet.id == sheet_id)?
        .name
        .clone();
      let editor = self.editor.clone();
      let state = window.use_keyed_state(("flow-sheet-name-input", sheet_id.as_u128() as u64), cx, move |window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(sheet_name).placeholder("Sheet name"));
        let subscription = cx.subscribe_in(&input, window, move |_: &mut SheetNameInputState, input, event: &InputEvent, _, cx| {
          if matches!(event, InputEvent::Change) {
            let name = input.read(cx).value().to_string();
            editor.update(cx, |editor, cx| editor.rename_active_sheet(name, cx));
          }
        });
        SheetNameInputState {
          input,
          _subscription: subscription,
        }
      });
      Some(state.read(cx).input.clone())
    });
    div()
      .w_full()
      .h(self.height)
      .flex()
      .items_center()
      .gap(px(8.0))
      .px(px(12.0))
      .bg(cx.theme().secondary)
      .when_some(sheet_name_input, |this, input| this.child(div().w(px(180.0)).child(Input::new(&input).w_full())))
      .child(
        Button::new("flow-undo")
          .label("Undo")
          .small()
          .disabled(!self.editor.read(cx).can_undo())
          .on_click(move |_, _, cx| undo_editor.update(cx, |editor, cx| editor.undo(cx))),
      )
      .child(
        Button::new("flow-redo")
          .label("Redo")
          .small()
          .disabled(!self.editor.read(cx).can_redo())
          .on_click(move |_, _, cx| redo_editor.update(cx, |editor, cx| editor.redo(cx))),
      )
      .child(
        Button::new("flow-create-aff-sheet")
          .label("New affirmative")
          .small()
          .on_click(move |_, _, cx| create_aff_editor.update(cx, |editor, cx| editor.create_sheet_of_type(0, cx))),
      )
      .child(
        Button::new("flow-create-neg-sheet")
          .label("New negative")
          .small()
          .on_click(move |_, _, cx| create_neg_editor.update(cx, |editor, cx| editor.create_sheet_of_type(1, cx))),
      )
      .child(
        Button::new("flow-move-sheet-left")
          .label("Move sheet left")
          .small()
          .disabled(!has_active_sheet)
          .on_click(move |_, _, cx| previous_sheet_editor.update(cx, |editor, cx| editor.move_active_sheet(-1, cx))),
      )
      .child(
        Button::new("flow-move-sheet-right")
          .label("Move sheet right")
          .small()
          .disabled(!has_active_sheet)
          .on_click(move |_, _, cx| next_sheet_editor.update(cx, |editor, cx| editor.move_active_sheet(1, cx))),
      )
      .child(
        Button::new("flow-delete-sheet")
          .label("Delete sheet")
          .small()
          .danger()
          .disabled(!has_active_sheet)
          .on_click(move |_, _, cx| delete_sheet_editor.update(cx, |editor, cx| editor.delete_active_sheet(cx))),
      )
      .child(
        Button::new("flow-add-first-argument")
          .label("Add argument")
          .small()
          .disabled(!has_active_sheet)
          .on_click(move |_, _, cx| first_argument_editor.update(cx, |editor, cx| editor.add_first_argument(cx))),
      )
      .child(
        Button::new("flow-add-response")
          .label("Add response")
          .small()
          .disabled(!has_active_cell)
          .on_click(move |_, _, cx| response_editor.update(cx, |editor, cx| editor.add_response(cx))),
      )
      .child(
        Button::new("flow-add-sibling-above")
          .label("Sibling above")
          .small()
          .disabled(!has_active_cell)
          .on_click(move |_, _, cx| above_editor.update(cx, |editor, cx| editor.add_sibling(RelativePosition::Before, cx))),
      )
      .child(
        Button::new("flow-add-sibling")
          .label("Add sibling")
          .small()
          .disabled(!has_active_cell)
          .on_click(move |_, _, cx| below_editor.update(cx, |editor, cx| editor.add_sibling(RelativePosition::After, cx))),
      )
      .child(
        Button::new("flow-delete-selected")
          .label("Delete selected")
          .small()
          .danger()
          .disabled(!has_active_cell)
          .on_click(move |_, window, cx| delete_editor.update(cx, |editor, cx| editor.delete_selected(window, cx))),
      )
      .child(
        Button::new("flow-strike-selected")
          .label("Strike")
          .small()
          .disabled(!has_active_cell)
          .on_click(move |_, _, cx| strike_editor.update(cx, |editor, cx| editor.strike_selected(cx))),
      )
      .child(
        Button::new("flow-arm-marker")
          .label("Marker")
          .small()
          .on_click(move |_, _, cx| marker_editor.update(cx, |editor, cx| editor.set_annotation_tool(AnnotationTool::Marker, cx))),
      )
      .child(
        Button::new("flow-arm-eraser")
          .label("Eraser")
          .small()
          .on_click(move |_, _, cx| eraser_editor.update(cx, |editor, cx| editor.set_annotation_tool(AnnotationTool::Eraser, cx))),
      )
      .child(
        Button::new("flow-toggle-annotations")
          .label(if self.editor.read(cx).annotations_visible() {
            "Hide annotations"
          } else {
            "Show annotations"
          })
          .small()
          .on_click(move |_, _, cx| visibility_editor.update(cx, |editor, cx| editor.toggle_annotations_visible(cx))),
      )
      .child(
        Button::new("flow-clear-annotations")
          .label("Clear annotations")
          .small()
          .danger()
          .disabled(!has_active_sheet)
          .on_click(move |_, _, cx| clear_editor.update(cx, |editor, cx| editor.clear_annotations(cx))),
      )
  }
}
