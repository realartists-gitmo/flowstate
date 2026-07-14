//! Pure board operations: preview, placement math, and the subtree-move law,
//! all over projection values with NO document and NO gate — safe to call
//! every drag frame (flow architecture spec Part 5: previews are pure
//! functions with zero gate traffic). The committing executors in
//! [`crate::mutate`] resolve through these same functions so previews and
//! commits cannot disagree.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, bail};

use crate::format::{CellId, ColumnId};
use crate::intents::{CellPlacement, FlowDropIntent};
use crate::projection::{Cell, Sheet};

pub fn is_descendant_of(sheet: &Sheet, cell_id: CellId, ancestor_id: CellId) -> bool {
  let mut parent = sheet
    .cells
    .iter()
    .find(|cell| cell.id == cell_id)
    .and_then(|cell| cell.parent_id);
  while let Some(parent_id) = parent {
    if parent_id == ancestor_id {
      return true;
    }
    parent = sheet
      .cells
      .iter()
      .find(|cell| cell.id == parent_id)
      .and_then(|cell| cell.parent_id);
  }
  false
}

/// The cell plus every descendant, in sheet order. Empty if unknown.
pub fn subtree_cell_ids(sheet: &Sheet, root_id: CellId) -> Vec<CellId> {
  sheet
    .cells
    .iter()
    .filter(|cell| cell.id == root_id || is_descendant_of(sheet, cell.id, root_id))
    .map(|cell| cell.id)
    .collect()
}

/// Flat-order insertion index that appends a new child AFTER `parent_id`'s
/// existing subtree (the shipped Shift-Enter answer placement).
pub fn child_append_index(sheet: &Sheet, parent_id: CellId) -> anyhow::Result<usize> {
  let parent_index = sheet
    .cells
    .iter()
    .position(|cell| cell.id == parent_id)
    .context("unknown cell")?;
  Ok(
    sheet
      .cells
      .iter()
      .enumerate()
      .filter(|(_, cell)| is_descendant_of(sheet, cell.id, parent_id))
      .map(|(index, _)| index)
      .max()
      .unwrap_or(parent_index)
      + 1,
  )
}

pub fn child_prepend_index(sheet: &Sheet, parent_id: CellId) -> anyhow::Result<usize> {
  let parent_index = sheet
    .cells
    .iter()
    .position(|cell| cell.id == parent_id)
    .context("unknown cell")?;
  Ok(parent_index + 1)
}

/// Focus target after deleting `cell_id`: previous sibling in the run, else
/// the parent, else the last root of the previous column.
pub fn deletion_fallback(sheet: &Sheet, column_ids: &[ColumnId], cell_id: CellId) -> Option<CellId> {
  let index = sheet.cells.iter().position(|cell| cell.id == cell_id)?;
  let cell = &sheet.cells[index];

  if let Some(previous) = sheet.cells[..index]
    .iter()
    .rev()
    .find(|candidate| candidate.column_id == cell.column_id && candidate.parent_id == cell.parent_id)
  {
    return Some(previous.id);
  }
  if let Some(parent) = cell.parent_id {
    return Some(parent);
  }
  let column = column_ids
    .iter()
    .position(|column| *column == cell.column_id)?;
  let left_column = *column_ids.get(column.checked_sub(1)?)?;
  sheet
    .cells
    .iter()
    .rev()
    .find(|candidate| candidate.column_id == left_column && candidate.parent_id.is_none())
    .map(|candidate| candidate.id)
}

