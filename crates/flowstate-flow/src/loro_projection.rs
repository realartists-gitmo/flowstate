//! The TOTAL grid materializer (excel flow spec §3): Loro doc →
//! [`FlowBoardProjection`], never failing on any canonical state. Merged LWW
//! states that violate grid invariants are repaired by a deterministic
//! normalization (a pure function of canonical state, so all peers converge
//! to the identical projection) and reported as [`FlowDefect`]s — the .db8
//! quarantine philosophy. The import path never writes repairs.
//!
//! Normalization order: sheet liveness → column liveness → row phantoms →
//! cell address → slot collision bump-down (D2). No cycles are possible in a
//! model with no parents.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use loro::{LoroDoc, LoroMap};
use uuid::Uuid;

use crate::format::{CellId, ColumnId, RowId, SheetId};
use crate::loro_schema::{
  self, annotations_map, cell_flow, cell_paragraph_registry, cells_map, child_map, list_strings, map_binary, map_f64, map_string, map_uuid,
  sheet_order, sheets_map,
};
use crate::projection::{AnnotationStroke, Cell, CellSummary, FlowBoardProjection, FlowDefect, GridColumn, GridRow, Sheet};

pub struct MaterializedBoard {
  pub board: FlowBoardProjection,
  pub defects: Vec<FlowDefect>,
}

