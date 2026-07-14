//! The TOTAL board materializer + normalization law (.fl0 v2 spec, Part A).
//!
//! Merged LWW states can violate flow invariants (concurrent moves, deletes
//! racing adds, parent rewrites crossing sheet deletes). The materializer
//! never fails on such states — it applies deterministic normalization (a pure
//! function of canonical state, so every peer converges on the identical
//! board) and reports [`FlowDefect`]s in the .db8 quarantine philosophy. The
//! import path never writes repairs; a capped `"repair"`-origin local
//! canonicalization pass (the runtime) may later rewrite canonical state to
//! match, converging with what every peer already renders.
//!
//! Normalization order:
//! 1. liveness (in map ∧ in order; repair by deterministic append / skip),
//! 2. unknown column → clamp to the first column,
//! 3. dangling parent → orphan,
//! 4. parent-cycle break (null the greatest-uuid member),
//! 5. column-adjacency violation → orphan,
//! 6. sibling-run contiguity → canonical DFS re-linearization.

use std::sync::Arc;

use flowstate_document::{PARAGRAPH_ANALYTIC, PARAGRAPH_TAG, PARAGRAPH_UNDERTAG, RegionRows, SEMANTIC_CITE, materialize_single_flow};
use gpui_flowtext::{DocumentProjection, DocumentTheme, InputBlock, document_from_input_blocks};
use loro::{LoroDoc, LoroMap};
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

use crate::format::{CellId, SheetId};
use crate::loro_schema::{
  cell_flow_label, cell_flow_map, cell_order_ids, cells_by_id, child_map, map_string, map_uuid, read_annotations, read_format, sheet_order_ids,
  sheets_by_id,
};
use crate::projection::{Cell, CellSummary, FlowBoardProjection, Sheet};

/// One normalized-away violation of the flow invariants. Every defect is a
/// deterministic function of canonical state; the repair pass keys attempt
/// caps off [`FlowDefect::stable_key`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowDefect {
  /// Sheet-order entry without a live sheet record → skipped.
  SheetOrderMissingRecord { key: String },
  /// Sheet record missing from the board order → appended (uuid-sorted).
  SheetRecordMissingOrder { sheet: SheetId },
  /// Sheet record whose sheet type is not in the immutable format → its cells
  /// are not materialized (nothing can place them in columns).
  UnknownSheetType { sheet: SheetId },
  /// Cell-order entry without a live cell record in that sheet → skipped.
  CellOrderMissingRecord { sheet: SheetId, key: String },
  /// Cell record missing from its sheet's order → appended (uuid-sorted).
  CellRecordMissingOrder { cell: CellId },
  /// Cell record referencing a dead/unknown sheet → not materialized (the
  /// sheet delete wins).
  CellRecordUnknownSheet { cell: CellId },
  /// Cell record with a missing/unknown column id → clamped to column 0.
  UnknownColumn { cell: CellId },
  /// Parent id not live in the same sheet → orphaned.
  DanglingParent { cell: CellId },
  /// Parent cycle → the greatest-uuid member orphaned.
  ParentCycle { cell: CellId },
  /// Child column is not exactly one right of its parent's → orphaned.
  ColumnAdjacency { cell: CellId },
  /// Raw cell order was not the canonical DFS linearization → re-linearized.
  SiblingRunSplit { sheet: SheetId },
  /// Cell record without a seeded flow container → empty summary.
  CellMissingFlow { cell: CellId },
  /// A defect reported by the shared rich-text materializer for one cell.
  CellFlow {
    cell: CellId,
    defect: flowstate_document::ProjectionDefect,
  },
}