/// Resolve a NEW cell's placement to (column index, flat insertion index,
/// parent) against the live sheet — the executor-side companion of
/// [`resolve_drop_intent`], for `FlowIntent::AddCell`.
pub fn resolve_cell_placement(
  sheet: &Sheet,
  column_ids: &[ColumnId],
  placement: CellPlacement,
) -> anyhow::Result<(usize, usize, Option<CellId>)> {
  let anchor = |target: CellId| -> anyhow::Result<(usize, &Cell)> {
    let index = sheet
      .cells
      .iter()
      .position(|cell| cell.id == target)
      .context("unknown anchor cell")?;
    Ok((index, &sheet.cells[index]))
  };
  let column_of = |cell: &Cell| -> anyhow::Result<usize> {
    column_ids
      .iter()
      .position(|column| *column == cell.column_id)
      .context("anchor references unknown column")
  };
  match placement {
    CellPlacement::Before(target) => {
      let (index, cell) = anchor(target)?;
      Ok((column_of(cell)?, index, cell.parent_id))
    },
    CellPlacement::After(target) => {
      let (index, cell) = anchor(target)?;
      Ok((column_of(cell)?, index + 1, cell.parent_id))
    },
    CellPlacement::FirstChildOf(parent) => {
      let (_, cell) = anchor(parent)?;
      let child_column = column_of(cell)? + 1;
      if child_column >= column_ids.len() {
        bail!("rightmost cells cannot receive responses");
      }
      Ok((child_column, child_prepend_index(sheet, parent)?, Some(parent)))
    },
    CellPlacement::LastChildOf(parent) => {
      let (_, cell) = anchor(parent)?;
      let child_column = column_of(cell)? + 1;
      if child_column >= column_ids.len() {
        bail!("rightmost cells cannot receive responses");
      }
      Ok((child_column, child_append_index(sheet, parent)?, Some(parent)))
    },
    CellPlacement::ColumnTop { column_index } => {
      if column_index >= column_ids.len() {
        bail!("column index out of range");
      }
      Ok((column_index, 0, None))
    },
    CellPlacement::SheetEnd { column_index } => {
      if column_index >= column_ids.len() {
        bail!("column index out of range");
      }
      Ok((column_index, sheet.cells.len(), None))
    },
  }
}

/// Cheap structural check of the two move-sensitive invariants — parent/child
/// column adjacency and contiguous sibling runs per column. A move never
/// touches cell content, so if the sheet was valid before, this is exactly
/// the set of failures a move can introduce.
pub fn sheet_topology_ok(sheet: &Sheet, column_ids: &[ColumnId]) -> bool {
  let level = |column: ColumnId| column_ids.iter().position(|candidate| *candidate == column);
  let cells: HashMap<CellId, &Cell> = sheet.cells.iter().map(|cell| (cell.id, cell)).collect();
  for cell in &sheet.cells {
    let Some(cell_level) = level(cell.column_id) else {
      return false;
    };
    if let Some(parent_id) = cell.parent_id {
      let Some(parent) = cells.get(&parent_id) else {
        return false;
      };
      let Some(parent_level) = level(parent.column_id) else {
        return false;
      };
      if cell_level != parent_level + 1 {
        return false;
      }
    }
  }
  for &column in column_ids {
    let mut completed_parents = HashSet::new();
    let mut current_parent = None;
    for cell in sheet.cells.iter().filter(|cell| cell.column_id == column) {
      if cell.parent_id != current_parent {
        if let Some(parent) = current_parent {
          completed_parents.insert(parent);
        }
        if cell
          .parent_id
          .is_some_and(|parent| completed_parents.contains(&parent))
        {
          return false;
        }
        current_parent = cell.parent_id;
      }
    }
  }
  true
}

pub fn resolve_drop_intent(
  sheet: &Sheet,
  column_ids: &[ColumnId],
  dragged: CellId,
  intent: FlowDropIntent,
) -> anyhow::Result<(usize, usize, Option<CellId>)> {
  match intent {
    FlowDropIntent::BeforeSibling(target) | FlowDropIntent::AfterSibling(target) => {
      if dragged == target {
        bail!("cannot drop a cell relative to itself");
      }
      if is_descendant_of(sheet, target, dragged) {
        bail!("cannot drop a cell relative to one of its descendants");
      }
      let target_index = sheet
        .cells
        .iter()
        .position(|cell| cell.id == target)
        .context("unknown target cell")?;
      let target_cell = &sheet.cells[target_index];
      let column_index = column_ids
        .iter()
        .position(|column| *column == target_cell.column_id)
        .context("target cell references unknown column")?;
      let insertion_index = if matches!(intent, FlowDropIntent::BeforeSibling(_)) {
        target_index
      } else {
        target_index + 1
      };
      Ok((column_index, insertion_index, target_cell.parent_id))
    },
    FlowDropIntent::FirstChildOf(parent) | FlowDropIntent::LastChildOf(parent) => {
      if dragged == parent {
        bail!("cannot drop a cell onto itself");
      }
      if is_descendant_of(sheet, parent, dragged) {
        bail!("cannot drop a cell onto one of its descendants");
      }
      let parent_index = sheet
        .cells
        .iter()
        .position(|cell| cell.id == parent)
        .context("unknown parent cell")?;
      let parent_cell = &sheet.cells[parent_index];
      let parent_column = column_ids
        .iter()
        .position(|column| *column == parent_cell.column_id)
        .context("parent cell references unknown column")?;
      let child_column = parent_column
        .checked_add(1)
        .filter(|index| *index < column_ids.len())
        .context("parent has no child column")?;
      let insertion_index = if matches!(intent, FlowDropIntent::FirstChildOf(_)) {
        parent_index + 1
      } else {
        sheet
          .cells
          .iter()
          .enumerate()
          .filter(|(_, cell)| is_descendant_of(sheet, cell.id, parent))
          .map(|(index, _)| index)
          .max()
          .unwrap_or(parent_index)
          + 1
      };
      Ok((child_column, insertion_index, Some(parent)))
    },
    FlowDropIntent::RootInColumn {
      column_index,
      insertion_index,
    } => {
      if column_index >= column_ids.len() {
        bail!("column index out of range");
      }
      // Positional-as-hint: clamped by the splice below / the executor.
      Ok((column_index, insertion_index.min(sheet.cells.len()), None))
    },
  }
}

