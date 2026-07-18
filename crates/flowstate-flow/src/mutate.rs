//! Intent EXECUTORS (excel flow spec §2): resolve a [`FlowIntent`] against
//! the CURRENT board projection (reject before any mutation), then write the
//! minimal Loro ops. One implementation, two entrances: the schema-level
//! [`crate::FlowDocument::apply_intent`] (solo/fixtures/tests) and the
//! runtime commit path in `flowstate-collab/src/flow` (gate + undo + streams)
//! both land here, so their semantics cannot drift.
//!
//! Op-shape law: a move/reorder touches ONLY order lists or the moved cell's
//! address registers — never any flow container — so it can never clobber a
//! concurrent text edit. Occupied-slot rejection happens here at execute
//! time; concurrent-merge collisions are the normalizer's job (D2), never
//! the executor's.

use anyhow::{Context as _, bail};
use loro::LoroDoc;

use crate::format::{CellId, ColumnId, RowId};
use crate::intents::{AnnotationScope, CellSeed, FlowIntent};
use crate::loro_schema::{
  self, annotations_map, cell_flow, cell_paragraph_registry, cell_record, cells_map, column_record, ensure_cell_record, ensure_column_record,
  ensure_sheet_record, list_strings, map_binary, set_cell_column, set_cell_row, sheet_column_order, sheet_order, sheet_record, sheet_row_heights,
  sheet_row_order,
};
use crate::projection::{AnnotationOriginator, FlowBoardProjection, Sheet};

/// What an execution touched — the runtime's stream-classification input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MutationReport {
  /// Cells whose FLOW CONTENT changed (structural-only intents leave this
  /// empty; the board projection changes on every intent).
  pub content_cells: Vec<CellId>,
}

