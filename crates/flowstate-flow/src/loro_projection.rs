//! The TOTAL board materializer (flow architecture spec Part 2.1): Loro doc →
//! [`FlowBoardProjection`], never failing on any canonical state. Merged LWW
//! states that violate flow invariants are repaired by a deterministic
//! normalization (a pure function of canonical state, so all peers converge
//! to the identical projection) and reported as [`FlowDefect`]s — the .db8
//! quarantine philosophy. The import path never writes repairs.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use loro::{LoroDoc, LoroMap};
use uuid::Uuid;

use crate::format::{CellId, ColumnId, SheetId};
use crate::loro_schema::{
  self, annotations_map, cell_flow, cell_paragraph_registry, cells_map, child_map, list_strings, map_binary, map_string, map_uuid, sheet_order,
  sheets_map,
};
use crate::projection::{AnnotationStroke, Cell, CellSummary, FlowBoardProjection, FlowDefect, Sheet};

pub struct MaterializedBoard {
  pub board: FlowBoardProjection,
  pub defects: Vec<FlowDefect>,
}

/// Materialize the whole board. Total: every failure mode is normalized in
/// projection space and reported, never returned as an error (only a missing
/// immutable format — an unopenable document — errors).
pub fn board_from_loro(doc: &LoroDoc) -> anyhow::Result<MaterializedBoard> {
  board_from_loro_cached(doc, &HashMap::new(), None)
}