/// Move `cell_id` (and its subtree) within `sheet` according to `intent`.
/// Pure over the sheet: backs both the committing executor and the read-only
/// drag previews, so the preview never shows a landing the commit would
/// reject.
pub fn apply_move_subtree(sheet: &mut Sheet, column_ids: &[ColumnId], cell_id: CellId, intent: FlowDropIntent) -> anyhow::Result<()> {
  let source = sheet
    .cells
    .iter()
    .position(|cell| cell.id == cell_id)
    .context("unknown cell")?;
  let source_level = column_ids
    .iter()
    .position(|column| *column == sheet.cells[source].column_id)
    .context("source cell references unknown column")?;

  let (target_column, target_index, new_parent) = resolve_drop_intent(sheet, column_ids, cell_id, intent)?;
  let level_delta = target_column as isize - source_level as isize;
  let subtree_ids = subtree_cell_ids(sheet, cell_id);
  if let Some(parent) = new_parent
    && subtree_ids.contains(&parent)
  {
    bail!("cannot move a cell into itself or one of its descendants");
  }
  let removed_before_target = sheet
    .cells
    .iter()
    .take(target_index.min(sheet.cells.len()))
    .filter(|cell| subtree_ids.contains(&cell.id))
    .count();

  let mut subtree = Vec::new();
  let mut remaining = Vec::with_capacity(sheet.cells.len());
  for cell in sheet.cells.drain(..) {
    if subtree_ids.contains(&cell.id) {
      subtree.push(cell);
    } else {
      remaining.push(cell);
    }
  }
  let insertion_index = target_index
    .saturating_sub(removed_before_target)
    .min(remaining.len());

  for cell in &mut subtree {
    let old_level = column_ids
      .iter()
      .position(|column| *column == cell.column_id)
      .context("subtree cell references unknown column")?;
    let new_level = old_level
      .checked_add_signed(level_delta)
      .filter(|level| *level < column_ids.len())
      .context("moving this subtree would place a descendant outside the sheet columns")?;
    cell.column_id = column_ids[new_level];
    if cell.id == cell_id {
      cell.parent_id = new_parent;
    }
  }
  remaining.splice(insertion_index..insertion_index, subtree);
  sheet.cells = remaining;
  Ok(())
}

/// Read-only preview of a subtree move: the sheet with the drag already
/// applied, or `None` if the real commit would reject it.
pub fn preview_move_cell_subtree(sheet: &Sheet, column_ids: &[ColumnId], cell_id: CellId, intent: FlowDropIntent) -> Option<Sheet> {
  let mut preview = sheet.clone();
  apply_move_subtree(&mut preview, column_ids, cell_id, intent).ok()?;
  sheet_topology_ok(&preview, column_ids).then_some(preview)
}

/// Read-only preview with the dragged subtree lifted out (reflow while no
/// valid target is hovered).
pub fn preview_without_subtree(sheet: &Sheet, cell_id: CellId) -> Option<Sheet> {
  let subtree: HashSet<CellId> = subtree_cell_ids(sheet, cell_id).into_iter().collect();
  if subtree.is_empty() {
    return None;
  }
  let mut preview = sheet.clone();
  preview.cells.retain(|cell| !subtree.contains(&cell.id));
  Some(preview)
}
