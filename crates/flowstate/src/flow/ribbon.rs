use flowstate_flow::RelativePosition;
use gpui::{
  App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, Render, Subscription, Window, div,
  prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::ActiveTheme as _;

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
    #[allow(
      clippy::needless_collect,
      reason = "Release the editor read guard before constructing buttons that clone the editor entity."
    )]
    let sheet_types: Vec<_> = self
      .editor
      .read(cx)
      .board()
      .format
      .sheet_types
      .iter()
      .map(|sheet_type| sheet_type.name.clone())
      .collect();
    let sheet_name_input = self.editor.read(cx).active_sheet().and_then(|sheet_id| {
      let sheet_name = self
        .editor
        .read(cx)
        .board()
        .sheets
        .iter()
        .find(|sheet| sheet.id == sheet_id)?
        .name
        .clone();
      let editor = self.editor.clone();
      let state = window.use_keyed_state(("flow-sheet-name-input", sheet_id.as_u128() as u64), cx, move |window, cx| {
        let input = cx.new(|cx| {
          InputState::new(window, cx)
            .default_value(sheet_name)
            .placeholder("Sheet name")
        });
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
    let chip = |chip: crate::ribbon::shared::RibbonChip| chip.build(cx);
    let scrubbing = self.editor.read(cx).history_scrubbing();
    let annotation_tool = self.editor.read(cx).annotation_tool();
    let annotations_visible = self.editor.read(cx).annotations_visible();
    let struck = self.editor.read(cx).active_cell_is_struck();
    let can_undo = self.editor.read(cx).can_undo();
    let can_redo = self.editor.read(cx).can_redo();
    use crate::commands::CommandId;
    use crate::ribbon::shared::{RibbonChip, ribbon_group};

    div()
      .w_full()
      .h(self.height)
      .flex()
      .items_center()
      .gap(px(10.0))
      .px(px(12.0))
      .bg(cx.theme().secondary)
      .on_mouse_down(MouseButton::Left, {
        let editor = self.editor.clone();
        move |_: &MouseDownEvent, _, cx| editor.update(cx, |editor, cx| editor.set_annotation_tool(AnnotationTool::None, cx))
      })
      // S11: peers on OTHER sheets show as colored dots by the switcher.
      .children({
        let active_sheet = self.editor.read(cx).active_sheet();
        let dots: Vec<_> = self
          .editor
          .read(cx)
          .external_presences()
          .iter()
          .filter(|presence| presence.sheet.is_some() && presence.sheet != active_sheet)
          .map(|presence| {
            gpui::div()
              .size(gpui::px(8.0))
              .rounded_full()
              .bg(gpui::Hsla::from(gpui::rgba((presence.color_rgb << 8) | 0xff)))
              .into_any_element()
          })
          .collect();
        dots
      })
      .when_some(sheet_name_input, |this, input| {
        this.child(div().w(px(170.0)).child(Input::new(&input).w_full()))
      })
      // ---- Undo / Redo ----
      .child(
        ribbon_group(false, cx)
          .child(
            chip(
              RibbonChip::new("flow-undo", "Undo", "Undo")
                .command_shortcut(CommandId::Undo)
                .disabled(!can_undo),
            )
            .on_click(move |_, _, cx| undo_editor.update(cx, |editor, cx| editor.undo(cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-redo", "Redo", "Redo")
                .command_shortcut(CommandId::Redo)
                .disabled(!can_redo),
            )
            .on_click(move |_, _, cx| redo_editor.update(cx, |editor, cx| editor.redo(cx))),
          ),
      )
      // ---- Sheet ----
      .child(
        ribbon_group(true, cx)
          .children(sheet_types.into_iter().enumerate().map(|(index, name)| {
            let editor = self.editor.clone();
            chip(RibbonChip::new(
              ("flow-create-sheet-type", index),
              format!("New {name}"),
              format!("Create a {name} sheet"),
            ))
            .on_click(move |_, _, cx| editor.update(cx, |editor, cx| editor.create_sheet_of_type(index, cx)))
          }))
          .child(
            chip(
              RibbonChip::new("flow-move-sheet-left", "◀", "Move this sheet left")
                .command_shortcut(CommandId::FlowMoveSheetLeft)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, _, cx| previous_sheet_editor.update(cx, |editor, cx| editor.move_active_sheet(-1, cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-move-sheet-right", "▶", "Move this sheet right")
                .command_shortcut(CommandId::FlowMoveSheetRight)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, _, cx| next_sheet_editor.update(cx, |editor, cx| editor.move_active_sheet(1, cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-delete-sheet", "Delete sheet", "Delete this sheet — every cell on it goes too")
                .command_shortcut(CommandId::FlowDeleteSheet)
                .danger(true)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, _, cx| delete_sheet_editor.update(cx, |editor, cx| editor.delete_active_sheet(cx))),
          ),
      )
      // ---- Argument ----
      .child(
        ribbon_group(true, cx)
          .child(
            chip(
              RibbonChip::new("flow-add-first-argument", "Argument", "Start a new argument in the first column")
                .icon("icons/paragraph-break-two.svg")
                .command_shortcut(CommandId::FlowNewFamily)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, window, cx| {
              first_argument_editor.update(cx, |editor, cx| {
                editor.add_first_argument(cx);
                editor.focus_active_cell(window, cx);
              });
            }),
          )
          .child(
            chip(
              RibbonChip::new("flow-add-response", "Response", "Answer the selected cell in the next column")
                .icon("icons/send-horizontal.svg")
                .command_shortcut(CommandId::FlowAddResponse)
                .disabled(!has_active_cell),
            )
            .on_click(move |_, window, cx| {
              response_editor.update(cx, |editor, cx| {
                editor.add_response(cx);
                editor.focus_active_cell(window, cx);
              });
            }),
          )
          .child(
            chip(
              RibbonChip::new("flow-add-sibling-above", "Above", "Add a sibling cell above")
                .command_shortcut(CommandId::FlowAddSiblingAbove)
                .disabled(!has_active_cell),
            )
            .on_click(move |_, window, cx| {
              above_editor.update(cx, |editor, cx| {
                editor.add_sibling(RelativePosition::Before, cx);
                editor.focus_active_cell(window, cx);
              });
            }),
          )
          .child(
            chip(
              RibbonChip::new("flow-add-sibling", "Below", "Add a sibling cell below")
                .command_shortcut(CommandId::FlowAddSiblingBelow)
                .disabled(!has_active_cell),
            )
            .on_click(move |_, window, cx| {
              below_editor.update(cx, |editor, cx| {
                editor.add_sibling(RelativePosition::After, cx);
                editor.focus_active_cell(window, cx);
              });
            }),
          ),
      )
      // ---- Cell ----
      .child(
        ribbon_group(true, cx)
          .child(
            chip(
              RibbonChip::new("flow-strike-selected", "Strike", "Strike the selected cell — answered, kept legible")
                .icon("icons/strikethrough.svg")
                .command_shortcut(CommandId::FlowStrike)
                .selected(struck)
                .disabled(!has_active_cell),
            )
            .on_click(move |_, _, cx| strike_editor.update(cx, |editor, cx| editor.strike_selected(cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-delete-selected", "Delete", "Delete the selected cell and its thread")
                .command_shortcut(CommandId::FlowDeleteSelected)
                .danger(true)
                .disabled(!has_active_cell),
            )
            .on_click(move |_, window, cx| delete_editor.update(cx, |editor, cx| editor.delete_selected(window, cx))),
          ),
      )
      // ---- Ink ----
      .child(
        ribbon_group(true, cx)
          .child(
            chip(
              RibbonChip::new("flow-arm-marker", "Marker", "Draw freehand strokes on the board")
                .icon("icons/highlighter.svg")
                .command_shortcut(CommandId::FlowToggleMarker)
                .selected(annotation_tool == AnnotationTool::Marker),
            )
            .on_click(move |_, _, cx| marker_editor.update(cx, |editor, cx| editor.toggle_annotation_tool(AnnotationTool::Marker, cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-arm-eraser", "Eraser", "Erase strokes")
                .icon("icons/eraser.svg")
                .command_shortcut(CommandId::FlowToggleEraser)
                .selected(annotation_tool == AnnotationTool::Eraser),
            )
            .on_click(move |_, _, cx| eraser_editor.update(cx, |editor, cx| editor.toggle_annotation_tool(AnnotationTool::Eraser, cx))),
          )
          .child(
            chip(
              RibbonChip::new(
                "flow-toggle-annotations",
                if annotations_visible { "Hide ink" } else { "Show ink" },
                "Show or hide every stroke on the board",
              )
              .command_shortcut(CommandId::FlowToggleAnnotations),
            )
            .on_click(move |_, _, cx| visibility_editor.update(cx, |editor, cx| editor.toggle_annotations_visible(cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-clear-annotations", "Clear ink", "Delete every stroke on this sheet")
                .command_shortcut(CommandId::FlowClearAnnotations)
                .danger(true)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, _, cx| clear_editor.update(cx, |editor, cx| editor.clear_annotations(cx))),
          ),
      )
      // ---- History ----
      .child(
        ribbon_group(true, cx).child({
          let editor = self.editor.clone();
          chip(
            RibbonChip::new(
              "flow-history-scrubber",
              if scrubbing { "Exit history" } else { "History" },
              "Replay this board on the tape — restore or pin any moment",
            )
            .icon("icons/pin.svg")
            .command_shortcut(CommandId::OpenHistory)
            .selected(scrubbing)
            .disabled(!has_active_sheet),
          )
          .on_click(move |_, _, cx| editor.update(cx, |editor, cx| editor.toggle_history_scrubber(cx)))
        }),
      )
  }
}
