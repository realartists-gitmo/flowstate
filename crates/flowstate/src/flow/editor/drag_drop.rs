use flowstate_flow::{CellId, RelativePosition};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*};
use gpui_component::ActiveTheme as _;

use super::FlowEditor;

#[derive(Clone)]
pub(super) struct FlowCellDrag {
  pub(super) cell_id: CellId,
}

pub(super) struct FlowCellDragPreview;

#[derive(Clone, Copy)]
pub(super) enum CellDropDestination {
  Relative(CellId, RelativePosition),
  ChildOf(CellId),
  OrphanInColumn { column_index: usize, insertion_index: usize },
}

impl Render for FlowCellDragPreview {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .px_2()
      .py_1()
      .rounded(cx.theme().radius)
      .bg(cx.theme().popover)
      .border_1()
      .border_color(cx.theme().border)
      .child("Move argument")
  }
}

impl FlowEditor {
  pub(super) fn update_cell_drop(&mut self, destination: CellDropDestination, cx: &mut Context<Self>) {
    self.pending_cell_drop = Some(destination);
    cx.notify();
  }

  pub(super) fn update_column_drop(&mut self, column_index: usize, y: gpui::Pixels, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let Some(sheet) = self.document.projection().sheets.iter().find(|sheet| sheet.id == sheet_id) else {
      return;
    };
    let Some(definition) = self.document.projection().format.sheet_type(sheet.sheet_type_id) else {
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
    let below_position = column_cells
      .iter()
      .position(|(_, cell)| self.cell_bounds.get(&cell.id).is_some_and(|bounds| y < bounds.center().y));
    if let Some(below_position) = below_position
      && let Some((_, above)) = below_position.checked_sub(1).and_then(|position| column_cells.get(position))
      && let Some((_, below)) = column_cells.get(below_position)
      && above.parent_id == below.parent_id
    {
      self.update_cell_drop(CellDropDestination::Relative(below.id, RelativePosition::Before), cx);
      return;
    }
    let insertion_index = below_position.map_or(sheet.cells.len(), |position| column_cells[position].0);
    self.update_cell_drop(
      CellDropDestination::OrphanInColumn {
        column_index,
        insertion_index,
      },
      cx,
    );
  }

  pub(super) fn finish_cell_drop(&mut self, dragged: CellId, cx: &mut Context<Self>) {
    let Some(destination) = self.pending_cell_drop.take() else {
      return;
    };
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let Some(sheet) = self.document.projection().sheets.iter().find(|sheet| sheet.id == sheet_id) else {
      return;
    };
    let Some(definition) = self.document.projection().format.sheet_type(sheet.sheet_type_id) else {
      return;
    };
    let (column_index, insertion_index, parent_id) = match destination {
      CellDropDestination::Relative(target, position) => {
        if dragged == target {
          return;
        }
        let Some(target_index) = sheet.cells.iter().position(|cell| cell.id == target) else {
          return;
        };
        let target_cell = &sheet.cells[target_index];
        let Some(column_index) = definition.columns.iter().position(|column| column.id == target_cell.column_id) else {
          return;
        };
        let insertion_index = match position {
          RelativePosition::Before => target_index,
          RelativePosition::After => target_index + 1,
        };
        (column_index, insertion_index, target_cell.parent_id)
      },
      CellDropDestination::ChildOf(target) => {
        if dragged == target {
          return;
        }
        let Some(target_cell) = sheet.cells.iter().find(|cell| cell.id == target) else {
          return;
        };
        let Some(parent_column) = definition.columns.iter().position(|column| column.id == target_cell.column_id) else {
          return;
        };
        let Some(child_column) = parent_column.checked_add(1).filter(|index| *index < definition.columns.len()) else {
          return;
        };
        let insertion_index = self.document.child_append_index(sheet_id, target).unwrap_or(sheet.cells.len());
        (child_column, insertion_index, Some(target))
      },
      CellDropDestination::OrphanInColumn {
        column_index,
        insertion_index,
      } => (column_index, insertion_index, None),
    };
    if self
      .document
      .move_cell(sheet_id, dragged, column_index, insertion_index, parent_id)
      .is_ok()
    {
      self.changed(Some(dragged), cx);
    }
  }
}