impl FlowDefect {
  #[must_use]
  pub fn stable_key(&self) -> String {
    match self {
      Self::SheetOrderMissingRecord { key } => format!("sheet-order-missing-record:{key}"),
      Self::SheetRecordMissingOrder { sheet } => format!("sheet-record-missing-order:{sheet}"),
      Self::UnknownSheetType { sheet } => format!("unknown-sheet-type:{sheet}"),
      Self::CellOrderMissingRecord { sheet, key } => format!("cell-order-missing-record:{sheet}:{key}"),
      Self::CellRecordMissingOrder { cell } => format!("cell-record-missing-order:{cell}"),
      Self::CellRecordUnknownSheet { cell } => format!("cell-record-unknown-sheet:{cell}"),
      Self::UnknownColumn { cell } => format!("unknown-column:{cell}"),
      Self::DanglingParent { cell } => format!("dangling-parent:{cell}"),
      Self::ParentCycle { cell } => format!("parent-cycle:{cell}"),
      Self::ColumnAdjacency { cell } => format!("column-adjacency:{cell}"),
      Self::SiblingRunSplit { sheet } => format!("sibling-run-split:{sheet}"),
      Self::CellMissingFlow { cell } => format!("cell-missing-flow:{cell}"),
      Self::CellFlow { cell, defect } => format!("cell-flow:{cell}:{}", defect.stable_key()),
    }
  }
}

