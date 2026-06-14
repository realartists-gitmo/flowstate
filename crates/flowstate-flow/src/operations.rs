use anyhow::{Context as _, bail};
use uuid::Uuid;

use crate::{AnnotationOriginator, AnnotationStroke, Cell, CellId, FlowDocument, Sheet, SheetId, SheetTypeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativePosition {
  Before,
  After,
}

impl FlowDocument {
  pub fn child_append_index(&self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
    let parent_index = sheet.cells.iter().position(|cell| cell.id == parent_id).context("unknown cell")?;
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
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
    let parent_index = sheet.cells.iter().position(|cell| cell.id == parent_id).context("unknown cell")?;
    Ok(parent_index + 1)
  }

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

  pub fn add_orphan_at_column_top(&mut self, sheet_id: SheetId, column_index: usize) -> anyhow::Result<CellId> {
    self.add_plain_cell(sheet_id, column_index, None, Some(0))
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
    let insertion = self.child_append_index(sheet_id, parent_id)?;
    self.add_plain_cell(sheet_id, child_column, Some(parent_id), Some(insertion))
  }

  pub fn add_first_response(&mut self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<CellId> {
    let sheet = self.projection().sheets.iter().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
    let parent = sheet.cells.iter().find(|cell| cell.id == parent_id).context("unknown cell")?;
    let definition = self.projection().format.sheet_type(sheet.sheet_type_id).context("unknown sheet type")?;
    let parent_column = definition.columns.iter().position(|column| column.id == parent.column_id).context("unknown column")?;
    let child_column = parent_column + 1;
    if child_column >= definition.columns.len() {
      bail!("rightmost cells cannot receive responses");
    }
    let insertion = self.child_prepend_index(sheet_id, parent_id)?;
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
      let struck = paragraphs.iter().flat_map(|paragraph| &paragraph.runs).all(|run| run.styles.strikethrough);
      for paragraph in paragraphs {
        for run in &mut paragraph.runs {
          run.styles.strikethrough = !struck;
        }
        paragraph.version = paragraph.version.wrapping_add(1);
      }
      document.blocks = std::sync::Arc::new(flowstate_document::paragraph_blocks_from_paragraphs(&document.paragraphs));
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
      let paragraphs = std::sync::Arc::make_mut(&mut document.paragraphs);
      let paragraph = paragraphs.first_mut().context("cell document has no paragraph")?;
      paragraph.style = flowstate_document::PARAGRAPH_TAG;
      paragraph.version = paragraph.version.wrapping_add(1);
      document.blocks = std::sync::Arc::new(flowstate_document::paragraph_blocks_from_paragraphs(&document.paragraphs));
      cell.document_bytes = flowstate_document::db8_bytes(&document)?;
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
    self.update(|projection| {
      let sheet = projection.sheets.iter_mut().find(|sheet| sheet.id == sheet_id).context("unknown sheet")?;
      let definition = projection.format.sheet_type(sheet.sheet_type_id).context("unknown sheet type")?;
      let column_id = definition.columns.get(target_column).context("column index out of range")?.id;
      let source = sheet.cells.iter().position(|cell| cell.id == cell_id).context("unknown cell")?;
      let mut cell = sheet.cells.remove(source);
      cell.column_id = column_id;
      cell.parent_id = new_parent;
      for child in &mut sheet.cells {
        if child.parent_id == Some(cell_id) {
          child.parent_id = None;
        }
      }
      let target_index = target_index.saturating_sub(usize::from(source < target_index));
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

  pub fn clear_all_annotations(&mut self, originator: &AnnotationOriginator) -> anyhow::Result<()> {
    self.update(|projection| {
      for sheet in &mut projection.sheets {
        sheet.annotations.retain(|stroke| &stroke.originator != originator);
      }
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

fn is_descendant_of(sheet: &Sheet, cell_id: CellId, ancestor_id: CellId) -> bool {
  let mut parent = sheet.cells.iter().find(|cell| cell.id == cell_id).and_then(|cell| cell.parent_id);
  while let Some(parent_id) = parent {
    if parent_id == ancestor_id {
      return true;
    }
    parent = sheet.cells.iter().find(|cell| cell.id == parent_id).and_then(|cell| cell.parent_id);
  }
  false
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
  fn moving_cell_down_uses_pre_removal_insertion_index() {
    let (mut document, sheet_id) = document_with_sheet();
    let first = document.add_plain_cell(sheet_id, 0, None, None).unwrap();
    let second = document.add_plain_cell(sheet_id, 0, None, None).unwrap();
    let third = document.add_plain_cell(sheet_id, 0, None, None).unwrap();

    document.move_cell(sheet_id, first, 0, 2, None).unwrap();

    let ids: Vec<_> = document.projection().sheets[0].cells.iter().map(|cell| cell.id).collect();
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
    document.replace_cell_document(sheet_id, cell_id, &source).unwrap();

    assert!(document.ensure_cell_editable_projection(sheet_id, cell_id).unwrap());
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

    let ids: Vec<_> = document.projection().sheets[0].cells.iter().map(|cell| cell.id).collect();
    assert_eq!(ids, vec![parent, first, grandchild, second]);
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
  fn moving_parent_orphans_children_and_leaves_them_in_place() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let child = document.add_response(sheet, parent).unwrap();
    let grandchild = document.add_response(sheet, child).unwrap();
    let child_column = document.projection().sheets[0].cells.iter().find(|cell| cell.id == child).unwrap().column_id;
    let grandchild_column = document.projection().sheets[0].cells.iter().find(|cell| cell.id == grandchild).unwrap().column_id;
    document.move_cell(sheet, parent, 1, 0, None).unwrap();
    let sheet = &document.projection().sheets[0];
    let child = sheet.cells.iter().find(|cell| cell.id == child).unwrap();
    let grandchild = sheet.cells.iter().find(|cell| cell.id == grandchild).unwrap();
    assert_eq!(child.column_id, child_column);
    assert_eq!(child.parent_id, None);
    assert_eq!(grandchild.column_id, grandchild_column);
    assert_eq!(grandchild.parent_id, Some(child.id));
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
    assert!(rich_text.paragraphs.iter().flat_map(|paragraph| &paragraph.runs).all(|run| run.styles.strikethrough));
    assert!(rich_text.blocks.iter().all(|block| match block {
      flowstate_document::Block::Paragraph(paragraph) => paragraph.runs.iter().all(|run| run.styles.strikethrough),
      _ => true,
    }));
    document.strike_cell(sheet, cell.id).unwrap();
    let rich_text = document.projection().sheets[0].cells[0].document().unwrap();
    assert!(rich_text.paragraphs.iter().flat_map(|paragraph| &paragraph.runs).all(|run| !run.styles.strikethrough));
  }

  #[test]
  fn first_response_precedes_existing_child_subtrees() {
    let (mut document, sheet) = document_with_sheet();
    let parent = document.add_plain_cell(sheet, 0, None, None).unwrap();
    let existing = document.add_response(sheet, parent).unwrap();
    let grandchild = document.add_response(sheet, existing).unwrap();
    let first = document.add_first_response(sheet, parent).unwrap();
    let ids = document.projection().sheets[0].cells.iter().map(|cell| cell.id).collect::<Vec<_>>();
    assert_eq!(ids, vec![parent, first, existing, grandchild]);
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
  fn clearing_annotations_removes_only_originators_own_strokes_across_sheets() {
    let (mut document, first_sheet) = document_with_sheet();
    let sheet_type = document.projection().format.sheet_types[0].id;
    let second_sheet = document.create_sheet("Other", sheet_type).unwrap();
    for sheet in [first_sheet, second_sheet] {
      document.add_annotation(sheet, test_stroke(sheet, "local")).unwrap();
      document.add_annotation(sheet, test_stroke(sheet, "collaborator")).unwrap();
    }

    document.clear_all_annotations(&AnnotationOriginator("local".into())).unwrap();

    assert!(document.projection().sheets.iter().all(|sheet| {
      sheet.annotations.len() == 1 && sheet.annotations[0].originator == AnnotationOriginator("collaborator".into())
    }));
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
