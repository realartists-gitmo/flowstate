use anyhow::{Context as _, bail};
use uuid::Uuid;

use crate::{AnnotationOriginator, AnnotationStroke, Cell, CellId, FlowDocument, Sheet, SheetId, SheetTypeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativePosition {
  Before,
  After,
}

impl FlowDocument {
  pub fn deletion_fallback(&self, sheet_id: SheetId, cell_id: CellId) -> Option<CellId> {
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id)?;
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

    let column = definition.columns.iter().position(|column| column.id == cell.column_id)?;
    let left_column = column.checked_sub(1).and_then(|index| definition.columns.get(index))?.id;
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
      projection.format.sheet_type(sheet_type_id).context("unknown sheet type")?;
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
      let source = projection.sheets.iter().position(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
      let sheet = projection.sheets.remove(source);
      projection.sheets.insert(target_index.min(projection.sheets.len()), sheet);
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
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
    let definition = self.projection().format.sheet_type(sheet.sheet_type_id).context("unknown sheet type")?;
    let column_id = definition.columns.get(column_index).context("column index out of range")?.id;
    let mut cell = Cell::plain(column_id)?;
    cell.parent_id = parent_id;
    let id = cell.id;
    self.update(|projection| {
      let sheet = projection.sheets.iter_mut().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
      sheet.cells.insert(insertion_index.unwrap_or(sheet.cells.len()).min(sheet.cells.len()), cell);
      Ok(())
    })?;
    Ok(id)
  }

  pub fn add_sibling(&mut self, sheet_id: SheetId, cell_id: CellId, position: RelativePosition) -> anyhow::Result<CellId> {
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
    let index = sheet.cells.iter().position(|cell| cell.id == cell_id).context("unknown cell")?;
    let source = &sheet.cells[index];
    let definition = self.projection().format.sheet_type(sheet.sheet_type_id).context("unknown sheet type")?;
    let column = definition.columns.iter().position(|column| column.id == source.column_id).context("unknown column")?;
    let insertion = match position {
      RelativePosition::Before => index,
      RelativePosition::After => index + 1,
    };
    self.add_plain_cell(sheet_id, column, source.parent_id, Some(insertion))
  }

  pub fn add_response(&mut self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<CellId> {
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
    let parent = sheet.cells.iter().find(|cell| cell.id == parent_id).context("unknown cell")?;
    let definition = self.projection().format.sheet_type(sheet.sheet_type_id).context("unknown sheet type")?;
    let parent_column = definition.columns.iter().position(|column| column.id == parent.column_id).context("unknown column")?;
    let child_column = parent_column + 1;
    if child_column >= definition.columns.len() {
      bail!("rightmost cells cannot receive responses");
    }
    let insertion = sheet
      .cells
      .iter()
      .position(|cell| cell.parent_id == Some(parent_id))
      .unwrap_or(sheet.cells.len());
    self.add_plain_cell(sheet_id, child_column, Some(parent_id), Some(insertion))
  }

  pub fn delete_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    self.update(|projection| {
      let sheet = projection.sheets.iter_mut().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
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
      let paragraphs = std::sync::Arc::make_mut(&mut document.paragraphs);
      for paragraph in paragraphs {
        for run in &mut paragraph.runs {
          run.styles.strikethrough = true;
        }
      }
      cell.document_bytes = flowstate_document::db8_bytes(&document)?;
      Ok(())
    })
  }

  pub fn replace_cell_document(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    document: &flowstate_document::Document,
  ) -> anyhow::Result<()> {
    let bytes = flowstate_document::db8_bytes(document)?;
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

  pub fn move_cell(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    target_column: usize,
    target_index: usize,
    new_parent: Option<CellId>,
  ) -> anyhow::Result<()> {
    self.update(|projection| {
      let sheet = projection.sheets.iter_mut().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
      let definition = projection.format.sheet_type(sheet.sheet_type_id).context("unknown sheet type")?;
      let column_id = definition.columns.get(target_column).context("column index out of range")?.id;
      let source = sheet.cells.iter().position(|cell| cell.id == cell_id).context("unknown cell")?;
      let mut cell = sheet.cells.remove(source);
      let source_column = definition
        .columns
        .iter()
        .position(|column| column.id == cell.column_id)
        .context("unknown source column")?;
      let delta = target_column as isize - source_column as isize;
      cell.column_id = column_id;
      cell.parent_id = new_parent;
      let mut moving_parents = vec![cell_id];
      while let Some(parent_id) = moving_parents.pop() {
        let child_ids: Vec<_> = sheet
          .cells
          .iter()
          .filter(|descendant| descendant.parent_id == Some(parent_id))
          .map(|descendant| descendant.id)
          .collect();
        for child_id in child_ids {
          let descendant = sheet.cells.iter_mut().find(|candidate| candidate.id == child_id).context("missing descendant")?;
          let descendant_column = definition
            .columns
            .iter()
            .position(|column| column.id == descendant.column_id)
            .context("unknown descendant column")?;
          let destination = descendant_column as isize + delta;
          if let Ok(destination) = usize::try_from(destination)
            && let Some(column) = definition.columns.get(destination)
          {
            descendant.column_id = column.id;
            moving_parents.push(child_id);
          } else {
            descendant.parent_id = None;
          }
        }
      }
      sheet.cells.insert(target_index.min(sheet.cells.len()), cell);
      Ok(())
    })
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
      let sheet = projection.sheets.iter_mut().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
      sheet.annotations.retain(|stroke| &stroke.originator != originator);
      Ok(())
    })
  }

  pub fn delete_annotation(&mut self, sheet_id: SheetId, stroke_id: uuid::Uuid, originator: &AnnotationOriginator) -> anyhow::Result<bool> {
    let mut removed = false;
    self.update(|projection| {
      let sheet = projection.sheets.iter_mut().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
      let before = sheet.annotations.len();
      sheet.annotations.retain(|stroke| stroke.id != stroke_id || &stroke.originator != originator);
      removed = sheet.annotations.len() != before;
      Ok(())
    })?;
    Ok(removed)
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
    assert_eq!(document.projection().sheets[0].cells.iter().find(|cell| cell.id == child).unwrap().parent_id, None);
  }

  #[test]
  fn deletion_fallback_prefers_previous_sibling_then_parent() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let first = document.add_plain_cell(sheet, 1, Some(parent), None).unwrap();
    let second = document.add_plain_cell(sheet, 1, Some(parent), None).unwrap();
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
  fn moving_parent_away_orphans_invalid_children() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();
    document.move_cell(sheet, parent, 6, 0, None).unwrap();
    assert_eq!(document.projection().sheets[0].cells.iter().find(|cell| cell.id == child).unwrap().parent_id, None);
  }

  #[test]
  fn moving_parent_carries_valid_descendants() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();
    document.move_cell(sheet, parent, 1, 0, None).unwrap();
    let sheet = &document.projection().sheets[0];
    let definition = &document.projection().format.sheet_types[0];
    assert_eq!(sheet.cells.iter().find(|cell| cell.id == child).unwrap().column_id, definition.columns[2].id);
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
  fn concurrent_annotation_adds_merge_additively() {
    let (base, sheet) = document_with_sheet();
    let snapshot = base.snapshot().unwrap();
    let mut one = FlowDocument::from_snapshot(&snapshot).unwrap();
    let mut two = FlowDocument::from_snapshot(&snapshot).unwrap();
    one.add_annotation(sheet, test_stroke(sheet, "one")).unwrap();
    two.add_annotation(sheet, test_stroke(sheet, "two")).unwrap();
    let one_updates = one.updates_since(&two.version_vector()).unwrap();
    let two_updates = two.updates_since(&one.version_vector()).unwrap();
    one.import_updates(&two_updates).unwrap();
    two.import_updates(&one_updates).unwrap();
    assert_eq!(one.projection().sheets[0].annotations.len(), 2);
    assert_eq!(two.projection().sheets[0].annotations.len(), 2);
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
        color_rgba: 0xff00_0000,
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