/// Materialize the whole board. Deterministic: byte-equal boards on every peer
/// sharing the same canonical state, regardless of local op history.
#[hotpath::measure]
pub fn materialize_board(doc: &LoroDoc) -> anyhow::Result<(FlowBoardProjection, Vec<FlowDefect>)> {
  let format = read_format(doc)?;
  let mut defects = Vec::new();

  // ---- Sheet linearization (normalization step 1, sheet scope) -------------
  let sheet_registry = sheets_by_id(doc);
  let mut live_sheet_keys: Vec<String> = {
    let mut keys: Vec<String> = sheet_registry.keys().map(|key| key.to_string()).collect();
    keys.sort();
    keys
  };
  let ordered = sheet_order_ids(doc);
  let live: FxHashSet<&String> = live_sheet_keys.iter().collect();
  let mut sheet_keys: Vec<String> = Vec::with_capacity(live_sheet_keys.len());
  let mut seen: FxHashSet<String> = FxHashSet::default();
  for key in &ordered {
    if !live.contains(key) {
      defects.push(FlowDefect::SheetOrderMissingRecord { key: key.clone() });
      continue;
    }
    if seen.insert(key.clone()) {
      sheet_keys.push(key.clone());
    }
  }
  live_sheet_keys.retain(|key| !seen.contains(key));
  for key in live_sheet_keys {
    // Record without an order entry: deterministic uuid-sorted append.
    if let Ok(sheet) = Uuid::parse_str(&key) {
      defects.push(FlowDefect::SheetRecordMissingOrder { sheet });
    }
    sheet_keys.push(key);
  }

  // ---- Cells grouped by sheet ----------------------------------------------
  let cell_registry = cells_by_id(doc);
  let mut cell_keys: Vec<String> = cell_registry.keys().map(|key| key.to_string()).collect();
  cell_keys.sort();
  struct RawCell {
    id: CellId,
    sheet_id: SheetId,
    column_id: Option<Uuid>,
    parent_id: Option<CellId>,
    map: LoroMap,
  }
  let mut cells_by_sheet: FxHashMap<SheetId, Vec<RawCell>> = FxHashMap::default();
  let live_sheet_ids: FxHashSet<SheetId> = sheet_keys
    .iter()
    .filter_map(|key| Uuid::parse_str(key).ok())
    .collect();
  for key in cell_keys {
    let Some(map) = child_map(&cell_registry, &key) else {
      continue;
    };
    let Some(id) = map_uuid(&map, "id").filter(|id| id.to_string() == key) else {
      continue;
    };
    let Some(sheet_id) = map_uuid(&map, "sheet_id") else {
      defects.push(FlowDefect::CellRecordUnknownSheet { cell: id });
      continue;
    };
    if !live_sheet_ids.contains(&sheet_id) {
      defects.push(FlowDefect::CellRecordUnknownSheet { cell: id });
      continue;
    }
    cells_by_sheet.entry(sheet_id).or_default().push(RawCell {
      id,
      sheet_id,
      column_id: map_uuid(&map, "column_id"),
      parent_id: map_uuid(&map, "parent_id"),
      map,
    });
  }

  // ---- Per-sheet normalization ----------------------------------------------
  let mut sheets = Vec::with_capacity(sheet_keys.len());
  for key in &sheet_keys {
    let Some(sheet_map) = child_map(&sheet_registry, key) else {
      continue;
    };
    let Ok(sheet_id) = Uuid::parse_str(key) else {
      continue;
    };
    let name = map_string(&sheet_map, "name").unwrap_or_default();
    let Some(sheet_type_id) = map_uuid(&sheet_map, "sheet_type_id") else {
      defects.push(FlowDefect::UnknownSheetType { sheet: sheet_id });
      sheets.push(Sheet {
        id: sheet_id,
        name,
        sheet_type_id: Uuid::nil(),
        cells: Vec::new(),
        annotations: Vec::new(),
      });
      continue;
    };
    let Some(definition) = format.sheet_type(sheet_type_id) else {
      defects.push(FlowDefect::UnknownSheetType { sheet: sheet_id });
      sheets.push(Sheet {
        id: sheet_id,
        name,
        sheet_type_id,
        cells: Vec::new(),
        annotations: Vec::new(),
      });
      continue;
    };
    let column_ids: Vec<Uuid> = definition.columns.iter().map(|column| column.id).collect();

    let mut raw_cells = cells_by_sheet.remove(&sheet_id).unwrap_or_default();
    debug_assert!(raw_cells.iter().all(|cell| cell.sheet_id == sheet_id));

    // Step 1 (cell liveness): order ∧ map, deterministic appends for
    // record-without-order, skips (+defect) for order-without-record.
    let by_id: FxHashMap<CellId, usize> = raw_cells
      .iter()
      .enumerate()
      .map(|(ix, cell)| (cell.id, ix))
      .collect();
    let mut linear: Vec<CellId> = Vec::with_capacity(raw_cells.len());
    let mut seen: FxHashSet<CellId> = FxHashSet::default();
    for entry in cell_order_ids(&sheet_map) {
      let Some(id) = Uuid::parse_str(&entry)
        .ok()
        .filter(|id| by_id.contains_key(id))
      else {
        defects.push(FlowDefect::CellOrderMissingRecord { sheet: sheet_id, key: entry });
        continue;
      };
      if seen.insert(id) {
        linear.push(id);
      }
    }
    // raw_cells is uuid-key-sorted already, so the append order is
    // deterministic.
    for cell in &raw_cells {
      if seen.insert(cell.id) {
        defects.push(FlowDefect::CellRecordMissingOrder { cell: cell.id });
        linear.push(cell.id);
      }
    }

    // Step 2 (unknown column → clamp) — requires at least one column; a
    // format with an empty column list cannot place any cell.
    let level_of = |column: Option<Uuid>| column.and_then(|column| column_ids.iter().position(|candidate| *candidate == column));
    for cell in &mut raw_cells {
      if level_of(cell.column_id).is_none() {
        defects.push(FlowDefect::UnknownColumn { cell: cell.id });
        cell.column_id = column_ids.first().copied();
      }
    }

    // Step 3 (dangling parent → orphan).
    for cell in &mut raw_cells {
      if let Some(parent) = cell.parent_id
        && !by_id.contains_key(&parent)
      {
        defects.push(FlowDefect::DanglingParent { cell: cell.id });
        cell.parent_id = None;
      }
    }

    // Step 4 (cycle break: null the greatest-uuid member of each cycle).
    loop {
      let parent_of: FxHashMap<CellId, CellId> = raw_cells
        .iter()
        .filter_map(|cell| cell.parent_id.map(|parent| (cell.id, parent)))
        .collect();
      let mut broken: Option<CellId> = None;
      let mut resolved: FxHashSet<CellId> = FxHashSet::default();
      for cell in &raw_cells {
        if resolved.contains(&cell.id) {
          continue;
        }
        let mut chain: Vec<CellId> = Vec::new();
        let mut on_chain: FxHashSet<CellId> = FxHashSet::default();
        let mut cursor = cell.id;
        loop {
          if resolved.contains(&cursor) {
            break;
          }
          if !on_chain.insert(cursor) {
            // Cycle: members are the chain suffix from the first occurrence.
            let start = chain.iter().position(|id| *id == cursor).unwrap_or(0);
            let member = chain[start..].iter().copied().max().unwrap_or(cursor);
            broken = Some(member);
            break;
          }
          chain.push(cursor);
          match parent_of.get(&cursor) {
            Some(parent) => cursor = *parent,
            None => break,
          }
        }
        if broken.is_some() {
          break;
        }
        resolved.extend(chain);
      }
      let Some(member) = broken else {
        break;
      };
      defects.push(FlowDefect::ParentCycle { cell: member });
      if let Some(cell) = raw_cells.iter_mut().find(|cell| cell.id == member) {
        cell.parent_id = None;
      }
    }

    // Step 5 (column adjacency → orphan). Depends only on the cell's own
    // column vs its parent's, so a single pass suffices.
    let levels: FxHashMap<CellId, usize> = raw_cells
      .iter()
      .filter_map(|cell| level_of(cell.column_id).map(|level| (cell.id, level)))
      .collect();
    for cell in &mut raw_cells {
      let Some(parent) = cell.parent_id else {
        continue;
      };
      let child_level = levels.get(&cell.id).copied();
      let parent_level = levels.get(&parent).copied();
      let adjacent = matches!((child_level, parent_level), (Some(child), Some(parent)) if child == parent + 1);
      if !adjacent {
        defects.push(FlowDefect::ColumnAdjacency { cell: cell.id });
        cell.parent_id = None;
      }
    }

    // Step 6 (canonical DFS re-linearization): roots in linear order, each
    // followed by its whole subtree, children in linear order. A well-formed
    // order maps to itself; anything else regroups deterministically.
    let normalized: FxHashMap<CellId, &RawCell> = raw_cells.iter().map(|cell| (cell.id, cell)).collect();
    let mut children: FxHashMap<CellId, Vec<CellId>> = FxHashMap::default();
    let mut roots: Vec<CellId> = Vec::new();
    for id in &linear {
      match normalized.get(id).and_then(|cell| cell.parent_id) {
        Some(parent) => children.entry(parent).or_default().push(*id),
        None => roots.push(*id),
      }
    }
    let mut canonical: Vec<CellId> = Vec::with_capacity(linear.len());
    let mut stack: Vec<CellId> = roots.iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
      canonical.push(id);
      if let Some(kids) = children.get(&id) {
        stack.extend(kids.iter().rev());
      }
    }
    debug_assert_eq!(canonical.len(), linear.len(), "DFS must visit every live cell exactly once");
    if canonical != linear {
      defects.push(FlowDefect::SiblingRunSplit { sheet: sheet_id });
    }

    // ---- Summaries -----------------------------------------------------------
    let mut cells = Vec::with_capacity(canonical.len());
    for id in canonical {
      let raw = normalized[&id];
      let summary = match cell_flow_map(&raw.map) {
        Some(flow) => {
          let label = cell_flow_label(id);
          match materialize_single_flow(doc, &flow, &label) {
            Ok(rows) => {
              for defect in rows.defects.iter().cloned() {
                defects.push(FlowDefect::CellFlow { cell: id, defect });
              }
              summary_from_rows(&rows.blocks)
            },
            Err(_) => {
              defects.push(FlowDefect::CellMissingFlow { cell: id });
              CellSummary::default()
            },
          }
        },
        None => {
          defects.push(FlowDefect::CellMissingFlow { cell: id });
          CellSummary::default()
        },
      };
      cells.push(Cell {
        id,
        column_id: raw.column_id.unwrap_or_else(Uuid::nil),
        parent_id: raw.parent_id,
        summary,
      });
    }

    sheets.push(Sheet {
      id: sheet_id,
      name,
      sheet_type_id,
      cells,
      annotations: Vec::new(),
    });
  }

  // ---- Annotations (unchanged convergent blobs, routed by sheet) -----------
  for stroke in read_annotations(doc) {
    if let Some(sheet) = sheets.iter_mut().find(|sheet| sheet.id == stroke.sheet_id) {
      sheet.annotations.push(stroke);
    }
  }

  Ok((FlowBoardProjection { format, sheets }, defects))
}

