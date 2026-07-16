//! Intent EXECUTORS (flow architecture spec Part 2.2): resolve a
//! [`FlowIntent`] against the CURRENT board projection (reject before any
//! mutation), then write the minimal Loro ops. One implementation, two
//! entrances: the schema-level [`crate::FlowDocument::apply_intent`]
//! (solo/fixtures/tests) and the runtime commit path in
//! `flowstate-collab/src/flow` (gate + undo + streams) both land here, so
//! their semantics cannot drift.
//!
//! Op-shape law: a reorder/move touches ONLY order lists and the moved root's
//! registers — never any flow container — so it can never clobber a
//! concurrent text edit.

use anyhow::{Context as _, bail};
use loro::LoroDoc;

use crate::board_ops;
use crate::format::{CellId, SheetId};
use crate::intents::{AnnotationScope, CellSeed, FlowDropIntent, FlowIntent};
use crate::loro_schema::{
  self, annotations_map, cell_flow, cell_paragraph_registry, cell_record, cells_map, ensure_cell_record, ensure_sheet_record, list_strings,
  map_binary, seed_cell_flow, set_cell_column, set_cell_parent, sheet_cell_order, sheet_order, sheet_record,
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
      board
        .format
        .sheet_type(*sheet_type_id)
        .context("unknown sheet type")?;
      ensure_sheet_record(doc, *sheet_id, name, *sheet_type_id)?;
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
      for cell in &sheet.cells {
        if cells.get(&cell.id.to_string()).is_some() {
          cells.delete(&cell.id.to_string())?;
        }
      }
      // I-S1: sweep the sheet's ink. Strokes referencing the dead sheet used
      // to linger in `flow.annotations` forever — unrendered, unreachable, and
      // only ever collected by an all-sheets clear.
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
      let entries = list_strings(&order);
      let from = entries
        .iter()
        .position(|entry| entry == &sheet_id.to_string())
        .context("unknown sheet")?;
      let mut target = match before {
        Some(anchor) => entries
          .iter()
          .position(|entry| entry == &anchor.to_string())
          .context("unknown anchor sheet")?,
        None => entries.len(),
      };
      if from < target {
        target -= 1;
      }
      if from != target {
        order.mov(from, target)?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::AddCell {
      sheet_id,
      cell_id,
      placement,
      seed,
    } => {
      // Duplicate-id guard: an id may exist ANYWHERE (another sheet included).
      if cell_record(doc, *cell_id).is_some() {
        bail!("cell {cell_id} already exists");
      }
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      let column_ids = board.sheet_column_ids(*sheet_id)?;
      let (column_ix, flat_ix, parent) = board_ops::resolve_cell_placement(sheet, &column_ids, *placement)?;
      ensure_cell_record(doc, *cell_id, *sheet_id, column_ids[column_ix], parent)?;
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_cell_order(&record)?;
      order.insert(flat_ix.min(order.len()), cell_id.to_string())?;
      match seed {
        CellSeed::Empty => seed_cell_flow(doc, *cell_id)?,
        CellSeed::Paragraphs(paragraphs) => write_cell_paragraphs(doc, *cell_id, paragraphs.clone())?,
      }
      Ok(MutationReport {
        content_cells: vec![*cell_id],
      })
    },
    FlowIntent::DeleteCell { sheet_id, cell_id } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if !sheet.cells.iter().any(|cell| cell.id == *cell_id) {
        bail!("unknown cell {cell_id}");
      }
      // Deleting a parent orphans its direct children (shipped semantics).
      for child in sheet
        .cells
        .iter()
        .filter(|cell| cell.parent_id == Some(*cell_id))
      {
        if let Some(record) = cell_record(doc, child.id) {
          set_cell_parent(&record, None)?;
        }
      }
      let record = sheet_record(doc, *sheet_id).context("unknown sheet record")?;
      let order = sheet_cell_order(&record)?;
      if let Some(index) = list_strings(&order)
        .iter()
        .position(|entry| entry == &cell_id.to_string())
      {
        order.delete(index, 1)?;
      }
      let cells = cells_map(doc);
      if cells.get(&cell_id.to_string()).is_some() {
        cells.delete(&cell_id.to_string())?;
      }
      Ok(MutationReport::default())
    },
    FlowIntent::MoveCellSubtree { sheet_id, cell_id, drop } => {
      execute_move_subtree(doc, board, *sheet_id, *cell_id, *drop)?;
      Ok(MutationReport::default())
    },
    FlowIntent::SetCellStruck { sheet_id, cell_id, struck } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if !sheet.cells.iter().any(|cell| cell.id == *cell_id) {
        bail!("unknown cell {cell_id}");
      }
      set_cell_struck(doc, *cell_id, *struck)?;
      Ok(MutationReport {
        content_cells: vec![*cell_id],
      })
    },
    FlowIntent::EnsureCellEditable { sheet_id, cell_id } => {
      let sheet = board.sheet(*sheet_id).context("unknown sheet")?;
      if !sheet.cells.iter().any(|cell| cell.id == *cell_id) {
        bail!("unknown cell {cell_id}");
      }
      let record = cell_record(doc, *cell_id).context("unknown cell record")?;
      let flow = cell_flow(&record).context("cell has no flow")?;
      let text = flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)?;
      let slot = flowstate_document::paragraph_slot(flowstate_document::PARAGRAPH_TAG).context("TAG style has no slot")?;
      if text.len_unicode() == 0 {
        seed_cell_flow(doc, *cell_id)?;
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
      if !sheet.cells.iter().any(|cell| cell.id == *cell_id) {
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
      let stroke: crate::projection::AnnotationStroke = postcard::from_bytes(&bytes)?;
      if stroke.sheet_id != *sheet_id || &stroke.originator != originator {
        bail!("annotation {stroke_id} does not match sheet/originator");
      }
      map.delete(&key)?;
      Ok(MutationReport::default())
    },
    FlowIntent::ClearAnnotations { scope, originator } => {
      clear_annotations(doc, board, scope, originator)?;
      Ok(MutationReport::default())
    },
    FlowIntent::CellText { .. } => {
      bail!("cell text intents execute only through the gated flow runtime");
    },
  }
}