/// The ONE bump-row law (D2). A loser of a slot collision lands in the row
/// with this id — a pure function of the losing cell and the bump round, so
/// every peer synthesizes the identical row. The runtime's repair pass and
/// the materializer MUST share this function (the `loro_id` law-mismatch
/// lesson: two derivations of "the same" id are a permanent-defect factory).
///
/// `round` 0 is reserved for cells with no usable row address; collisions
/// start at round 1 and re-salt per cascade round so bumped cells that
/// collide again always reach fresh rows (termination).
pub fn bump_row_id(loser: CellId, round: u32) -> RowId {
  const BUMP_NAMESPACE: Uuid = Uuid::from_u128(0x8f0c_b1de_44a3_4c6e_9a7d_31d2_5e8f_66aa);
  Uuid::new_v5(&BUMP_NAMESPACE, format!("{loser}:{round}").as_bytes())
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
    let name = map_string(&record, "name").unwrap_or_default();
    let sheet_type_id = map_uuid(&record, "sheet_type_id");
    let definition = sheet_type_id.and_then(|id| format.sheet_type(id));
    if sheet_type_id.is_none() || definition.is_none() {
      defects.push(FlowDefect::SheetTypeUnknown { sheet: sheet_id });
    }

    // ---- columns: order ∩ records; stragglers appended; type fallback -----
    let mut columns: Vec<GridColumn> = Vec::new();
    let mut column_seen: HashSet<ColumnId> = HashSet::new();
    let columns_map = loro_schema::child_map(&record, loro_schema::COLUMNS_BY_ID_KEY);
    if let (Ok(order), Some(columns_map)) = (loro_schema::sheet_column_order(&record), columns_map.as_ref()) {
      for entry in list_strings(&order) {
        let Ok(column_id) = Uuid::parse_str(&entry) else {
          defects.push(FlowDefect::OrderEntryMissingColumn { sheet: sheet_id, entry });
          continue;
        };
        let Some(column_record) = child_map(columns_map, &entry) else {
          defects.push(FlowDefect::OrderEntryMissingColumn { sheet: sheet_id, entry });
          continue;
        };
        if column_seen.insert(column_id) {
          columns.push(grid_column(column_id, &column_record));
        }
      }
      let mut column_stragglers: Vec<ColumnId> = loro_schema::map_keys(columns_map)
        .into_iter()
        .filter_map(|key| Uuid::parse_str(&key).ok())
        .filter(|id| !column_seen.contains(id))
        .collect();
      column_stragglers.sort();
      for column_id in column_stragglers {
        let Some(column_record) = child_map(columns_map, &column_id.to_string()) else {
          continue;
        };
        defects.push(FlowDefect::ColumnMissingFromOrder {
          sheet: sheet_id,
          column: column_id,
        });
        column_seen.insert(column_id);
        columns.push(grid_column(column_id, &column_record));
      }
    }
    if columns.is_empty() {
      // No live column records: fall back to the sheet type template (a
      // projection-space seed — the repair pass writes it for real).
      let Some(definition) = definition else {
        // No columns AND no template: the sheet cannot render; skip it.
        continue;
      };
      defects.push(FlowDefect::ColumnsSeededFromType { sheet: sheet_id });
      for column in &definition.columns {
        columns.push(GridColumn {
          id: column.id,
          label: column.label.clone(),
          side: column.side,
          width: None,
        });
      }
    }

    // ---- rows: order entries, deduped -------------------------------------
    let mut rows: Vec<RowId> = Vec::new();
    let mut row_seen: HashSet<RowId> = HashSet::new();
    if let Ok(order) = loro_schema::sheet_row_order(&record) {
      for entry in list_strings(&order) {
        let Ok(row_id) = Uuid::parse_str(&entry) else {
          defects.push(FlowDefect::OrderEntryInvalidRow { sheet: sheet_id, entry });
          continue;
        };
        if row_seen.insert(row_id) {
          rows.push(row_id);
        }
      }
    }

    // ---- raw cell placements (address resolution) --------------------------
    let sheet_cells = cells_by_sheet.remove(&sheet_id).unwrap_or_default();
    let mut cell_ids: Vec<CellId> = sheet_cells.keys().copied().collect();
    cell_ids.sort();
    let mut placements: Vec<Placement> = Vec::with_capacity(cell_ids.len());
    let mut records_by_id: HashMap<CellId, LoroMap> = HashMap::with_capacity(cell_ids.len());
    for cell_id in cell_ids {
      let cell_record = sheet_cells
        .get(&cell_id)
        .expect("key just enumerated")
        .clone();
      let column_ix = map_uuid(&cell_record, "column_id")
        .and_then(|column| columns.iter().position(|candidate| candidate.id == column))
        .unwrap_or_else(|| {
          defects.push(FlowDefect::UnknownColumnReassigned { cell: cell_id });
          0
        });
      let row_id = match map_uuid(&cell_record, "row_id") {
        Some(row_id) => row_id,
        None => {
          defects.push(FlowDefect::CellRowInvalid { cell: cell_id });
          bump_row_id(cell_id, 0)
        },
      };
      placements.push(Placement {
        cell: cell_id,
        row: row_id,
        column_ix,
      });
      records_by_id.insert(cell_id, cell_record);
    }

    // ---- normalization: phantom rows, then collision bump-down ------------
    normalize_grid(&mut rows, &mut row_seen, &mut placements, &columns, sheet_id, &mut defects);

    // ---- summaries: reuse the cache unless the cell is dirty --------------
    let mut summaries: HashMap<CellId, CellSummary> = HashMap::with_capacity(placements.len());
    for placement in &placements {
      let cell_id = placement.cell;
      if let Some(dirty) = dirty
        && !dirty.contains(&cell_id)
        && let Some(cached) = summary_cache.get(&cell_id)
      {
        summaries.insert(cell_id, cached.clone());
        continue;
      }
      let Some(cell_record) = records_by_id.get(&cell_id) else {
        continue;
      };
      let summary = match cell_document_from_record(doc, cell_record) {
        Ok(document) => derive_cell_summary(&document),
        Err(error) => {
          defects.push(FlowDefect::CellFlowInvalid {
            cell: cell_id,
            error: error.to_string(),
          });
          CellSummary::default()
        },
      };
      summaries.insert(cell_id, summary);
    }

    // ---- assemble aligned grid rows ----------------------------------------
    let heights = row_height_overrides(&record);
    let row_positions: HashMap<RowId, usize> = rows.iter().enumerate().map(|(ix, id)| (*id, ix)).collect();
    let mut grid_rows: Vec<GridRow> = rows
      .iter()
      .map(|row_id| GridRow {
        id: *row_id,
        height_override: heights.get(row_id).copied(),
        cells: vec![None; columns.len()],
      })
      .collect();
    for placement in placements {
      let Some(&row_ix) = row_positions.get(&placement.row) else {
        continue;
      };
      let summary = summaries.get(&placement.cell).cloned().unwrap_or_default();
      grid_rows[row_ix].cells[placement.column_ix] = Some(Cell {
        id: placement.cell,
        row_id: placement.row,
        column_id: columns[placement.column_ix].id,
        summary,
      });
    }

    let annotations = sheet_annotations(doc, sheet_id);
    sheets.push(Sheet {
      id: sheet_id,
      name,
      sheet_type_id: sheet_type_id.unwrap_or_default(),
      columns,
      rows: grid_rows,
      annotations,
    });
  }

  Ok(MaterializedBoard {
    board: FlowBoardProjection { format, sheets },
    defects,
  })
}