/// Runtime variant: reuse CACHED cell summaries except for `dirty` cells
/// (None = everything is dirty). Summaries are pure functions of a cell's
/// flow content, so reuse is exact; this is what keeps a structural commit or
/// a single-cell keystroke O(changed), not O(board).
#[allow(
  clippy::implicit_hasher,
  reason = "the cache is the runtime's plain HashMap; generalizing the hasher buys nothing at this call count"
)]
pub fn board_from_loro_cached(
  doc: &LoroDoc,
  summary_cache: &HashMap<CellId, CellSummary>,
  dirty: Option<&HashSet<CellId>>,
) -> anyhow::Result<MaterializedBoard> {
  let format = loro_schema::read_format(doc)?;
  let mut defects = Vec::new();

  // ---- sheets: liveness = in order list ∧ in record map -------------------
  let sheet_records = sheets_map(doc);
  let mut live_sheet_ids: Vec<SheetId> = Vec::new();
  let mut seen: HashSet<SheetId> = HashSet::new();
  for entry in list_strings(&sheet_order(doc)) {
    let Ok(sheet_id) = Uuid::parse_str(&entry) else {
      defects.push(FlowDefect::OrderEntryMissingSheet { entry });
      continue;
    };
    if child_map(&sheet_records, &entry).is_none() {
      defects.push(FlowDefect::OrderEntryMissingSheet { entry });
      continue;
    }
    if seen.insert(sheet_id) {
      live_sheet_ids.push(sheet_id);
    }
  }
  let mut stragglers: Vec<SheetId> = loro_schema::map_keys(&sheet_records)
    .into_iter()
    .filter_map(|key| Uuid::parse_str(&key).ok())
    .filter(|id| !seen.contains(id))
    .collect();
  stragglers.sort();
  for sheet_id in stragglers {
    defects.push(FlowDefect::SheetMissingFromOrder { sheet: sheet_id });
    seen.insert(sheet_id);
    live_sheet_ids.push(sheet_id);
  }

  // ---- cells grouped by owning sheet (ONE pass over the record map) -------
  let cell_records = cells_map(doc);
  let mut cells_by_sheet: HashMap<SheetId, HashMap<CellId, LoroMap>> = HashMap::new();
  for key in loro_schema::map_keys(&cell_records) {
    let Some(record) = child_map(&cell_records, &key) else {
      continue;
    };
    let (Ok(cell_id), Some(sheet_id)) = (Uuid::parse_str(&key), map_uuid(&record, "sheet_id")) else {
      continue;
    };
    cells_by_sheet
      .entry(sheet_id)
      .or_default()
      .insert(cell_id, record);
  }

  let mut sheets = Vec::with_capacity(live_sheet_ids.len());
  for sheet_id in live_sheet_ids {
    let Some(record) = child_map(&sheet_records, &sheet_id.to_string()) else {
      continue;
    };
    let Some(sheet_type_id) = map_uuid(&record, "sheet_type_id") else {
      defects.push(FlowDefect::SheetTypeUnknown { sheet: sheet_id });
      continue;
    };
    let Some(definition) = format.sheet_type(sheet_type_id) else {
      defects.push(FlowDefect::SheetTypeUnknown { sheet: sheet_id });
      continue;
    };
    let column_ids: Vec<ColumnId> = definition.columns.iter().map(|column| column.id).collect();
    let name = map_string(&record, "name").unwrap_or_default();
    let mut sheet_cells = cells_by_sheet.remove(&sheet_id).unwrap_or_default();

    // Cell liveness against this sheet's order list.
    let mut ordered: Vec<(CellId, LoroMap)> = Vec::new();
    let mut placed: HashSet<CellId> = HashSet::new();
    if let Ok(order) = loro_schema::sheet_cell_order(&record) {
      for entry in list_strings(&order) {
        let Ok(cell_id) = Uuid::parse_str(&entry) else {
          defects.push(FlowDefect::OrderEntryMissingCell { sheet: sheet_id, entry });
          continue;
        };
        let Some(cell_record) = sheet_cells.remove(&cell_id) else {
          if !placed.contains(&cell_id) {
            defects.push(FlowDefect::OrderEntryMissingCell { sheet: sheet_id, entry });
          }
          continue;
        };
        if placed.insert(cell_id) {
          ordered.push((cell_id, cell_record));
        }
      }
    }
    let mut stragglers: Vec<CellId> = sheet_cells.keys().copied().collect();
    stragglers.sort();
    for cell_id in stragglers {
      let record = sheet_cells
        .remove(&cell_id)
        .expect("straggler key just enumerated");
      defects.push(FlowDefect::CellMissingFromOrder {
        sheet: sheet_id,
        cell: cell_id,
      });
      placed.insert(cell_id);
      ordered.push((cell_id, record));
    }

    // Raw cells (column clamp is part of normalization).
    let mut cells: Vec<Cell> = Vec::with_capacity(ordered.len());
    let mut records_by_id: HashMap<CellId, LoroMap> = HashMap::with_capacity(ordered.len());
    for (cell_id, cell_record) in ordered {
      let column_id = map_uuid(&cell_record, "column_id")
        .filter(|column| column_ids.contains(column))
        .unwrap_or_else(|| {
          defects.push(FlowDefect::UnknownColumnReassigned { cell: cell_id });
          column_ids[0]
        });
      let parent_id = map_uuid(&cell_record, "parent_id");
      cells.push(Cell {
        id: cell_id,
        column_id,
        parent_id,
        summary: CellSummary::default(),
      });
      records_by_id.insert(cell_id, cell_record);
    }

    normalize_sheet_cells(&mut cells, &column_ids, sheet_id, &mut defects);

    // Summaries: reuse the cache unless the cell is dirty (or no cache).
    for cell in &mut cells {
      if let Some(dirty) = dirty
        && !dirty.contains(&cell.id)
        && let Some(cached) = summary_cache.get(&cell.id)
      {
        cell.summary = cached.clone();
        continue;
      }
      let Some(record) = records_by_id.get(&cell.id) else {
        continue;
      };
      match cell_document_from_record(doc, record) {
        Ok(document) => cell.summary = derive_cell_summary(&document),
        Err(error) => {
          defects.push(FlowDefect::CellFlowInvalid {
            cell: cell.id,
            error: error.to_string(),
          });
          cell.summary = CellSummary::default();
        },
      }
    }

    let annotations = sheet_annotations(doc, sheet_id);
    sheets.push(Sheet {
      id: sheet_id,
      name,
      sheet_type_id,
      cells,
      annotations,
    });
  }

  Ok(MaterializedBoard {
    board: FlowBoardProjection { format, sheets },
    defects,
  })
}