pub fn execute_intent(doc: &LoroDoc, board: &FlowBoardProjection, intent: &FlowIntent) -> anyhow::Result<MutationReport> {
  match intent {
    FlowIntent::CreateSheet {
      sheet_id,
      name,
      sheet_type_id,
    } => {
      if board.sheet(*sheet_id).is_some() {
        bail!("sheet {sheet_id} already exists");
      }
      let definition = board
        .format
        .sheet_type(*sheet_type_id)
        .context("unknown sheet type")?;
      ensure_sheet_record(doc, *sheet_id, name, *sheet_type_id, &definition.columns)?;
      sheet_order(doc).insert(sheet_order(doc).len(), sheet_id.to_string())?;
      Ok(MutationReport::default())
    },
    FlowIntent::RenameSheet { sheet_id, name } => {
      let record = sheet_record(doc, *sheet_id).context("unknown sheet")?;
      record.insert("name", name.as_str())?;
      Ok(MutationReport::default())
    },
    FlowIntent::DeleteSheet { sheet_id } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      let cells = cells_map(doc);
      for cell in sheet.cells() {
        if cells.get(&cell.id.to_string()).is_some() {
          cells.delete(&cell.id.to_string())?;
        }
      }
      // I-S1: sweep the sheet's ink — strokes referencing a dead sheet must
      // not linger unrendered and unreachable.
      let annotations = annotations_map(doc);
      for key in loro_schema::map_keys(&annotations) {
        let Some(bytes) = map_binary(&annotations, &key) else { continue };
        let Ok(stroke) = postcard::from_bytes::<crate::projection::AnnotationStroke>(&bytes) else {
          continue;
        };
        if stroke.sheet_id == *sheet_id {
          annotations.delete(&key)?;
        }
      }
      let order = sheet_order(doc);
      if let Some(index) = list_strings(&order)
        .iter()
        .position(|entry| entry == &sheet_id.to_string())
      {
        order.delete(index, 1)?;
      }
      if sheet_record(doc, *sheet_id).is_some() {
        loro_schema::sheets_map(doc).delete(&sheet_id.to_string())?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::MoveSheet { sheet_id, before } => {
      let order = sheet_order(doc);
      move_entry(&order, &sheet_id.to_string(), before.map(|anchor| anchor.to_string()).as_deref())?;
      Ok(MutationReport::default())
    },
    FlowIntent::InsertRows { sheet_id, before, row_ids } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if row_ids.is_empty() {
        bail!("no rows to insert");
      }
      for row_id in row_ids {
        if sheet.row_index(*row_id).is_some() {
          bail!("row {row_id} already exists");
        }
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_row_order(&record)?;
      let entries = list_strings(&order);
      let index = match before {
        Some(anchor) => entries
          .iter()
          .position(|entry| entry == &anchor.to_string())
          .context("unknown anchor row")?,
        None => entries.len(),
      };
      for (offset, row_id) in row_ids.iter().enumerate() {
        order.insert((index + offset).min(order.len()), row_id.to_string())?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::MoveRows { sheet_id, row_ids, before } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if row_ids.is_empty() {
        bail!("no rows to move");
      }
      for row_id in row_ids {
        sheet
          .row_index(*row_id)
          .with_context(|| format!("unknown row {row_id}"))?;
      }
      if let Some(anchor) = before
        && row_ids.contains(anchor)
      {
        bail!("anchor row is part of the moved selection");
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_row_order(&record)?;
      // Desired final order: non-members keep relative order; members land
      // (in intent order) immediately before the anchor.
      let moved: Vec<String> = row_ids.iter().map(|row| row.to_string()).collect();
      let current = list_strings(&order);
      let mut desired: Vec<String> = Vec::with_capacity(current.len());
      for entry in &current {
        if moved.contains(entry) {
          continue;
        }
        if before.is_some_and(|anchor| entry == &anchor.to_string()) {
          desired.extend(moved.iter().cloned());
        }
        desired.push(entry.clone());
      }
      if before.is_none() {
        desired.extend(moved.iter().cloned());
      }
      // Transform via `mov` for ONLY the moved members, ascending final
      // position (non-members keep their relative order, so this converges).
      for (desired_ix, entry) in desired.iter().enumerate() {
        if !moved.contains(entry) {
          continue;
        }
        let current = list_strings(&order);
        let Some(from) = current.iter().position(|candidate| candidate == entry) else {
          continue;
        };
        let to = desired_ix.min(current.len().saturating_sub(1));
        if from != to {
          order.mov(from, to)?;
        }
      }
      Ok(MutationReport::default())
    },
    FlowIntent::DeleteRows { sheet_id, row_ids } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if row_ids.is_empty() {
        bail!("no rows to delete");
      }
      for row_id in row_ids {
        sheet
          .row_index(*row_id)
          .with_context(|| format!("unknown row {row_id}"))?;
      }
      let cells = cells_map(doc);
      for row_id in row_ids {
        let Some(row_ix) = sheet.row_index(*row_id) else { continue };
        for cell in sheet.rows[row_ix].cells.iter().filter_map(Option::as_ref) {
          if cells.get(&cell.id.to_string()).is_some() {
            cells.delete(&cell.id.to_string())?;
          }
        }
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_row_order(&record)?;
      for row_id in row_ids {
        if let Some(index) = list_strings(&order)
          .iter()
          .position(|entry| entry == &row_id.to_string())
        {
          order.delete(index, 1)?;
        }
      }
      let heights = sheet_row_heights(&record)?;
      for row_id in row_ids {
        if heights.get(&row_id.to_string()).is_some() {
          heights.delete(&row_id.to_string())?;
        }
      }
      Ok(MutationReport::default())
    },
    FlowIntent::SetRowHeight { sheet_id, row_id, height } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      sheet.row_index(*row_id).context("unknown row")?;
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let heights = sheet_row_heights(&record)?;
      match height {
        Some(height) => heights.insert(&row_id.to_string(), f64::from(*height))?,
        None => {
          if heights.get(&row_id.to_string()).is_some() {
            heights.delete(&row_id.to_string())?;
          }
        },
      }
      Ok(MutationReport::default())
    },
    FlowIntent::AddColumn {
      sheet_id,
      column_id,
      label,
      side,
      before,
    } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if sheet.column_index(*column_id).is_some() {
        bail!("column {column_id} already exists");
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      ensure_column_record(&record, *column_id, label, *side)?;
      let order = sheet_column_order(&record)?;
      let entries = list_strings(&order);
      let index = match before {
        Some(anchor) => entries
          .iter()
          .position(|entry| entry == &anchor.to_string())
          .context("unknown anchor column")?,
        None => entries.len(),
      };
      order.insert(index.min(order.len()), column_id.to_string())?;
      Ok(MutationReport::default())
    },
    FlowIntent::RenameColumn { sheet_id, column_id, label } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      sheet.column_index(*column_id).context("unknown column")?;
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let column = column_record(&record, *column_id).context("unknown column record")?;
      column.insert("label", label.as_str())?;
      Ok(MutationReport::default())
    },
    FlowIntent::MoveColumn { sheet_id, column_id, before } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      sheet.column_index(*column_id).context("unknown column")?;
      if let Some(anchor) = before {
        if anchor == column_id {
          bail!("column cannot anchor on itself");
        }
        sheet.column_index(*anchor).context("unknown anchor column")?;
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_column_order(&record)?;
      move_entry(&order, &column_id.to_string(), before.map(|anchor| anchor.to_string()).as_deref())?;
      Ok(MutationReport::default())
    },
    FlowIntent::DeleteColumn { sheet_id, column_id } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      let column_ix = sheet.column_index(*column_id).context("unknown column")?;
      if sheet.columns.len() == 1 {
        bail!("a sheet's last column cannot be deleted");
      }
      let cells = cells_map(doc);
      for row in &sheet.rows {
        if let Some(cell) = row.cells[column_ix].as_ref()
          && cells.get(&cell.id.to_string()).is_some()
        {
          cells.delete(&cell.id.to_string())?;
        }
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_column_order(&record)?;
      if let Some(index) = list_strings(&order)
        .iter()
        .position(|entry| entry == &column_id.to_string())
      {
        order.delete(index, 1)?;
      }
      let columns = loro_schema::sheet_columns_map(&record)?;
      if columns.get(&column_id.to_string()).is_some() {
        columns.delete(&column_id.to_string())?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::SetColumnWidth { sheet_id, column_id, width } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      sheet.column_index(*column_id).context("unknown column")?;
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let column = column_record(&record, *column_id).context("unknown column record")?;
      loro_schema::set_column_width(&column, *width)?;
      Ok(MutationReport::default())
    },
    FlowIntent::AddCell {
      sheet_id,
      cell_id,
      row_id,
      column_id,
      seed,
    } => {
      // Duplicate-id guard: an id may exist ANYWHERE (another sheet included).
      if cell_record(doc, *cell_id).is_some() {
        bail!("cell {cell_id} already exists");
      }
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      resolve_empty_slot(sheet, *row_id, *column_id, None)?;
      ensure_cell_record(doc, *cell_id, *sheet_id, *row_id, *column_id)?;
      match seed {
        CellSeed::Empty => loro_schema::seed_cell_flow(doc, *cell_id)?,
        CellSeed::Paragraphs(paragraphs) => write_cell_paragraphs(doc, *cell_id, paragraphs.clone())?,
      }
      Ok(MutationReport {
        content_cells: vec![*cell_id],
      })
    },
    FlowIntent::DeleteCell { sheet_id, cell_id } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if sheet.find_cell(*cell_id).is_none() {
        bail!("unknown cell {cell_id}");
      }
      let cells = cells_map(doc);
      if cells.get(&cell_id.to_string()).is_some() {
        cells.delete(&cell_id.to_string())?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::SetCellAddress {
      sheet_id,
      cell_id,
      row_id,
      column_id,
    } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      let cell = sheet.find_cell(*cell_id).context("unknown cell")?;
      resolve_empty_slot(sheet, *row_id, *column_id, Some(*cell_id))?;
      let (previous_row, previous_column) = (cell.row_id, cell.column_id);
      let record = cell_record(doc, *cell_id).context("unknown cell record")?;
      if previous_row != *row_id {
        set_cell_row(&record, *row_id)?;
      }
      if previous_column != *column_id {
        set_cell_column(&record, *column_id)?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::SwapCells { sheet_id, a, b } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if a == b {
        return Ok(MutationReport::default());
      }
      let cell_a = sheet.find_cell(*a).context("unknown cell")?;
      let cell_b = sheet.find_cell(*b).context("unknown cell")?;
      let (a_row, a_column) = (cell_a.row_id, cell_a.column_id);
      let (b_row, b_column) = (cell_b.row_id, cell_b.column_id);
      let record_a = cell_record(doc, *a).context("unknown cell record")?;
      let record_b = cell_record(doc, *b).context("unknown cell record")?;
      // Both writes commit under one op set: the intermediate never surfaces,
      // so the swapped pair never trips the empty-slot guard.
      if a_row != b_row {
        set_cell_row(&record_a, b_row)?;
        set_cell_row(&record_b, a_row)?;
      }
      if a_column != b_column {
        set_cell_column(&record_a, b_column)?;
        set_cell_column(&record_b, a_column)?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::SetCellAddresses { sheet_id, placements } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if placements.is_empty() {
        return Ok(MutationReport::default());
      }
      let moving: std::collections::HashSet<CellId> = placements.iter().map(|(cell_id, _, _)| *cell_id).collect();
      let mut targets: std::collections::HashSet<(RowId, ColumnId)> = std::collections::HashSet::new();
      for (cell_id, row_id, column_id) in placements {
        if sheet.find_cell(*cell_id).is_none() {
          bail!("unknown cell {cell_id}");
        }
        if sheet.row_index(*row_id).is_none() {
          bail!("unknown row {row_id}");
        }
        if sheet.column_index(*column_id).is_none() {
          bail!("unknown column {column_id}");
        }
        if !targets.insert((*row_id, *column_id)) {
          bail!("two cells placed on one address");
        }
        // A target may be occupied only by a cell that is itself moving.
        if let Some(occupant) = sheet.slot_by_ids(*row_id, *column_id)
          && !moving.contains(&occupant.id)
        {
          bail!("placement collides with cell {} outside the set", occupant.id);
        }
      }
      // All validated: apply every register write under one op set.
      for (cell_id, row_id, column_id) in placements {
        let cell = sheet.find_cell(*cell_id).context("unknown cell")?;
        let record = cell_record(doc, *cell_id).context("unknown cell record")?;
        if cell.row_id != *row_id {
          set_cell_row(&record, *row_id)?;
        }
        if cell.column_id != *column_id {
          set_cell_column(&record, *column_id)?;
        }
      }
      Ok(MutationReport::default())
    },
    FlowIntent::SetCellStruck { sheet_id, cell_id, struck } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if sheet.find_cell(*cell_id).is_none() {
        bail!("unknown cell {cell_id}");
      }
      set_cell_struck(doc, *cell_id, *struck)?;
      Ok(MutationReport {
        content_cells: vec![*cell_id],
      })
    },
    FlowIntent::EnsureCellEditable { sheet_id, cell_id } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if sheet.find_cell(*cell_id).is_none() {
        bail!("unknown cell {cell_id}");
      }
      let record = cell_record(doc, *cell_id).context("unknown cell record")?;
      let flow = cell_flow(&record).context("cell has no flow")?;
      let text = flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)?;
      let slot = flowstate_document::paragraph_slot(flowstate_document::PARAGRAPH_TAG).context("TAG style has no slot")?;
      if text.len_unicode() == 0 {
        loro_schema::seed_cell_flow(doc, *cell_id)?;
      } else {
        text.mark(0..1, flowstate_document::MARK_PARAGRAPH_STYLE, i64::from(slot))?;
      }
      Ok(MutationReport {
        content_cells: vec![*cell_id],
      })
    },
    FlowIntent::ReplaceCellContent {
      sheet_id,
      cell_id,
      paragraphs,
    } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if sheet.find_cell(*cell_id).is_none() {
        bail!("unknown cell {cell_id}");
      }
      write_cell_paragraphs(doc, *cell_id, paragraphs.clone())?;
      Ok(MutationReport {
        content_cells: vec![*cell_id],
      })
    },
    FlowIntent::AddAnnotation { stroke } => {
      board.sheet(stroke.sheet_id).context("unknown sheet")?;
      annotations_map(doc).insert(&stroke.id.to_string(), postcard::to_allocvec(stroke)?)?;
      Ok(MutationReport::default())
    },
    FlowIntent::DeleteAnnotation {
      sheet_id,
      stroke_id,
      originator,
    } => {
      let map = annotations_map(doc);
      let key = stroke_id.to_string();
      let Some(bytes) = map_binary(&map, &key) else {
        bail!("unknown annotation {stroke_id}");
      };
      // I-S2 hardening: an UNDECODABLE stroke must not be immortal — the old
      // `?` made corrupt blobs undeletable while the materializer silently
      // skipped them (invisible AND unremovable).
      match postcard::from_bytes::<crate::projection::AnnotationStroke>(&bytes) {
        Ok(stroke) => {
          if stroke.sheet_id != *sheet_id || &stroke.originator != originator {
            bail!("annotation {stroke_id} does not match sheet/originator");
          }
        },
        Err(error) => {
          tracing::warn!(%error, stroke = %stroke_id, "deleting undecodable annotation blob");
        },
      }
      map.delete(&key)?;
      Ok(MutationReport::default())
    },
    FlowIntent::ClearAnnotations { scope, originator } => {
      clear_annotations(doc, scope, originator)?;
      Ok(MutationReport::default())
    },
    FlowIntent::CellText { .. } => {
      bail!("cell text intents execute only through the gated flow runtime");
    },
  }
}

