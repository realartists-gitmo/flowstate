use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, bail};
use uuid::Uuid;

use crate::{AnnotationOriginator, AnnotationStroke, Cell, CellId, FlowDocument, FlowProjection, Sheet, SheetId, SheetTypeId};
pub use crate::intents::{FlowDropIntent, RelativePosition};

impl FlowDocument {
  pub fn child_append_index(&self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?;
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

  pub fn child_prepend_index(&self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?;
    let parent_index = sheet
      .cells
      .iter()
      .position(|cell| cell.id == parent_id)
      .context("unknown cell")?;
    Ok(parent_index + 1)
  }

  pub fn deletion_fallback(&self, sheet_id: SheetId, cell_id: CellId) -> Option<CellId> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)?;
    let definition = self.projection().format.sheet_type(sheet.sheet_type_id)?;
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

  pub fn create_sheet(&mut self, name: impl Into<String>, sheet_type_id: SheetTypeId) -> anyhow::Result<SheetId> {
    let id = Uuid::new_v4();
    let name = name.into();
    self.update(|projection| {
      projection
        .format
        .sheet_type(sheet_type_id)
        .context("unknown sheet type")?;
      projection.sheets.push(Sheet {
        id,
        name,
        sheet_type_id,
        cells: Vec::new(),
        annotations: Vec::new(),
      });
      Ok(())
    })?;
    Ok(id)
  }

  pub fn rename_sheet(&mut self, sheet_id: SheetId, name: impl Into<String>) -> anyhow::Result<()> {
    let name = name.into();
    self.update(|projection| {
      projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?
        .name = name;
      Ok(())
    })
  }

  pub fn delete_sheet(&mut self, sheet_id: SheetId) -> anyhow::Result<()> {
    self.update(|projection| {
      let before = projection.sheets.len();
      projection.sheets.retain(|sheet| sheet.id != sheet_id);
      if projection.sheets.len() == before {
        bail!("unknown sheet");
      }
      Ok(())
    })
  }

  pub fn move_sheet(&mut self, sheet_id: SheetId, target_index: usize) -> anyhow::Result<()> {
    self.update(|projection| {
      let source = projection
        .sheets
        .iter()
        .position(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?;
      let sheet = projection.sheets.remove(source);
      projection
        .sheets
        .insert(target_index.min(projection.sheets.len()), sheet);
      Ok(())
    })
  }

  pub fn add_plain_cell(
    &mut self,
    sheet_id: SheetId,
    column_index: usize,
    parent_id: Option<CellId>,
    insertion_index: Option<usize>,
  ) -> anyhow::Result<CellId> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?;
    let definition = self
      .projection()
      .format
      .sheet_type(sheet.sheet_type_id)
      .context("unknown sheet type")?;
    let column_id = definition
      .columns
      .get(column_index)
      .context("column index out of range")?
      .id;
    let mut cell = Cell::plain(column_id)?;
    cell.parent_id = parent_id;
    let id = cell.id;
    self.update(|projection| {
      let sheet = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?;
      sheet.cells.insert(
        insertion_index
          .unwrap_or(sheet.cells.len())
          .min(sheet.cells.len()),
        cell,
      );
      Ok(())
    })?;
    Ok(id)
  }

  pub fn add_orphan_at_column_top(&mut self, sheet_id: SheetId, column_index: usize) -> anyhow::Result<CellId> {
    self.add_plain_cell(sheet_id, column_index, None, Some(0))
  }