struct Placement {
  cell: CellId,
  row: RowId,
  column_ix: usize,
}

/// Normalization steps 3 + 5 (spec §3), pure over ids: phantom rows for
/// dangling addresses, then slot-collision bump-down. Deterministic on
/// canonical state: phantoms append sorted by uuid; per contested slot the
/// least-uuid cell wins; losers land in [`bump_row_id`] rows inserted
/// immediately after the contested row in (column, uuid) order. Cascades
/// re-salt by round and terminate (each round's targets are fresh rows).
fn normalize_grid(
  rows: &mut Vec<RowId>,
  row_seen: &mut HashSet<RowId>,
  placements: &mut [Placement],
  columns: &[GridColumn],
  sheet_id: SheetId,
  defects: &mut Vec<FlowDefect>,
) {
  // (3) phantom rows: every referenced-but-unlisted row materializes at the
  // bottom, sorted by uuid.
  let mut phantoms: Vec<RowId> = placements
    .iter()
    .filter(|placement| !row_seen.contains(&placement.row))
    .map(|placement| placement.row)
    .collect::<HashSet<_>>()
    .into_iter()
    .collect();
  phantoms.sort();
  for row_id in phantoms {
    defects.push(FlowDefect::RowMissingFromOrder { sheet: sheet_id, row: row_id });
    row_seen.insert(row_id);
    rows.push(row_id);
  }

  // (5) slot collisions, bump-down rounds.
  let mut round: u32 = 1;
  loop {
    let mut by_slot: HashMap<(RowId, usize), Vec<usize>> = HashMap::new();
    for (index, placement) in placements.iter().enumerate() {
      by_slot
        .entry((placement.row, placement.column_ix))
        .or_default()
        .push(index);
    }
    // Contested slots in deterministic order: row position (at round start),
    // then column. Keyed by row IDENTITY below — synth-row insertions shift
    // positions mid-round.
    let row_positions: HashMap<RowId, usize> = rows.iter().enumerate().map(|(ix, id)| (*id, ix)).collect();
    let mut contested: Vec<(usize, usize, RowId, Vec<usize>)> = by_slot
      .into_iter()
      .filter(|(_, members)| members.len() > 1)
      .map(|((row, column_ix), members)| (row_positions[&row], column_ix, row, members))
      .collect();
    if contested.is_empty() || round > 64 {
      break;
    }
    contested.sort_by_key(|(row_pos, column_ix, _, _)| (*row_pos, *column_ix));
    for (_, _, contested_row, mut members) in contested {
      members.sort_by_key(|&index| placements[index].cell);
      let row_pos = rows
        .iter()
        .position(|row| *row == contested_row)
        .expect("contested row is in the live row list");
      // Winner keeps the slot; losers bump, inserted after the contested row
      // (and after synth rows already inserted there this round).
      let mut insert_at = row_pos + 1;
      for &loser_index in &members[1..] {
        let loser = placements[loser_index].cell;
        let column_id = columns[placements[loser_index].column_ix].id;
        let target = bump_row_id(loser, round);
        if row_seen.insert(target) {
          let position = insert_at.min(rows.len());
          rows.insert(position, target);
          insert_at = position + 1;
        }
        placements[loser_index].row = target;
        defects.push(FlowDefect::SlotCollisionBumped {
          cell: loser,
          row: contested_row,
          column: column_id,
        });
      }
    }
    round += 1;
  }
}

fn grid_column(column_id: ColumnId, record: &LoroMap) -> GridColumn {
  GridColumn {
    id: column_id,
    label: map_string(record, "label").unwrap_or_default(),
    side: map_string(record, "side")
      .and_then(|value| loro_schema::parse_side(&value))
      .unwrap_or(crate::format::ArgumentSide::One),
    width: map_f64(record, "width").map(|value| value as f32),
  }
}

fn row_height_overrides(sheet_record: &LoroMap) -> HashMap<RowId, f32> {
  let mut heights = HashMap::new();
  let Some(map) = child_map(sheet_record, loro_schema::ROW_HEIGHTS_KEY) else {
    return heights;
  };
  for key in loro_schema::map_keys(&map) {
    let (Ok(row_id), Some(height)) = (Uuid::parse_str(&key), map_f64(&map, &key)) else {
      continue;
    };
    heights.insert(row_id, height as f32);
  }
  heights
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