/// Occupied-slot rejection (executor law): the target address must exist and
/// be empty (or held by the moving cell itself).
fn resolve_empty_slot(sheet: &Sheet, row_id: RowId, column_id: ColumnId, moving: Option<CellId>) -> anyhow::Result<()> {
  if sheet.row_index(row_id).is_none() {
    bail!("unknown row {row_id}");
  }
  if sheet.column_index(column_id).is_none() {
    bail!("unknown column {column_id}");
  }
  if let Some(occupant) = sheet.slot_by_ids(row_id, column_id)
    && Some(occupant.id) != moving
  {
    bail!("slot is occupied by cell {}", occupant.id);
  }
  Ok(())
}

/// Move one entry of a `MovableList` before an anchor entry (`None` = end)
/// with a single `mov` op.
fn move_entry(order: &loro::LoroMovableList, entry: &str, before: Option<&str>) -> anyhow::Result<()> {
  let entries = list_strings(order);
  let from = entries
    .iter()
    .position(|candidate| candidate == entry)
    .context("unknown order entry")?;
  let mut target = match before {
    Some(anchor) => entries
      .iter()
      .position(|candidate| candidate == anchor)
      .context("unknown anchor entry")?,
    None => entries.len(),
  };
  if from < target {
    target -= 1;
  }
  if from != target {
    order.mov(from, target)?;
  }
  Ok(())
}