/// Normalization law steps 2–5 (spec Part 2.1), pure over the cell list:
/// cycle break → dangling parent → column adjacency → sibling-run contiguity.
/// Deterministic on canonical state: ties break by uuid, scans run in flat
/// order, and the regroup loop is bounded with an orphan fallback.
pub fn normalize_sheet_cells(cells: &mut Vec<Cell>, column_ids: &[ColumnId], _sheet: SheetId, defects: &mut Vec<FlowDefect>) {
  let level = |column: ColumnId| column_ids.iter().position(|candidate| *candidate == column);

  // (2) parent cycles: walk chains; break at the cycle member with the
  // greatest uuid. Iterate seeds in uuid order for determinism.
  let mut seeds: Vec<CellId> = cells.iter().map(|cell| cell.id).collect();
  seeds.sort();
  for seed in seeds {
    let parents: HashMap<CellId, Option<CellId>> = cells.iter().map(|cell| (cell.id, cell.parent_id)).collect();
    let mut chain: Vec<CellId> = vec![seed];
    let mut visited: HashSet<CellId> = [seed].into();
    let mut current = seed;
    while let Some(Some(parent)) = parents.get(&current).copied() {
      if visited.contains(&parent) {
        // Cycle: members are the chain suffix from `parent` on.
        let start = chain.iter().position(|id| *id == parent).unwrap_or(0);
        let breaker = chain[start..].iter().copied().max().unwrap_or(parent);
        if let Some(cell) = cells.iter_mut().find(|cell| cell.id == breaker) {
          cell.parent_id = None;
          defects.push(FlowDefect::ParentCycleBroken { cell: breaker });
        }
        break;
      }
      visited.insert(parent);
      chain.push(parent);
      current = parent;
    }
  }

  // (3) dangling parents.
  let live: HashSet<CellId> = cells.iter().map(|cell| cell.id).collect();
  for cell in cells.iter_mut() {
    if let Some(parent) = cell.parent_id
      && !live.contains(&parent)
    {
      cell.parent_id = None;
      defects.push(FlowDefect::DanglingParentOrphaned { cell: cell.id, parent });
    }
  }

  // (4) column adjacency: detach (keep the cell's own column — detaching
  // loses less intent than re-columning a whole subtree).
  let columns_by_id: HashMap<CellId, ColumnId> = cells.iter().map(|cell| (cell.id, cell.column_id)).collect();
  for cell in cells.iter_mut() {
    let Some(parent) = cell.parent_id else { continue };
    let (Some(cell_level), Some(parent_level)) = (level(cell.column_id), columns_by_id.get(&parent).and_then(|column| level(*column))) else {
      continue;
    };
    if cell_level != parent_level + 1 {
      cell.parent_id = None;
      defects.push(FlowDefect::ColumnAdjacencyOrphaned { cell: cell.id });
    }
  }

  // (5) sibling-run contiguity: relocate a split-run member (and its subtree)
  // to immediately after the last member of the run's first occurrence.
  // Bounded; leftover violators orphan (defect) rather than loop.
  // Bounded: each relocation merges the violator into the run's first
  // segment; adversarial cascades are cut off by the budget, leaving a (still
  // deterministic) split run reported as a defect rather than looping.
  let mut budget = cells.len().saturating_mul(2) + 8;
  while let Some((violator, run_last)) = first_run_violation(cells, column_ids) {
    defects.push(FlowDefect::RunSplitRegrouped { cell: violator });
    if budget == 0 {
      break;
    }
    budget -= 1;
    relocate_subtree_after(cells, violator, run_last);
  }
}

/// First cell (flat order) whose parent-run in its column was already closed,
/// plus the flat index of the last member of that run's first segment.
fn first_run_violation(cells: &[Cell], column_ids: &[ColumnId]) -> Option<(CellId, usize)> {
  for &column in column_ids {
    let mut completed: HashMap<Option<CellId>, usize> = HashMap::new(); // run key → last flat ix of first segment
    let mut current: Option<Option<CellId>> = None;
    let mut current_last = 0_usize;
    for (flat_ix, cell) in cells.iter().enumerate() {
      if cell.column_id != column {
        continue;
      }
      let key = cell.parent_id;
      if current == Some(key) {
        current_last = flat_ix;
        continue;
      }
      if let Some(previous) = current {
        completed.entry(previous).or_insert(current_last);
      }
      if let Some(&run_last) = completed.get(&key) {
        // Root runs (None) may legitimately be interleaved across families?
        // No — the shipped validator treats ANY re-opened run as a violation,
        // including root runs, so mirror it exactly.
        return Some((cell.id, run_last));
      }
      current = Some(key);
      current_last = flat_ix;
    }
  }
  None
}

/// Move `cell_id`'s subtree block to immediately after flat index `after_ix`
/// (positions relative to the list WITHOUT the subtree).
fn relocate_subtree_after(cells: &mut Vec<Cell>, cell_id: CellId, after_ix: usize) {
  let subtree = subtree_set(cells, cell_id);
  let removed_before = cells
    .iter()
    .take(after_ix + 1)
    .filter(|cell| subtree.contains(&cell.id))
    .count();
  let mut moved = Vec::new();
  let mut remaining = Vec::with_capacity(cells.len());
  for cell in cells.drain(..) {
    if subtree.contains(&cell.id) {
      moved.push(cell);
    } else {
      remaining.push(cell);
    }
  }
  let insertion = (after_ix + 1 - removed_before).min(remaining.len());
  remaining.splice(insertion..insertion, moved);
  *cells = remaining;
}

fn subtree_set(cells: &[Cell], root: CellId) -> HashSet<CellId> {
  let mut children: HashMap<CellId, Vec<CellId>> = HashMap::new();
  for cell in cells {
    if let Some(parent) = cell.parent_id {
      children.entry(parent).or_default().push(cell.id);
    }
  }
  let mut out: HashSet<CellId> = HashSet::new();
  let mut stack = vec![root];
  while let Some(id) = stack.pop() {
    if out.insert(id)
      && let Some(kids) = children.get(&id)
    {
      stack.extend(kids.iter().copied());
    }
  }
  out
}