fn clear_annotations(
  doc: &LoroDoc,
  board: &FlowBoardProjection,
  scope: &AnnotationScope,
  originator: &AnnotationOriginator,
) -> anyhow::Result<()> {
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
    let _ = board;
    if in_scope {
      map.delete(&key)?;
    }
  }
  Ok(())
}

/// The move law: validate + derive the final flat order via the SAME pure
/// [`board_ops::apply_move_subtree`] the previews use, then transform the
/// order list into it with `mov` ops for ONLY the subtree members, plus
/// column/parent register writes. Content containers are never touched.
fn execute_move_subtree(
  doc: &LoroDoc,
  board: &FlowBoardProjection,
  sheet_id: SheetId,
  cell_id: CellId,
  drop: FlowDropIntent,
) -> anyhow::Result<()> {
  let sheet = board.sheet(sheet_id).context("unknown sheet")?;
  let column_ids = board.sheet_column_ids(sheet_id)?;
  let mut preview: Sheet = sheet.clone();
  board_ops::apply_move_subtree(&mut preview, &column_ids, cell_id, drop)?;
  if !board_ops::sheet_topology_ok(&preview, &column_ids) {
    bail!("move would break the sheet topology");
  }
  let subtree: Vec<CellId> = board_ops::subtree_cell_ids(sheet, cell_id);

  // Register writes: every subtree member's column may shift; only the moved
  // root's parent changes.
  for moved in &preview.cells {
    if !subtree.contains(&moved.id) {
      continue;
    }
    let record = cell_record(doc, moved.id).context("unknown cell record")?;
    let before = sheet
      .cells
      .iter()
      .find(|cell| cell.id == moved.id)
      .context("cell vanished")?;
    if before.column_id != moved.column_id {
      set_cell_column(&record, moved.column_id)?;
    }
    if moved.id == cell_id && before.parent_id != moved.parent_id {
      set_cell_parent(&record, moved.parent_id)?;
    }
  }

  // Order-list transform via mov, subtree members only, ascending final
  // position (non-members keep their relative order, so this converges).
  let record = sheet_record(doc, sheet_id).context("unknown sheet record")?;
  let order = sheet_cell_order(&record)?;
  let desired: Vec<String> = preview
    .cells
    .iter()
    .map(|cell| cell.id.to_string())
    .collect();
  for (desired_ix, entry) in desired.iter().enumerate() {
    if !subtree.iter().any(|id| &id.to_string() == entry) {
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
/// solo write-back path until the per-cell authority lands).
pub fn replace_cell_document(doc: &LoroDoc, cell_id: CellId, document: &flowstate_document::DocumentProjection) -> anyhow::Result<()> {
  let record = cell_record(doc, cell_id).context("unknown cell record")?;
  let flow = cell_flow(&record).context("cell has no flow")?;
  let registry = cell_paragraph_registry(&flow).context("cell has no registry")?;
  flowstate_document::replace_single_flow_from_document(doc, &flow, &registry, &loro_schema::cell_flow_id(cell_id), document)?;
  Ok(())
}