fn clear_annotations(doc: &LoroDoc, scope: &AnnotationScope, originator: &AnnotationOriginator) -> anyhow::Result<()> {
  let map = annotations_map(doc);
  for key in loro_schema::map_keys(&map) {
    let Some(bytes) = map_binary(&map, &key) else { continue };
    let Ok(stroke) = postcard::from_bytes::<crate::projection::AnnotationStroke>(&bytes) else {
      continue;
    };
    if &stroke.originator != originator {
      continue;
    }
    let in_scope = match scope {
      AnnotationScope::AllSheets => true,
      AnnotationScope::Sheet(sheet_id) => stroke.sheet_id == *sheet_id,
    };
    if in_scope {
      map.delete(&key)?;
    }
  }
  Ok(())
}

/// Strike = a `MARK_STRIKETHROUGH` mark over every paragraph's TEXT range
/// (sentinels excluded — sentinel hygiene), so concurrent typing merges under
/// it char-level instead of clobbering a whole cell.
fn set_cell_struck(doc: &LoroDoc, cell_id: CellId, struck: bool) -> anyhow::Result<()> {
  let record = cell_record(doc, cell_id).context("unknown cell record")?;
  let flow = cell_flow(&record).context("cell has no flow")?;
  let text = flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)?;
  let value = text.to_string();
  let mut start: Option<usize> = None;
  let mut ranges: Vec<(usize, usize)> = Vec::new();
  for (index, ch) in value.chars().enumerate() {
    if ch == '\n' {
      if let Some(begin) = start.take()
        && index > begin
      {
        ranges.push((begin, index));
      }
      start = Some(index + 1);
    } else if start.is_none() {
      start = Some(index);
    }
  }
  if let Some(begin) = start
    && value.chars().count() > begin
  {
    ranges.push((begin, value.chars().count()));
  }
  for (begin, end) in ranges {
    if struck {
      text.mark(begin..end, flowstate_document::MARK_STRIKETHROUGH, true)?;
    } else {
      text.unmark(begin..end, flowstate_document::MARK_STRIKETHROUGH)?;
    }
  }
  Ok(())
}

