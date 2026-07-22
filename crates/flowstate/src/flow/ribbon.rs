use gpui::{
  App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, Render, Window, div,
  prelude::*, px,
};
use gpui_component::ActiveTheme as _;

use crate::flow::editor::RelativePosition;
use crate::flow::{AnnotationTool, FlowEditor};

pub struct FlowRibbon {
  editor: Entity<FlowEditor>,
  focus_handle: FocusHandle,
  height: gpui::Pixels,
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
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let editor = self.editor.clone();
    let undo_editor = editor.clone();
    let redo_editor = editor.clone();
    let response_editor = editor.clone();
    let first_argument_editor = editor.clone();
    let above_editor = editor.clone();
    let below_editor = editor.clone();
    let delete_editor = editor.clone();
    let strike_editor = editor;
    let marker_editor = self.editor.clone();
    let eraser_editor = self.editor.clone();
    let visibility_editor = self.editor.clone();
    let clear_editor = self.editor.clone();
    let clear_all_editor = self.editor.clone();
    let has_active_sheet = self.editor.read(cx).active_sheet().is_some();
    let has_active_cell = self.editor.read(cx).active_cell().is_some();
    // A9: header/gutter selections fill the operation set while `active_cell`
    // stays None — cell verbs gate on the SET, not the active cell.
    let has_targets = self.editor.read(cx).has_operation_targets();
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
              RibbonChip::new("flow-add-sibling-above", "Row above", "Insert a row above the cursor, with a fresh card in its column")
                .command_shortcut(CommandId::FlowAddSiblingAbove)
                .disabled(!has_active_sheet),
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
              RibbonChip::new("flow-add-sibling", "Row below", "Insert a row below the cursor, with a fresh card in its column")
                .command_shortcut(CommandId::FlowAddSiblingBelow)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, window, cx| {
              below_editor.update(cx, |editor, cx| {
                editor.add_sibling(RelativePosition::After, cx);
                editor.focus_active_cell(window, cx);
              });
            }),
          ),
      )
      // ---- Grid structure ----
      .child(
        ribbon_group(true, cx)
          .child({
            let editor = self.editor.clone();
            chip(
              RibbonChip::new("flow-delete-row", "Delete row", "Delete the cursor's row and every card in it").disabled(!has_active_sheet),
            )
            .on_click(move |_, _, cx| editor.update(cx, |editor, cx| editor.delete_cursor_row(cx)))
          })
          .child({
            let editor = self.editor.clone();
            chip(RibbonChip::new("flow-add-column", "Column", "Add a column at the right edge").disabled(!has_active_sheet))
              .on_click(move |_, _, cx| editor.update(cx, |editor, cx| editor.add_column_end(cx)))
          })
          .child({
            let editor = self.editor.clone();
            chip(
              RibbonChip::new("flow-delete-column", "Delete column", "Delete the cursor's column — undo brings it back")
                .danger(true)
                .disabled(!has_active_sheet),
            )
            .on_click(move |_, _, cx| editor.update(cx, |editor, cx| editor.delete_cursor_column(cx)))
          }),
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
                .disabled(!has_targets),
            )
            .on_click(move |_, _, cx| strike_editor.update(cx, |editor, cx| editor.strike_selected(cx))),
          )
          .child(
            chip(
              RibbonChip::new("flow-delete-selected", "Delete", "Delete the selected cells")
                .command_shortcut(CommandId::FlowDeleteSelected)
                .danger(true)
                .disabled(!has_targets),
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
          // Q-8: the pen IS your collab identity color — no palette. What
          // remains is the C11 width/opacity preset picker; picking one arms
          // the marker (picking a pen means you want to draw).
          .child({
            use crate::flow::PenPreset;
            let current = self.editor.read(cx).pen_preset();
            let pen_color = gpui::Hsla::from(gpui::rgba(self.editor.read(cx).marker_color_rgba()));
            gpui::div().flex().flex_row().items_center().gap_0p5().children(
              [PenPreset::Fine, PenPreset::Marker, PenPreset::Highlighter].into_iter().map(|preset| {
                let editor = self.editor.clone();
                let dot = 6.0 + preset.width() * 2.0;
                gpui::div()
                  .id(gpui::SharedString::from(format!("flow-pen-{}", preset.label())))
                  .size(gpui::px(20.0))
                  .flex()
                  .items_center()
                  .justify_center()
                  .rounded_full()
                  .border_2()
                  .border_color(if current == preset {
                    cx.theme().foreground
                  } else {
                    cx.theme().border.opacity(0.4)
                  })
                  .cursor_pointer()
                  .child(
                    gpui::div()
                      .size(gpui::px(dot))
                      .rounded_full()
                      .bg(pen_color.opacity(preset.opacity())),
                  )
                  .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    editor.update(cx, |editor, cx| editor.set_pen_preset(preset, cx));
                    cx.stop_propagation();
                  })
              }),
            )
          })
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
                "Show or hide ink everywhere — all sheets, remembered until you flip it back",
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
          )
          .child(
            chip(
              RibbonChip::new("flow-clear-all-annotations", "Clear all", "Delete your strokes on EVERY sheet — undo brings them back")
                .command_shortcut(CommandId::FlowClearAllAnnotations)
                .danger(true),
            )
            // P3: no confirmation dialogs anywhere — undo is the guard.
            .on_click(move |_, _, cx| clear_all_editor.update(cx, |editor, cx| editor.clear_all_annotations(cx))),
          ),
      )
      // ---- Round (E10) ----
      .child(
        ribbon_group(true, cx).child({
          let editor = self.editor.clone();
          let open = self.editor.read(cx).round_form_open();
          chip(
            RibbonChip::new("flow-round-metadata", "Round", "Tournament, round, opponent, judge, side, result — the flow's identity")
              .selected(open),
          )
          .on_click(move |_, window, cx| editor.update(cx, |editor, cx| editor.toggle_round_form(window, cx)))
        }),
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
