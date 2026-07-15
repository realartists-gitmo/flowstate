use flowstate_flow::{CellId, FlowDropIntent, Sheet};
use gpui::{Context, IntoElement, Pixels, Point, Render, SharedString, Window, div, point, prelude::*, px};
use gpui_component::ActiveTheme as _;
use gpui_component::PixelsExt as _;

use super::FlowEditor;

#[derive(Clone)]
pub(super) struct FlowCellDrag {
  pub(super) cell_id: CellId,
}

/// Which edge of the target cell to accent as the drop indicator.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DropEdge {
  Before,
  After,
  Child,
}

pub(super) struct FlowCellDragPreview {
  pub(super) label: SharedString,
}

impl Render for FlowCellDragPreview {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .max_w(px(280.0))
      .px_2()
      .py_1()
      .rounded(cx.theme().radius)
      .bg(cx.theme().popover)
      .border_1()
      .border_color(cx.theme().border)
      .text_color(cx.theme().popover_foreground)
      .overflow_hidden()
      .child(if self.label.is_empty() {
        SharedString::from("Argument")
      } else {
        self.label.clone()
      })
  }
}

impl FlowEditor {
  /// Called when a card drag actually starts (past the drag threshold). Recording the source cell
  /// lets [`Self::drag_preview_sheet`] reflow the board as if the cell had already been picked up.
  pub(super) fn begin_cell_drag(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.dragging_cell != Some(cell_id) {
      self.dragging_cell = Some(cell_id);
      self.pending_cell_drop = None;
      self.start_drag_log(cell_id);
      cx.notify();
    }
  }

  /// The cell (and which edge of it) to accent as the current drop target, or `None` when there's no
  /// valid landing. Only landings the committed move would accept are shown, so hovering an illegal
  /// target (e.g. the cell's own descendant) shows nothing.
  pub(super) fn drag_drop_target(&self, sheet: &Sheet) -> Option<(CellId, DropEdge)> {
    let dragging = self.dragging_cell?;
    let intent = self.pending_cell_drop?;
    let _sheet_id = self.active_sheet?;
    let column_ids: Vec<_> = self
      .board
      .format
      .sheet_type(sheet.sheet_type_id)?
      .columns
      .iter()
      .map(|column| column.id)
      .collect();
    flowstate_flow::board_ops::preview_move_cell_subtree(sheet, &column_ids, dragging, intent)?;
    match intent {
      FlowDropIntent::BeforeSibling(target) => Some((target, DropEdge::Before)),
      FlowDropIntent::AfterSibling(target) => Some((target, DropEdge::After)),
      // Nesting maps to the boundary of the parent's existing children so the exact insertion point is
      // visible; with no children yet, accent the parent's child-side edge.
      FlowDropIntent::FirstChildOf(target) => Some(
        sheet
          .cells
          .iter()
          .find(|cell| cell.parent_id == Some(target))
          .map_or((target, DropEdge::Child), |child| (child.id, DropEdge::Before)),
      ),
      FlowDropIntent::LastChildOf(target) => Some(
        sheet
          .cells
          .iter()
          .rev()
          .find(|cell| cell.parent_id == Some(target))
          .map_or((target, DropEdge::Child), |child| (child.id, DropEdge::After)),
      ),
      FlowDropIntent::RootInColumn {
        column_index,
        insertion_index,
      } => {
        if let Some(cell) = sheet.cells.get(insertion_index) {
          Some((cell.id, DropEdge::Before))
        } else {
          let column = self
            .board
            .format
            .sheet_type(sheet.sheet_type_id)?
            .columns
            .get(column_index)?
            .id;
          sheet
            .cells
            .iter()
            .rev()
            .find(|cell| cell.column_id == column)
            .map(|cell| (cell.id, DropEdge::After))
        }
      },
    }
  }

  /// Whether the pointer is over a cell other than the one being dragged, so the column handler can
  /// defer to that cell's own drop zones. The dragged cell holds its slot as a faded placeholder, so
  /// the pointer passes through it to the column beneath.
  pub(super) fn cursor_over_live_cell(&self, position: Point<Pixels>) -> bool {
    self
      .cell_bounds
      .iter()
      .any(|(id, bounds)| Some(*id) != self.dragging_cell && bounds.contains(&position))
  }

  /// A parent in `parent_column` with no children yet whose card vertically contains `y`, so a drop in
  /// the (empty) `child_column` beside it can adopt it. The dragged cell is skipped so it can't parent
  /// to itself.
  fn childless_parent_at_row(&self, parent_column: usize, child_column: usize, y: gpui::Pixels) -> Option<CellId> {
    let sheet = self
      .board
      .sheets
      .iter()
      .find(|sheet| Some(sheet.id) == self.active_sheet)?;
    let definition = self.board.format.sheet_type(sheet.sheet_type_id)?;
    let parent_column = definition.columns.get(parent_column)?.id;
    let _ = definition.columns.get(child_column)?;
    sheet
      .cells
      .iter()
      .filter(|cell| cell.column_id == parent_column && Some(cell.id) != self.dragging_cell)
      .filter(|cell| {
        !sheet
          .cells
          .iter()
          .any(|other| other.parent_id == Some(cell.id))
      })
      .find(|cell| {
        self
          .cell_bounds
          .get(&cell.id)
          .is_some_and(|bounds| y >= bounds.top() && y <= bounds.bottom())
      })
      .map(|cell| cell.id)
  }