  pub fn add_sibling(&mut self, sheet_id: SheetId, cell_id: CellId, position: RelativePosition) -> anyhow::Result<CellId> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?;
    let index = sheet
      .cells
      .iter()
      .position(|cell| cell.id == cell_id)
      .context("unknown cell")?;
    let source = &sheet.cells[index];
    let definition = self
      .projection()
      .format
      .sheet_type(sheet.sheet_type_id)
      .context("unknown sheet type")?;
    let column = definition
      .columns
      .iter()
      .position(|column| column.id == source.column_id)
      .context("unknown column")?;
    let insertion = match position {
      RelativePosition::Before => index,
      RelativePosition::After => index + 1,
    };
    self.add_plain_cell(sheet_id, column, source.parent_id, Some(insertion))
  }

  pub fn add_response(&mut self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<CellId> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?;
    let parent = sheet
      .cells
      .iter()
      .find(|cell| cell.id == parent_id)
      .context("unknown cell")?;
    let definition = self
      .projection()
      .format
      .sheet_type(sheet.sheet_type_id)
      .context("unknown sheet type")?;
    let parent_column = definition
      .columns
      .iter()
      .position(|column| column.id == parent.column_id)
      .context("unknown column")?;
    let child_column = parent_column + 1;
    if child_column >= definition.columns.len() {
      bail!("rightmost cells cannot receive responses");
    }
    let insertion = self.child_append_index(sheet_id, parent_id)?;
    self.add_plain_cell(sheet_id, child_column, Some(parent_id), Some(insertion))
  }

  pub fn add_first_response(&mut self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<CellId> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?;
    let parent = sheet
      .cells
      .iter()
      .find(|cell| cell.id == parent_id)
      .context("unknown cell")?;
    let definition = self
      .projection()
      .format
      .sheet_type(sheet.sheet_type_id)
      .context("unknown sheet type")?;
    let parent_column = definition
      .columns
      .iter()
      .position(|column| column.id == parent.column_id)
      .context("unknown column")?;
    let child_column = parent_column + 1;
    if child_column >= definition.columns.len() {
      bail!("rightmost cells cannot receive responses");
    }
    let insertion = self.child_prepend_index(sheet_id, parent_id)?;
    self.add_plain_cell(sheet_id, child_column, Some(parent_id), Some(insertion))
  }

  pub fn delete_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    self.update(|projection| {
      let sheet = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?;
      let before = sheet.cells.len();
      sheet.cells.retain(|cell| cell.id != cell_id);
      if sheet.cells.len() == before {
        bail!("unknown cell");
      }
      for cell in &mut sheet.cells {
        if cell.parent_id == Some(cell_id) {
          cell.parent_id = None;
        }
      }
      Ok(())
    })
  }

  pub fn strike_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    self.update(|projection| {
      let cell = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?
        .cells
        .iter_mut()
        .find(|cell| cell.id == cell_id)
        .context("unknown cell")?;
      let mut document = cell.document()?;
      {
        let mut paragraphs = document.paragraphs.make_mut();
        let struck = paragraphs
          .iter()
          .flat_map(|paragraph| &paragraph.runs)
          .all(|run| run.styles.strikethrough);
        for paragraph in &mut *paragraphs {
          for run in &mut paragraph.runs {
            run.styles.strikethrough = !struck;
          }
          paragraph.version = paragraph.version.wrapping_add(1);
        }
      }
      document.blocks =
        flowstate_document::BlockSeq::from_vec(flowstate_document::paragraph_blocks_from_paragraphs(&document.paragraphs.to_vec()));
      cell.document_bytes = crate::document::db8_bytes(&document)?;
      Ok(())
    })
  }

  pub fn replace_cell_document(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    document: &flowstate_document::DocumentProjection,
  ) -> anyhow::Result<()> {
    let bytes = crate::document::db8_bytes(document)?;
    self.update(|projection| {
      projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?
        .cells
        .iter_mut()
        .find(|cell| cell.id == cell_id)
        .context("unknown cell")?
        .document_bytes = bytes;
      Ok(())
    })
  }

  pub fn ensure_cell_editable_projection(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<bool> {
    let cell = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .context("unknown sheet")?
      .cells
      .iter()
      .find(|cell| cell.id == cell_id)
      .context("unknown cell")?;
    if cell.uses_summary_projection()? {
      return Ok(false);
    }
    self.update(|projection| {
      let cell = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?
        .cells
        .iter_mut()
        .find(|cell| cell.id == cell_id)
        .context("unknown cell")?;
      let mut document = cell.document()?;
      let mut paragraphs = document.paragraphs.make_mut();
      let paragraph = paragraphs
        .first_mut()
        .context("cell document has no paragraph")?;
      paragraph.style = flowstate_document::PARAGRAPH_TAG;
      paragraph.version = paragraph.version.wrapping_add(1);
      drop(paragraphs);
      document.blocks =
        flowstate_document::BlockSeq::from_vec(flowstate_document::paragraph_blocks_from_paragraphs(&document.paragraphs.to_vec()));
      cell.document_bytes = crate::document::db8_bytes(&document)?;
      Ok(())
    })?;
    Ok(true)
  }

  pub fn move_cell(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    target_column: usize,
    target_index: usize,
    new_parent: Option<CellId>,
  ) -> anyhow::Result<()> {
    let intent = new_parent.map_or(
      FlowDropIntent::RootInColumn {
        column_index: target_column,
        insertion_index: target_index,
      },
      FlowDropIntent::LastChildOf,
    );
    self.move_cell_subtree(sheet_id, cell_id, intent)
  }

  pub fn move_cell_subtree(&mut self, sheet_id: SheetId, cell_id: CellId, intent: FlowDropIntent) -> anyhow::Result<()> {
    self.update(|projection| {
      let column_ids = sheet_column_ids(projection, sheet_id)?;
      let sheet = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?;
      apply_move_subtree(sheet, &column_ids, cell_id, intent)
    })
  }

  /// Read-only preview of [`Self::move_cell_subtree`]: returns a clone of the sheet with the drag
  /// already applied, or `None` if the move is invalid (self/descendant drop, or a descendant would
  /// fall outside the sheet columns). Nothing is committed, so this is safe to call every drag frame.
  pub fn preview_move_cell_subtree(&self, sheet_id: SheetId, cell_id: CellId, intent: FlowDropIntent) -> Option<Sheet> {
    let projection = self.projection();
    let column_ids = sheet_column_ids(projection, sheet_id).ok()?;
    let mut sheet = projection
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)?
      .clone();
    apply_move_subtree(&mut sheet, &column_ids, cell_id, intent).ok()?;
    // Only report a landing the real (fully validated) move would accept, so the drag preview never
    // shows the cell dropping somewhere the commit will silently reject.
    sheet_topology_ok(&sheet, &column_ids).then_some(sheet)
  }

  /// Read-only preview of the sheet with the dragged cell and its whole subtree lifted out, so the
  /// board can reflow as if it were gone while a drag is in flight and no valid target is hovered yet.
  pub fn preview_without_subtree(&self, sheet_id: SheetId, cell_id: CellId) -> Option<Sheet> {
    let sheet = self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)?;
    let subtree: HashSet<CellId> = subtree_cell_ids(sheet, cell_id).into_iter().collect();
    if subtree.is_empty() {
      return None;
    }
    let mut sheet = sheet.clone();
    sheet.cells.retain(|cell| !subtree.contains(&cell.id));
    Some(sheet)
  }

  /// The dragged cell plus every descendant, in sheet order. Empty if the cell is unknown.
  pub fn subtree_cell_ids_for(&self, sheet_id: SheetId, cell_id: CellId) -> Vec<CellId> {
    self
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .map(|sheet| subtree_cell_ids(sheet, cell_id))
      .unwrap_or_default()
  }

  pub fn add_annotation(&mut self, sheet_id: SheetId, stroke: AnnotationStroke) -> anyhow::Result<()> {
    self.update(|projection| {
      if stroke.sheet_id != sheet_id {
        bail!("annotation sheet id mismatch");
      }
      projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?
        .annotations
        .push(stroke);
      Ok(())
    })
  }

  pub fn clear_annotations(&mut self, sheet_id: SheetId, originator: &AnnotationOriginator) -> anyhow::Result<()> {
    self.update(|projection| {
      let sheet = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?;
      sheet
        .annotations
        .retain(|stroke| &stroke.originator != originator);
      Ok(())
    })
  }

  pub fn clear_all_annotations(&mut self, originator: &AnnotationOriginator) -> anyhow::Result<()> {
    self.update(|projection| {
      for sheet in &mut projection.sheets {
        sheet
          .annotations
          .retain(|stroke| &stroke.originator != originator);
      }
      Ok(())
    })
  }

  pub fn delete_annotation(&mut self, sheet_id: SheetId, stroke_id: uuid::Uuid, originator: &AnnotationOriginator) -> anyhow::Result<bool> {
    let mut removed = false;
    self.update(|projection| {
      let sheet = projection
        .sheets
        .iter_mut()
        .find(|sheet| sheet.id == sheet_id)
        .context("unknown sheet")?;
      let before = sheet.annotations.len();
      sheet
        .annotations
        .retain(|stroke| stroke.id != stroke_id || &stroke.originator != originator);
      removed = sheet.annotations.len() != before;
      Ok(())
    })?;
    Ok(removed)
  }
}