fn sheet_annotations(doc: &LoroDoc, sheet_id: SheetId) -> Vec<AnnotationStroke> {
  let map = annotations_map(doc);
  let mut strokes: Vec<AnnotationStroke> = Vec::new();
  for key in loro_schema::map_keys(&map) {
    let Some(bytes) = map_binary(&map, &key) else {
      continue;
    };
    let Ok(stroke) = postcard::from_bytes::<AnnotationStroke>(&bytes) else {
      // I-S2 hardening: silent refusal is a defect — an invisible stroke too.
      tracing::warn!("skipping undecodable annotation blob in projection");
      continue;
    };
    if key != stroke.id.to_string() || stroke.sheet_id != sheet_id {
      continue;
    }
    strokes.push(stroke);
  }
  strokes.sort_by_key(|stroke| stroke.id);
  strokes
}

/// Materialize ONE cell's rich text into a full editor projection, with
/// durable ids from the cell's scoped registry (never fabricated fresh — the
/// intent-resolution law anchors on them).
pub fn cell_document(doc: &LoroDoc, cell_id: CellId) -> anyhow::Result<flowstate_document::DocumentProjection> {
  let record = loro_schema::cell_record(doc, cell_id).ok_or_else(|| anyhow::anyhow!("unknown cell {cell_id}"))?;
  cell_document_from_record(doc, &record)
}

fn cell_document_from_record(doc: &LoroDoc, record: &LoroMap) -> anyhow::Result<flowstate_document::DocumentProjection> {
  let flow = cell_flow(record).ok_or_else(|| anyhow::anyhow!("cell record has no flow"))?;
  let registry = cell_paragraph_registry(&flow);
  let rows = flowstate_document::materialize_single_flow(doc, &flow, registry.as_ref())?;
  let mut document = flowstate_document::document_from_input_blocks(
    flowstate_document::DocumentTheme::clone(&flowstate_document::flowstate_document_theme()),
    rows.blocks,
  );
  if rows.paragraph_ids.len() == document.paragraphs.len() {
    document.ids.paragraph_ids = Arc::new(rows.paragraph_ids);
  }
  if rows.block_ids.len() == document.blocks.len() {
    document.ids.block_ids = Arc::new(rows.block_ids);
  }
  document.frontier = doc.state_frontiers().encode();
  Ok(document)
}

/// The board-level read model of a cell's rich text (the old `Cell::summary_*`
/// law, ported onto a materialized projection).
pub fn derive_cell_summary(document: &flowstate_document::DocumentProjection) -> CellSummary {
  let mut projection: Vec<String> = Vec::new();
  let mut uses_summary = false;
  let mut any_text = false;
  let mut all_struck = true;
  for (index, paragraph) in document.paragraphs.iter().enumerate() {
    let text = paragraph_text(document, index);
    if matches!(
      paragraph.style,
      flowstate_document::PARAGRAPH_TAG | flowstate_document::PARAGRAPH_UNDERTAG | flowstate_document::PARAGRAPH_ANALYTIC
    ) {
      uses_summary = true;
      projection.push(text.clone());
    } else {
      let mut cite_text = String::new();
      let mut offset = 0;
      for run in &paragraph.runs {
        let end = offset + run.len;
        if run.styles.semantic == flowstate_document::SEMANTIC_CITE {
          uses_summary = true;
          cite_text.push_str(&text[offset..end]);
        }
        offset = end;
      }
      if !cite_text.is_empty() {
        projection.push(cite_text);
      }
    }
    let mut offset = 0;
    for run in &paragraph.runs {
      let end = offset + run.len;
      let run_text = &text[offset..end];
      if !run_text.trim().is_empty() {
        any_text = true;
        if !run.styles.strikethrough {
          all_struck = false;
        }
      }
      offset = end;
    }
  }
  let full_text = document.text.to_string();
  let summary_text = if projection.is_empty() {
    full_text.clone()
  } else {
    projection.join("\n")
  };
  CellSummary {
    summary_text: Arc::from(summary_text.as_str()),
    uses_summary_projection: uses_summary,
    struck: any_text && all_struck,
    is_empty: full_text.trim().is_empty() && document.blocks.len() == document.paragraphs.len(),
  }
}

fn paragraph_text(document: &flowstate_document::DocumentProjection, index: usize) -> String {
  document
    .text
    .byte_slice(flowstate_document::paragraph_byte_range(document, index))
    .to_string()
}