  pub(super) fn update_cell_drop(&mut self, destination: FlowDropIntent, cx: &mut Context<Self>) {
    if self.pending_cell_drop != Some(destination) {
      self.pending_cell_drop = Some(destination);
      cx.notify();
    }
  }

  pub(super) fn update_column_drop(&mut self, column_index: usize, y: gpui::Pixels, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let Some(sheet) = self.board.sheets.iter().find(|sheet| sheet.id == sheet_id) else {
      return;
    };
    let Some(definition) = self.board.format.sheet_type(sheet.sheet_type_id) else {
      return;
    };
    let Some(column) = definition.columns.get(column_index) else {
      return;
    };
    let column_cells: Vec<_> = sheet
      .cells
      .iter()
      .enumerate()
      .filter(|(_, cell)| cell.column_id == column.id)
      .collect();
    let below_position = column_cells.iter().position(|(_, cell)| {
      self
        .cell_bounds
        .get(&cell.id)
        .is_some_and(|bounds| y < bounds.center().y)
    });
    if let Some(below_position) = below_position
      && let Some((_, above)) = below_position
        .checked_sub(1)
        .and_then(|position| column_cells.get(position))
      && let Some((_, below)) = column_cells.get(below_position)
      && above.parent_id == below.parent_id
    {
      self.update_cell_drop(FlowDropIntent::BeforeSibling(below.id), cx);
      return;
    }
    // The natural "make a child" gesture is dropping in the child column beside the parent — but a
    // childless parent has nothing there, so it would otherwise fall through to an orphan drop. Adopt
    // it as the parent's child when the pointer sits at that parent's row.
    if let Some(parent) = column_index
      .checked_sub(1)
      .and_then(|parent_column| self.childless_parent_at_row(parent_column, column_index, y))
    {
      self.update_cell_drop(FlowDropIntent::LastChildOf(parent), cx);
      return;
    }
    let insertion_index = below_position.map_or(sheet.cells.len(), |position| column_cells[position].0);
    self.update_cell_drop(
      FlowDropIntent::RootInColumn {
        column_index,
        insertion_index,
      },
      cx,
    );
  }

  /// While dragging near a viewport edge, scroll the board toward it. Driven by a self-rescheduling
  /// frame loop so scrolling continues even when the pointer holds still at the edge.
  pub(super) fn update_drag_autoscroll(&mut self, pointer: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let bounds = self.board_scroll.bounds();
    if bounds.size.width <= px(1.0) || bounds.size.height <= px(1.0) {
      return;
    }
    const MARGIN: f32 = 56.0;
    const MAX_SPEED: f32 = 24.0;
    let ramp = |distance: f32| -> f32 {
      if distance <= 0.0 {
        MAX_SPEED
      } else if distance < MARGIN {
        MAX_SPEED * (1.0 - distance / MARGIN)
      } else {
        0.0
      }
    };
    let vx = ramp((pointer.x - bounds.left()).as_f32()) - ramp((bounds.right() - pointer.x).as_f32());
    let vy = ramp((pointer.y - bounds.top()).as_f32()) - ramp((bounds.bottom() - pointer.y).as_f32());
    if vx == 0.0 && vy == 0.0 {
      self.drag_autoscroll = None;
      return;
    }
    self.drag_autoscroll = Some(point(px(vx), px(vy)));
    self.schedule_drag_autoscroll(window, cx);
  }

  fn schedule_drag_autoscroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.drag_autoscroll_scheduled {
      return;
    }
    self.drag_autoscroll_scheduled = true;
    cx.on_next_frame(window, |editor, window, cx| {
      editor.drag_autoscroll_scheduled = false;
      let Some(velocity) = editor.drag_autoscroll else {
        return;
      };
      if !cx.has_active_drag() || editor.dragging_cell.is_none() {
        editor.drag_autoscroll = None;
        return;
      }
      editor.set_user_scroll_offset(editor.board_scroll.offset() + velocity);
      cx.notify();
      editor.schedule_drag_autoscroll(window, cx);
    });
  }

  pub(super) fn finish_cell_drop(&mut self, dragged: CellId, cx: &mut Context<Self>) {
    let destination = self.pending_cell_drop.take();
    self.dragging_cell = None;
    self.drag_autoscroll = None;
    let committed = match (destination, self.active_sheet) {
      (Some(destination), Some(sheet_id)) => self
        .apply_intent(
          &flowstate_flow::FlowIntent::MoveCellSubtree {
            sheet_id,
            cell_id: dragged,
            drop: destination,
          },
          cx,
        )
        .is_ok(),
      _ => false,
    };
    self.finish_drag_log(destination, committed);
    if committed {
      self.changed(Some(dragged), cx);
    } else {
      cx.notify();
    }
  }
}
