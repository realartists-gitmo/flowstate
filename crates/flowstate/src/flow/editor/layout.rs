use std::collections::HashMap;

use flowstate_flow::{Cell, CellId, Sheet};
use gpui::{Bounds, Pixels};
use gpui_component::PixelsExt as _;

pub(super) const CELL_GAP: f32 = 16.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct CellLayout {
  pub top: f32,
  pub height: f32,
}

pub(super) fn sheet_cell_layout(sheet: &Sheet, bounds: &HashMap<CellId, Bounds<Pixels>>, zoom: f32) -> HashMap<CellId, CellLayout> {
  let cells = sheet.cells.iter().map(|cell| (cell.id, cell)).collect::<HashMap<_, _>>();
  let mut children: HashMap<CellId, Vec<CellId>> = HashMap::new();
  for cell in &sheet.cells {
    if let Some(parent) = cell.parent_id {
      children.entry(parent).or_default().push(cell.id);
    }
  }
  let mut layout = HashMap::with_capacity(sheet.cells.len());
  let mut top = 0.0;
  for root in sheet.cells.iter().filter(|cell| cell.parent_id.is_none()) {
    let family_height = layout_family(root.id, top, &cells, &children, bounds, zoom, &mut layout);
    top += family_height + CELL_GAP * zoom;
  }
  layout
}

fn layout_family(
  cell_id: CellId,
  top: f32,
  cells: &HashMap<CellId, &Cell>,
  children: &HashMap<CellId, Vec<CellId>>,
  bounds: &HashMap<CellId, Bounds<Pixels>>,
  zoom: f32,
  layout: &mut HashMap<CellId, CellLayout>,
) -> f32 {
  let Some(cell) = cells.get(&cell_id) else {
    return 0.0;
  };
  let cell_height = measured_cell_height(cell, bounds);
  layout.insert(cell_id, CellLayout { top, height: cell_height });

  let mut children_top = top;
  let mut children_height = 0.0;
  for child_id in children.get(&cell_id).into_iter().flatten() {
    let child_height = layout_family(*child_id, children_top, cells, children, bounds, zoom, layout);
    children_top += child_height + CELL_GAP * zoom;
    children_height += child_height + CELL_GAP * zoom;
  }
  if children_height > 0.0 {
    children_height -= CELL_GAP * zoom;
  }
  cell_height.max(children_height)
}

fn measured_cell_height(cell: &Cell, bounds: &HashMap<CellId, Bounds<Pixels>>) -> f32 {
  bounds.get(&cell.id).map_or_else(|| estimated_cell_height(cell), |bounds| bounds.size.height.as_f32())
}

fn estimated_cell_height(cell: &Cell) -> f32 {
  let text = cell.summary_text().unwrap_or_default();
  let lines = text
    .lines()
    .map(|line| line.chars().count().div_ceil(34).max(1))
    .sum::<usize>()
    .max(1);
  32.0 + 22.0 * lines as f32
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_flow::{Cell, ColumnId};

  fn cell(parent_id: Option<CellId>) -> Cell {
    let mut cell = Cell::plain(ColumnId::new_v4()).unwrap();
    cell.parent_id = parent_id;
    cell
  }

  #[test]
  fn unrelated_roots_occupy_disjoint_global_bands() {
    let root = cell(None);
    let child_one = cell(Some(root.id));
    let child_two = cell(Some(root.id));
    let orphan = cell(None);
    let sheet = Sheet {
      id: flowstate_flow::SheetId::new_v4(),
      sheet_type_id: flowstate_flow::SheetTypeId::new_v4(),
      name: String::new(),
      cells: vec![root.clone(), child_one, child_two, orphan.clone()],
      annotations: Vec::new(),
    };

    let layout = sheet_cell_layout(&sheet, &HashMap::new(), 1.0);

    assert_eq!(layout[&root.id].top, 0.0);
    assert_eq!(layout[&orphan.id].top, 2.0 * 54.0 + 2.0 * CELL_GAP);
  }
}