/// Materialize ONE cell's full rich-text projection (editor attach). The board
/// carries only summaries; this is the on-demand deep materialization.
#[hotpath::measure]
pub fn materialize_cell_projection(doc: &LoroDoc, cell_id: CellId, theme: DocumentTheme) -> anyhow::Result<DocumentProjection> {
  let rows = materialize_cell_rows(doc, cell_id)?;
  Ok(document_from_rows(doc, rows, theme))
}

/// The raw materialized rows for one cell (shared by the projection above and
/// the runtime's summary refresh).
#[hotpath::measure]
pub fn materialize_cell_rows(doc: &LoroDoc, cell_id: CellId) -> anyhow::Result<RegionRows> {
  let cell = crate::loro_schema::cell_map(doc, cell_id).ok_or_else(|| anyhow::anyhow!("unknown cell {cell_id}"))?;
  let flow = cell_flow_map(&cell).ok_or_else(|| anyhow::anyhow!("cell {cell_id} has no flow"))?;
  Ok(materialize_single_flow(doc, &flow, &cell_flow_label(cell_id))?)
}

/// Assemble a full per-cell `DocumentProjection` from materialized rows:
/// durable paragraph/block ids installed, frontier stamped.
#[must_use]
pub fn document_from_rows(doc: &LoroDoc, rows: RegionRows, theme: DocumentTheme) -> DocumentProjection {
  let RegionRows {
    blocks,
    paragraph_ids,
    block_ids,
    defects: _,
  } = rows;
  let mut document = document_from_input_blocks(theme, blocks);
  if paragraph_ids.len() == document.paragraphs.len() {
    document.ids.paragraph_ids = Arc::new(paragraph_ids);
  }
  if block_ids.len() == document.blocks.len() {
    document.ids.block_ids = Arc::new(block_ids);
  }
  document.frontier = doc.state_frontiers().encode();
  document
}

