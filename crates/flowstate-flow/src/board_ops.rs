//! Pure board operations over [`FlowBoardProjection`] (.fl0 v2 spec, Part A).
//!
//! Every function here is a pure function of projection data — no Loro, no
//! gate — so the drag preview can run one every frame, and the committing
//! runtime resolves the SAME functions inside the gate (one law for preview
//! and commit). Ported verbatim from the record-era `operations.rs`.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, bail};
use uuid::Uuid;

use crate::format::{CellId, SheetId};
use crate::intents::FlowDropIntent;
use crate::projection::{Cell, FlowBoardProjection, Sheet};

pub fn is_descendant_of(sheet: &Sheet, cell_id: CellId, ancestor_id: CellId) -> bool {
  let mut parent = sheet.cell(cell_id).and_then(|cell| cell.parent_id);
  while let Some(parent_id) = parent {
    if parent_id == ancestor_id {
      return true;
    }
    parent = sheet.cell(parent_id).and_then(|cell| cell.parent_id);
  }
  false
}

/// The dragged cell plus every descendant, in sheet order. Empty if unknown.
#[must_use]
pub fn subtree_cell_ids(sheet: &Sheet, root_id: CellId) -> Vec<CellId> {
  sheet
    .cells
    .iter()
    .filter(|cell| cell.id == root_id || is_descendant_of(sheet, cell.id, root_id))
    .map(|cell| cell.id)
    .collect()
}

pub fn sheet_column_ids(board: &FlowBoardProjection, sheet_id: SheetId) -> anyhow::Result<Vec<Uuid>> {
  let sheet = board.sheet(sheet_id).context("unknown sheet")?;
  let definition = board
    .format
    .sheet_type(sheet.sheet_type_id)
    .context("unknown sheet type")?;
  Ok(definition.columns.iter().map(|column| column.id).collect())
}

/// Flat insertion index appending AFTER every existing descendant of `parent`.
pub fn child_append_index(board: &FlowBoardProjection, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
  let sheet = board.sheet(sheet_id).context("unknown sheet")?;
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

/// Flat insertion index immediately after `parent` (before existing subtrees).
pub fn child_prepend_index(board: &FlowBoardProjection, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
  let sheet = board.sheet(sheet_id).context("unknown sheet")?;
  let parent_index = sheet
    .cells
    .iter()
    .position(|cell| cell.id == parent_id)
    .context("unknown cell")?;
  Ok(parent_index + 1)
}

/// Focus fallback after deleting `cell_id`: previous same-column sibling, then
/// parent, then the last root of the column to the left.
#[must_use]
pub fn deletion_fallback(board: &FlowBoardProjection, sheet_id: SheetId, cell_id: CellId) -> Option<CellId> {
  let sheet = board.sheet(sheet_id)?;
  let definition = board.format.sheet_type(sheet.sheet_type_id)?;
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

  let column = definition
    .columns
    .iter()
    .position(|column| column.id == cell.column_id)?;
  let left_column = column
    .checked_sub(1)
    .and_then(|index| definition.columns.get(index))?
    .id;
  sheet
    .cells
    .iter()
    .rev()
    .find(|candidate| candidate.column_id == left_column && candidate.parent_id.is_none())
    .map(|candidate| candidate.id)
}

/// Cheap structural check of the two move-sensitive invariants — parent/child
/// column adjacency and contiguous sibling runs per column. A move never
/// touches cell content, so this is exactly the failure set a move can
/// introduce.
#[must_use]
pub fn sheet_topology_ok(sheet: &Sheet, column_ids: &[Uuid]) -> bool {
  let level = |column: Uuid| column_ids.iter().position(|candidate| *candidate == column);
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

/// Move `cell_id` (and its subtree) within `sheet` according to `intent`.
/// Pure over the sheet, so the same algorithm backs the committing runtime
/// AND the read-only drag previews.
pub fn apply_move_subtree(sheet: &mut Sheet, column_ids: &[Uuid], cell_id: CellId, intent: FlowDropIntent) -> anyhow::Result<()> {
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

pub fn resolve_drop_intent(
  sheet: &Sheet,
  column_ids: &[Uuid],
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
      let insertion_index = match intent {
        FlowDropIntent::BeforeSibling(_) => target_index,
        FlowDropIntent::AfterSibling(_) => target_index + 1,
        FlowDropIntent::FirstChildOf(_) | FlowDropIntent::LastChildOf(_) | FlowDropIntent::RootInColumn { .. } => unreachable!(),
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
      let insertion_index = match intent {
        FlowDropIntent::FirstChildOf(_) => parent_index + 1,
        FlowDropIntent::LastChildOf(_) => {
          sheet
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| is_descendant_of(sheet, cell.id, parent))
            .map(|(index, _)| index)
            .max()
            .unwrap_or(parent_index)
            + 1
        },
        FlowDropIntent::BeforeSibling(_) | FlowDropIntent::AfterSibling(_) | FlowDropIntent::RootInColumn { .. } => unreachable!(),
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
      Ok((column_index, insertion_index, None))
    },
  }
}

/// Read-only preview of a subtree move: the sheet with the drag already
/// applied, or `None` if the move is invalid (self/descendant drop, a
/// descendant outside the columns, or a landing the committed move would
/// reject). Safe every drag frame — zero gate traffic.
#[must_use]
pub fn preview_move_cell_subtree(board: &FlowBoardProjection, sheet_id: SheetId, cell_id: CellId, intent: FlowDropIntent) -> Option<Sheet> {
  let column_ids = sheet_column_ids(board, sheet_id).ok()?;
  let mut sheet = board.sheet(sheet_id)?.clone();
  apply_move_subtree(&mut sheet, &column_ids, cell_id, intent).ok()?;
  sheet_topology_ok(&sheet, &column_ids).then_some(sheet)
}

/// Read-only preview with the dragged subtree lifted out (drag in flight, no
/// valid target hovered).
#[must_use]
pub fn preview_without_subtree(board: &FlowBoardProjection, sheet_id: SheetId, cell_id: CellId) -> Option<Sheet> {
  let sheet = board.sheet(sheet_id)?;
  let subtree: HashSet<CellId> = subtree_cell_ids(sheet, cell_id).into_iter().collect();
  if subtree.is_empty() {
    return None;
  }
  let mut sheet = sheet.clone();
  sheet.cells.retain(|cell| !subtree.contains(&cell.id));
  Some(sheet)
}