fn is_descendant_of(sheet: &Sheet, cell_id: CellId, ancestor_id: CellId) -> bool {
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

fn subtree_cell_ids(sheet: &Sheet, root_id: CellId) -> Vec<CellId> {
  sheet
    .cells
    .iter()
    .filter(|cell| cell.id == root_id || is_descendant_of(sheet, cell.id, root_id))
    .map(|cell| cell.id)
    .collect()
}

/// Cheap structural check of the two move-sensitive invariants — parent/child column adjacency and
/// contiguous sibling runs per column — without deserializing any cell documents. A move never
/// touches cell content, so if the sheet was valid before, this is exactly the set of failures a move
/// can introduce, matching what [`FlowProjection::validate`] would reject.
fn sheet_topology_ok(sheet: &Sheet, column_ids: &[Uuid]) -> bool {
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

fn sheet_column_ids(projection: &FlowProjection, sheet_id: SheetId) -> anyhow::Result<Vec<Uuid>> {
  let sheet = projection
    .sheets
    .iter()
    .find(|sheet| sheet.id == sheet_id)
    .context("unknown sheet")?;
  let definition = projection
    .format
    .sheet_type(sheet.sheet_type_id)
    .context("unknown sheet type")?;
  Ok(definition.columns.iter().map(|column| column.id).collect())
}

/// Move `cell_id` (and its subtree) within `sheet` according to `intent`. Pure over the sheet so the
/// same algorithm backs both the committing [`FlowDocument::move_cell_subtree`] and the read-only
/// drag previews.
fn apply_move_subtree(sheet: &mut Sheet, column_ids: &[Uuid], cell_id: CellId, intent: FlowDropIntent) -> anyhow::Result<()> {
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

fn resolve_drop_intent(
  sheet: &Sheet,
  column_ids: &[uuid::Uuid],
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{BoardPoint, BoardRect, StrokeStyle};

  fn document_with_sheet() -> (FlowDocument, SheetId) {
    let mut document = FlowDocument::new();
    let sheet_type = document.projection().format.sheet_types[0].id;
    let sheet = document.create_sheet("Case", sheet_type).unwrap();
    (document, sheet)
  }

  #[test]
  fn deleting_parent_orphans_children() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();
    document.delete_cell(sheet, parent).unwrap();
    assert_eq!(
      document.projection().sheets[0]
        .cells
        .iter()
        .find(|cell| cell.id == child)
        .unwrap()
        .parent_id,
      None
    );
  }

  #[test]
  fn moving_cell_down_uses_pre_removal_insertion_index() {
    let (mut document, sheet_id) = document_with_sheet();
    let first = document.add_plain_cell(sheet_id, 0, None, None).unwrap();
    let second = document.add_plain_cell(sheet_id, 0, None, None).unwrap();
    let third = document.add_plain_cell(sheet_id, 0, None, None).unwrap();

    document.move_cell(sheet_id, first, 0, 2, None).unwrap();

    let ids: Vec<_> = document.projection().sheets[0]
      .cells
      .iter()
      .map(|cell| cell.id)
      .collect();
    assert_eq!(ids, vec![second, first, third]);
  }

  #[test]
  fn unsupported_cell_content_becomes_an_editable_tag_without_losing_text() {
    let (mut document, sheet_id) = document_with_sheet();
    let cell_id = document.add_plain_cell(sheet_id, 0, None, None).unwrap();
    let source = flowstate_document::document_from_input(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputParagraph {
        style: flowstate_document::ParagraphStyle::Normal,
        runs: vec![flowstate_document::InputRun {
          text: "Preserve me".into(),
          styles: flowstate_document::RunStyles::default(),
        }],
      }],
    );
    document
      .replace_cell_document(sheet_id, cell_id, &source)
      .unwrap();

    assert!(
      document
        .ensure_cell_editable_projection(sheet_id, cell_id)
        .unwrap()
    );
    let cell = &document.projection().sheets[0].cells[0];
    assert_eq!(cell.summary_text().unwrap(), "Preserve me");
    assert_eq!(cell.document().unwrap().paragraphs[0].style, flowstate_document::PARAGRAPH_TAG);
  }

  #[test]
  fn quick_responses_append_after_existing_children() {
    let (mut document, sheet_id) = document_with_sheet();
    let parent = document.add_plain_cell(sheet_id, 0, None, None).unwrap();
    let first = document.add_response(sheet_id, parent).unwrap();
    let grandchild = document.add_response(sheet_id, first).unwrap();
    let second = document.add_response(sheet_id, parent).unwrap();

    let ids: Vec<_> = document.projection().sheets[0]
      .cells
      .iter()
      .map(|cell| cell.id)
      .collect();
    assert_eq!(ids, vec![parent, first, grandchild, second]);
  }

  #[test]
  fn deletion_fallback_prefers_previous_sibling_then_parent() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let first = document
      .add_plain_cell(sheet, 1, Some(parent), None)
      .unwrap();
    let second = document
      .add_plain_cell(sheet, 1, Some(parent), None)
      .unwrap();
    assert_eq!(document.deletion_fallback(sheet, second), Some(first));
    assert_eq!(document.deletion_fallback(sheet, first), Some(parent));
  }

  #[test]
  fn deletion_fallback_for_orphan_prefers_same_column_then_left_column() {
    let (mut document, sheet) = document_with_sheet();
    let left_first = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let left_last = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let orphan_first = document.add_plain_cell(sheet, 1, None, None).unwrap();
    let orphan_second = document.add_plain_cell(sheet, 1, None, None).unwrap();
    assert_eq!(document.deletion_fallback(sheet, orphan_second), Some(orphan_first));
    assert_eq!(document.deletion_fallback(sheet, orphan_first), Some(left_last));
    assert_eq!(document.deletion_fallback(sheet, left_first), None);
  }

  #[test]
  fn moving_parent_as_subtree_preserves_child_links() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();
    let grandchild = document.add_response(sheet, child).unwrap();
    document
      .move_cell_subtree(
        sheet,
        parent,
        FlowDropIntent::RootInColumn {
          column_index: 1,
          insertion_index: 0,
        },
      )
      .unwrap();
    let sheet = &document.projection().sheets[0];
    let parent_cell = sheet.cells.iter().find(|cell| cell.id == parent).unwrap();
    let child_cell = sheet.cells.iter().find(|cell| cell.id == child).unwrap();
    let grandchild_cell = sheet
      .cells
      .iter()
      .find(|cell| cell.id == grandchild)
      .unwrap();
    let columns = &document.projection().format.sheet_types[0].columns;
    assert_eq!(parent_cell.column_id, columns[1].id);
    assert_eq!(parent_cell.parent_id, None);
    assert_eq!(child_cell.column_id, columns[2].id);
    assert_eq!(child_cell.parent_id, Some(parent));
    assert_eq!(grandchild_cell.column_id, columns[3].id);
    assert_eq!(grandchild_cell.parent_id, Some(child));
  }

  #[test]
  fn moving_parent_subtree_keeps_descendants_contiguous() {
    let (mut document, sheet) = document_with_sheet();
    let other = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();
    let grandchild = document.add_response(sheet, child).unwrap();
    document
      .move_cell_subtree(sheet, parent, FlowDropIntent::BeforeSibling(other))
      .unwrap();
    let sheet = &document.projection().sheets[0];
    assert_eq!(
      sheet.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
      vec![parent, child, grandchild, other]
    );
  }

  #[test]
  fn preview_move_matches_the_committed_move() {
    let (mut document, sheet) = document_with_sheet();
    let first = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let second = document.add_plain_cell(sheet, 0, None, None).unwrap();

    let preview = document
      .preview_move_cell_subtree(sheet, first, FlowDropIntent::AfterSibling(second))
      .unwrap();
    document
      .move_cell_subtree(sheet, first, FlowDropIntent::AfterSibling(second))
      .unwrap();

    assert_eq!(
      preview.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(),
      document.projection().sheets[0]
        .cells
        .iter()
        .map(|cell| cell.id)
        .collect::<Vec<_>>()
    );
  }

  #[test]
  fn preview_without_subtree_lifts_the_whole_family() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let _child = document.add_response(sheet, parent).unwrap();
    let other = document.add_plain_cell(sheet, 0, None, None).unwrap();

    let lifted = document.preview_without_subtree(sheet, parent).unwrap();

    assert_eq!(lifted.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(), vec![other]);
  }

  #[test]
  fn preview_move_rejects_targets_that_would_split_a_sibling_run() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let _first = document.add_response(sheet, parent).unwrap();
    let second = document.add_response(sheet, parent).unwrap();
    let orphan = document.add_plain_cell(sheet, 1, None, None).unwrap();

    // Dropping the parentless `orphan` between `parent`'s two responses would split their sibling run,
    // which the committed move rejects — so the preview must decline to show a landing there.
    let insertion_index = document.projection().sheets[0]
      .cells
      .iter()
      .position(|cell| cell.id == second)
      .unwrap();
    let intent = FlowDropIntent::RootInColumn {
      column_index: 1,
      insertion_index,
    };

    assert!(
      document
        .preview_move_cell_subtree(sheet, orphan, intent)
        .is_none()
    );
    assert!(document.move_cell_subtree(sheet, orphan, intent).is_err());
  }

  #[test]
  fn preview_move_onto_descendant_returns_none() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();

    assert!(
      document
        .preview_move_cell_subtree(sheet, parent, FlowDropIntent::LastChildOf(child))
        .is_none()
    );
  }

  #[test]
  fn moving_cell_onto_descendant_is_rejected() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();

    let error = document
      .move_cell_subtree(sheet, parent, FlowDropIntent::LastChildOf(child))
      .unwrap_err();

    assert!(error.to_string().contains("descendants"));
    let sheet = &document.projection().sheets[0];
    assert_eq!(sheet.cells.iter().map(|cell| cell.id).collect::<Vec<_>>(), vec![parent, child]);
    assert_eq!(sheet.cells[1].parent_id, Some(parent));
  }

  #[test]
  fn undo_and_redo_restore_projection() {
    let (mut document, sheet) = document_with_sheet();
    document.add_plain_cell(sheet, 0, None, None).unwrap();
    assert_eq!(document.projection().sheets[0].cells.len(), 1);
    assert!(document.undo().unwrap());
    assert!(document.projection().sheets[0].cells.is_empty());
    assert!(document.redo().unwrap());
    assert_eq!(document.projection().sheets[0].cells.len(), 1);
  }

  #[test]
  fn newly_added_top_orphan_precedes_every_existing_run() {
    let (mut document, sheet) = document_with_sheet();
    let first = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let second = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let newest = document.add_orphan_at_column_top(sheet, 1).unwrap();
    let cells = &document.projection().sheets[0].cells;
    assert_eq!(cells.iter().map(|cell| cell.id).collect::<Vec<_>>(), vec![newest, first, second]);
  }

  #[test]
  fn striking_cell_updates_all_serialized_paragraph_runs() {
    let (mut document, sheet) = document_with_sheet();
    let cell = document.add_plain_cell(sheet, 0, None, None).unwrap();
    document.strike_cell(sheet, cell).unwrap();
    let cell = &document.projection().sheets[0].cells[0];
    let rich_text = cell.document().unwrap();
    assert!(
      rich_text
        .paragraphs
        .iter()
        .flat_map(|paragraph| &paragraph.runs)
        .all(|run| run.styles.strikethrough)
    );
    assert!(rich_text.blocks.iter().all(|block| match block {
      flowstate_document::Block::Paragraph(paragraph) => paragraph.runs.iter().all(|run| run.styles.strikethrough),
      _ => true,
    }));
    document.strike_cell(sheet, cell.id).unwrap();
    let rich_text = document.projection().sheets[0].cells[0].document().unwrap();
    assert!(
      rich_text
        .paragraphs
        .iter()
        .flat_map(|paragraph| &paragraph.runs)
        .all(|run| !run.styles.strikethrough)
    );
  }

  #[test]
  fn first_response_precedes_existing_child_subtrees() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let existing = document.add_response(sheet, parent).unwrap();
    let grandchild = document.add_response(sheet, existing).unwrap();
    let first = document.add_first_response(sheet, parent).unwrap();
    let ids = document.projection().sheets[0]
      .cells
      .iter()
      .map(|cell| cell.id)
      .collect::<Vec<_>>();
    assert_eq!(ids, vec![parent, first, existing, grandchild]);
  }

  #[test]
  fn concurrent_annotation_adds_merge_additively() {
    let (base, sheet) = document_with_sheet();
    let snapshot = base.snapshot().unwrap();
    let mut one = FlowDocument::from_snapshot(&snapshot).unwrap();
    let mut two = FlowDocument::from_snapshot(&snapshot).unwrap();
    one
      .add_annotation(sheet, test_stroke(sheet, "one"))
      .unwrap();
    two
      .add_annotation(sheet, test_stroke(sheet, "two"))
      .unwrap();
    let one_updates = one.updates_since(&two.version_vector()).unwrap();
    let two_updates = two.updates_since(&one.version_vector()).unwrap();
    one.import_updates(&two_updates).unwrap();
    two.import_updates(&one_updates).unwrap();
    assert_eq!(one.projection().sheets[0].annotations.len(), 2);
    assert_eq!(two.projection().sheets[0].annotations.len(), 2);
  }

  #[test]
  fn clearing_annotations_removes_only_originators_own_strokes_across_sheets() {
    let (mut document, first_sheet) = document_with_sheet();
    let sheet_type = document.projection().format.sheet_types[0].id;
    let second_sheet = document.create_sheet("Other", sheet_type).unwrap();
    for sheet in [first_sheet, second_sheet] {
      document
        .add_annotation(sheet, test_stroke(sheet, "local"))
        .unwrap();
      document
        .add_annotation(sheet, test_stroke(sheet, "collaborator"))
        .unwrap();
    }

    document
      .clear_all_annotations(&AnnotationOriginator("local".into()))
      .unwrap();

    assert!(
      document
        .projection()
        .sheets
        .iter()
        .all(|sheet| { sheet.annotations.len() == 1 && sheet.annotations[0].originator == AnnotationOriginator("collaborator".into()) })
    );
  }

  #[test]
  fn concurrent_cell_adds_merge_by_stable_id() {
    let (base, sheet) = document_with_sheet();
    let snapshot = base.snapshot().unwrap();
    let mut one = FlowDocument::from_snapshot(&snapshot).unwrap();
    let mut two = FlowDocument::from_snapshot(&snapshot).unwrap();
    one.add_plain_cell(sheet, 0, None, None).unwrap();
    two.add_plain_cell(sheet, 0, None, None).unwrap();
    exchange_updates(&mut one, &mut two);
    assert_eq!(one.projection().sheets[0].cells.len(), 2);
    assert_eq!(two.projection().sheets[0].cells.len(), 2);
  }

  fn exchange_updates(one: &mut FlowDocument, two: &mut FlowDocument) {
    let one_updates = one.updates_since(&two.version_vector()).unwrap();
    let two_updates = two.updates_since(&one.version_vector()).unwrap();
    one.import_updates(&two_updates).unwrap();
    two.import_updates(&one_updates).unwrap();
  }

  fn test_stroke(sheet_id: SheetId, originator: &str) -> AnnotationStroke {
    AnnotationStroke {
      id: Uuid::new_v4(),
      sheet_id,
      originator: AnnotationOriginator(originator.into()),
      points: vec![BoardPoint { x: 0.0, y: 0.0 }, BoardPoint { x: 2.0, y: 2.0 }],
      style: StrokeStyle {
        color_rgba: 0xff00_00ff,
        width: 2.0,
        opacity: 0.5,
      },
      bbox: BoardRect {
        min: BoardPoint { x: 0.0, y: 0.0 },
        max: BoardPoint { x: 2.0, y: 2.0 },
      },
    }
  }
}