/// The board-summary law (former `Cell::summary_text` /
/// `uses_summary_projection` / strike detection), over materialized rows.
#[must_use]
pub fn summary_from_rows(blocks: &[InputBlock]) -> CellSummary {
  let paragraphs: Vec<&gpui_flowtext::InputParagraph> = blocks
    .iter()
    .filter_map(|block| match block {
      InputBlock::Paragraph(paragraph) => Some(paragraph),
      _ => None,
    })
    .collect();

  let mut full_text = String::new();
  let mut summary_rows: Vec<String> = Vec::new();
  let mut uses_summary_projection = false;
  let mut any_visible_run = false;
  let mut all_struck = true;

  for (ix, paragraph) in paragraphs.iter().enumerate() {
    let text: String = paragraph.runs.iter().map(|run| run.text.as_str()).collect();
    if ix > 0 {
      full_text.push('\n');
    }
    full_text.push_str(&text);

    for run in &paragraph.runs {
      if !run.text.is_empty() {
        any_visible_run = true;
        if !run.styles.strikethrough {
          all_struck = false;
        }
      }
    }

    if matches!(paragraph.style, PARAGRAPH_TAG | PARAGRAPH_UNDERTAG | PARAGRAPH_ANALYTIC) {
      uses_summary_projection = true;
      summary_rows.push(text);
      continue;
    }
    // An empty cite run still flips the summary-projection flag (the former
    // `uses_summary_projection` law) — only non-empty cite text becomes a row.
    if paragraph
      .runs
      .iter()
      .any(|run| run.styles.semantic == SEMANTIC_CITE)
    {
      uses_summary_projection = true;
    }
    let cite_text: String = paragraph
      .runs
      .iter()
      .filter(|run| run.styles.semantic == SEMANTIC_CITE)
      .map(|run| run.text.as_str())
      .collect();
    if !cite_text.is_empty() {
      summary_rows.push(cite_text);
    }
  }

  let summary_text: Arc<str> = if summary_rows.is_empty() {
    Arc::from(full_text.as_str())
  } else {
    Arc::from(summary_rows.join("\n").as_str())
  };
  CellSummary {
    summary_text,
    uses_summary_projection,
    struck: any_visible_run && all_struck,
    is_empty: full_text.trim().is_empty(),
  }
}