/// Write a cell's full rich text from input paragraphs (`AddCell` seeds and
/// `ReplaceCellContent`) through the shared single-flow import law.
pub fn write_cell_paragraphs(doc: &LoroDoc, cell_id: CellId, paragraphs: Vec<flowstate_document::InputParagraph>) -> anyhow::Result<()> {
  let record = cell_record(doc, cell_id).context("unknown cell record")?;
  let flow = cell_flow(&record).context("cell has no flow")?;
  let registry = cell_paragraph_registry(&flow).context("cell has no registry")?;
  let document = flowstate_document::document_from_input(
    flowstate_document::DocumentTheme::clone(&flowstate_document::flowstate_document_theme()),
    paragraphs,
  );
  flowstate_document::replace_single_flow_from_document(doc, &flow, &registry, &loro_schema::cell_flow_id(cell_id), &document)?;
  Ok(())
}

/// Replace a cell's content from a full editor projection (the transitional
/// solo write-back path until every surface rides the per-cell authority).
pub fn replace_cell_document(doc: &LoroDoc, cell_id: CellId, document: &flowstate_document::DocumentProjection) -> anyhow::Result<()> {
  let record = cell_record(doc, cell_id).context("unknown cell record")?;
  let flow = cell_flow(&record).context("cell has no flow")?;
  let registry = cell_paragraph_registry(&flow).context("cell has no registry")?;
  flowstate_document::replace_single_flow_from_document(doc, &flow, &registry, &loro_schema::cell_flow_id(cell_id), document)?;
  Ok(())
}
