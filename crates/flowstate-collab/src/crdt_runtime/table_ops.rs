//! §P2b id-addressed canonical table operations.
//!
//! Every granular table structural command resolves its target by a **durable
//! id** (`RowId` / `ColumnId` / deterministic `CellId`) plus an anchor, never a
//! raw positional index against a possibly-merged order list. This is the
//! convergence-critical half of the table surface: two peers that concurrently
//! apply the same command (or replay a remote one) address the identical Loro
//! child containers, so Loro map / movable-list LWW merges rather than duplicates
//! or resolves a stale index.
//!
//! Position resolution is identical to the editor replay: `index_of(id)` scans
//! the order list; insert-after `Some(anchor)` places the new id immediately
//! after the anchor, `None` at the head, and an anchor that is present-but-absent
//! (concurrently deleted) falls back to the tail deterministically. Every op is
//! idempotent: containers are `ensure`d by their stable id, and an id is only
//! pushed into an order list when it is not already present.
//!
//! `loro_import.rs::import_table` (flowstate-document) is the reference for the
//! exact per-cell map fields; the whole-table writers in `crdt_runtime.rs`
//! (`write_table_map_from_input` / `write_table_cell_map_from_input`) share the
//! same id scheme so create-only merges never rekey.

use anyhow::Result;
use flowstate_document::{
  BlockId, CellId, ColumnId, InputTableCell, InputTableColumnWidth, InputTableRow, RowId, TABLE_CELLS_BY_ID, TABLE_COLUMN_ORDER,
  TABLE_COLUMNS_BY_ID, TABLE_ROW_ORDER, TABLE_ROWS_BY_ID, cell_loro_id, cell_loro_id_for, column_loro_id, parse_column_loro_id,
  parse_row_loro_id, row_loro_id,
};
use loro::{ContainerTrait as _, LoroDoc};

use super::{
  child_map, child_movable_list, empty_input_table_cell, map_keys, map_string_opt, movable_list_strings, projection_table_map_by_block_id,
  update_table_cell_map_from_input, write_table_cell_map_from_input, write_table_column_width,
};

/// The order-list index at which an id anchored `after` should be inserted.
///
/// `None` => head (0); `Some(anchor)` present => immediately after it; anchor
/// absent (concurrently deleted) => tail. Identical to the editor replay and the
/// predicted-projection path so optimistic and canonical state agree.
fn resolve_after_position(order: &[String], after: Option<&str>) -> usize {
  match after {
    None => 0,
    Some(anchor) => order
      .iter()
      .position(|id| id == anchor)
      .map_or(order.len(), |ix| ix + 1),
  }
}

/// §P2b `InsertTableRow`: insert `row_loro_id(new_row_id)` into `row_order` at the
/// anchor-resolved position (skipped if already present), ensure the row record,
/// and materialize one durable-coordinate cell per existing column from
/// `row.cells` (empty fallback for any short row).
pub(super) fn insert_table_row(
  doc: &LoroDoc,
  table_block_id: BlockId,
  new_row_id: RowId,
  after_row: Option<RowId>,
  row: &InputTableRow,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, "skipping table row insert because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let row_order = table.ensure_mergeable_movable_list(TABLE_ROW_ORDER)?;
  let column_order = table.ensure_mergeable_movable_list(TABLE_COLUMN_ORDER)?;
  let rows_by_id = table.ensure_mergeable_map(TABLE_ROWS_BY_ID)?;
  let cells_by_id = table.ensure_mergeable_map(TABLE_CELLS_BY_ID)?;
  let column_ids = movable_list_strings(&column_order);
  if column_ids.is_empty() {
    tracing::warn!(?table_block_id, "skipping table row insert because the table has no columns");
    return Ok(false);
  }

  let row_id_str = row_loro_id(new_row_id);
  let row_order_ids = movable_list_strings(&row_order);
  if !row_order_ids.iter().any(|id| id == &row_id_str) {
    let pos = resolve_after_position(&row_order_ids, after_row.map(row_loro_id).as_deref());
    row_order.insert(pos.min(row_order.len()), row_id_str.as_str())?;
  }
  let row_map = rows_by_id.ensure_mergeable_map(&row_id_str)?;
  row_map.insert("id", row_id_str.as_str())?;
  row_map.insert("container_id", row_map.id().to_string())?;
  let attrs = row_map.ensure_mergeable_map("attrs")?;
  row_map.insert("attrs_container_id", attrs.id().to_string())?;

  for (column_ix, column_id_str) in column_ids.iter().enumerate() {
    let Some(column_id) = parse_column_loro_id(column_id_str) else {
      continue;
    };
    let cell_id_str = cell_loro_id_for(new_row_id, column_id);
    let cell_map = cells_by_id.ensure_mergeable_map(&cell_id_str)?;
    let fallback = empty_input_table_cell(new_row_id, column_id);
    let cell = row.cells.get(column_ix).unwrap_or(&fallback);
    write_table_cell_map_from_input(doc, &cell_map, &cell_id_str, &row_id_str, column_id_str, cell, true)?;
  }
  Ok(true)
}

/// §P2b `DeleteTableRow`: remove the id from `row_order`, delete its row record,
/// and delete every deterministic-coordinate cell for the row (plus, defensively,
/// any cell whose stored `row_id` field still names it). Convergent no-op when the
/// row was already removed.
pub(super) fn delete_table_row(doc: &LoroDoc, table_block_id: BlockId, row_id: RowId) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, "skipping table row delete because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(row_order) = child_movable_list(&table, TABLE_ROW_ORDER) else {
    tracing::warn!(?table_block_id, "skipping table row delete because the table has no row order");
    return Ok(false);
  };
  let row_id_str = row_loro_id(row_id);
  let row_ids = movable_list_strings(&row_order);
  let Some(row_ix) = row_ids.iter().position(|id| id == &row_id_str) else {
    return Ok(false);
  };
  row_order.delete(row_ix, 1)?;
  if let Some(rows_by_id) = child_map(&table, TABLE_ROWS_BY_ID) {
    rows_by_id.delete(&row_id_str)?;
  }
  if let Some(cells_by_id) = child_map(&table, TABLE_CELLS_BY_ID) {
    if let Some(column_order) = child_movable_list(&table, TABLE_COLUMN_ORDER) {
      for column_id_str in movable_list_strings(&column_order) {
        if let Some(column_id) = parse_column_loro_id(&column_id_str) {
          cells_by_id.delete(&cell_loro_id_for(row_id, column_id))?;
        }
      }
    }
    for cell_id in map_keys(&cells_by_id) {
      let orphaned = child_map(&cells_by_id, &cell_id)
        .and_then(|cell| map_string_opt(&cell, "row_id"))
        .as_deref()
        == Some(row_id_str.as_str());
      if orphaned {
        cells_by_id.delete(&cell_id)?;
      }
    }
  }
  Ok(true)
}

/// §P2b `MoveTableRow`: `row_order.mov(index_of(row_id), target)` where `target`
/// is resolved from `after_row` exactly as an insert-after position.
pub(super) fn move_table_row(doc: &LoroDoc, table_block_id: BlockId, row_id: RowId, after_row: Option<RowId>) -> Result<bool> {
  move_table_axis(
    doc,
    table_block_id,
    TABLE_ROW_ORDER,
    &row_loro_id(row_id),
    after_row.map(row_loro_id).as_deref(),
  )
}

/// §P2b `MoveTableColumn`: `column_order.mov(index_of(column_id), target)`.
pub(super) fn move_table_column(doc: &LoroDoc, table_block_id: BlockId, column_id: ColumnId, after_column: Option<ColumnId>) -> Result<bool> {
  move_table_axis(
    doc,
    table_block_id,
    TABLE_COLUMN_ORDER,
    &column_loro_id(column_id),
    after_column.map(column_loro_id).as_deref(),
  )
}

/// Shared movable-list reorder for a row/column order list. Resolves the moved id
/// and the anchor by value (never a raw index) and computes the final Loro `mov`
/// target so that, after the move, the element sits immediately after the anchor
/// (head for `None`, tail when the anchor is absent). No-op when the id is absent
/// or already in place.
fn move_table_axis(doc: &LoroDoc, table_block_id: BlockId, order_key: &str, target_id: &str, after_id: Option<&str>) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, order_key, "skipping table move because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(order) = child_movable_list(&table, order_key) else {
    tracing::warn!(?table_block_id, order_key, "skipping table move because its order list is missing");
    return Ok(false);
  };
  let ids = movable_list_strings(&order);
  let Some(from) = ids.iter().position(|id| id == target_id) else {
    return Ok(false);
  };
  // `mov(from, to)` removes the element then re-inserts it at `to` in the reduced
  // list. Translate "immediately after the anchor" into that reduced index.
  let target = match after_id {
    None => 0,
    Some(anchor) => match ids.iter().position(|id| id == anchor) {
      Some(anchor_ix) if anchor_ix < from => anchor_ix + 1,
      Some(anchor_ix) => anchor_ix,
      None => ids.len().saturating_sub(1),
    },
  };
  if target == from {
    return Ok(false);
  }
  order.mov(from, target)?;
  Ok(true)
}

/// §P2b `InsertTableColumn`: insert `column_loro_id(new_column_id)` into
/// `column_order` at the anchor-resolved position, ensure the column record with
/// its width, and materialize one durable-coordinate cell per existing row from
/// `cells` (empty fallback for any short list).
pub(super) fn insert_table_column(
  doc: &LoroDoc,
  table_block_id: BlockId,
  new_column_id: ColumnId,
  after_column: Option<ColumnId>,
  width: &InputTableColumnWidth,
  cells: &[InputTableCell],
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, "skipping table column insert because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let row_order = table.ensure_mergeable_movable_list(TABLE_ROW_ORDER)?;
  let column_order = table.ensure_mergeable_movable_list(TABLE_COLUMN_ORDER)?;
  let rows_by_id = table.ensure_mergeable_map(TABLE_ROWS_BY_ID)?;
  let columns_by_id = table.ensure_mergeable_map(TABLE_COLUMNS_BY_ID)?;
  let cells_by_id = table.ensure_mergeable_map(TABLE_CELLS_BY_ID)?;
  table.insert("container_id", table.id().to_string())?;
  table.insert("row_order_container_id", row_order.id().to_string())?;
  table.insert("column_order_container_id", column_order.id().to_string())?;
  table.insert("rows_container_id", rows_by_id.id().to_string())?;
  table.insert("columns_container_id", columns_by_id.id().to_string())?;
  table.insert("cells_container_id", cells_by_id.id().to_string())?;
  let row_ids = movable_list_strings(&row_order);
  if row_ids.is_empty() {
    tracing::warn!(?table_block_id, "skipping table column insert because the table has no rows");
    return Ok(false);
  }

  let column_id_str = column_loro_id(new_column_id);
  let column_order_ids = movable_list_strings(&column_order);
  if !column_order_ids.iter().any(|id| id == &column_id_str) {
    let pos = resolve_after_position(&column_order_ids, after_column.map(column_loro_id).as_deref());
    column_order.insert(pos.min(column_order.len()), column_id_str.as_str())?;
  }
  let column_map = columns_by_id.ensure_mergeable_map(&column_id_str)?;
  column_map.insert("id", column_id_str.as_str())?;
  column_map.insert("container_id", column_map.id().to_string())?;
  let attrs = column_map.ensure_mergeable_map("attrs")?;
  column_map.insert("attrs_container_id", attrs.id().to_string())?;
  write_table_column_width(&column_map, width)?;

  for (row_ix, row_id_str) in row_ids.iter().enumerate() {
    let Some(row_id) = parse_row_loro_id(row_id_str) else {
      continue;
    };
    let cell_id_str = cell_loro_id_for(row_id, new_column_id);
    let cell_map = cells_by_id.ensure_mergeable_map(&cell_id_str)?;
    let fallback = empty_input_table_cell(row_id, new_column_id);
    let cell = cells.get(row_ix).unwrap_or(&fallback);
    write_table_cell_map_from_input(doc, &cell_map, &cell_id_str, row_id_str, &column_id_str, cell, true)?;
  }
  Ok(true)
}

/// §P2b `DeleteTableColumn`: remove the id from `column_order`, delete its column
/// record, and delete every deterministic-coordinate cell for the column (plus,
/// defensively, any cell whose stored `column_id` field still names it).
pub(super) fn delete_table_column(doc: &LoroDoc, table_block_id: BlockId, column_id: ColumnId) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, "skipping table column delete because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(column_order) = child_movable_list(&table, TABLE_COLUMN_ORDER) else {
    tracing::warn!(?table_block_id, "skipping table column delete because the table has no column order");
    return Ok(false);
  };
  let column_id_str = column_loro_id(column_id);
  let column_ids = movable_list_strings(&column_order);
  let Some(column_ix) = column_ids.iter().position(|id| id == &column_id_str) else {
    return Ok(false);
  };
  column_order.delete(column_ix, 1)?;
  if let Some(columns_by_id) = child_map(&table, TABLE_COLUMNS_BY_ID) {
    columns_by_id.delete(&column_id_str)?;
  }
  if let Some(cells_by_id) = child_map(&table, TABLE_CELLS_BY_ID) {
    if let Some(row_order) = child_movable_list(&table, TABLE_ROW_ORDER) {
      for row_id_str in movable_list_strings(&row_order) {
        if let Some(row_id) = parse_row_loro_id(&row_id_str) {
          cells_by_id.delete(&cell_loro_id_for(row_id, column_id))?;
        }
      }
    }
    for cell_id in map_keys(&cells_by_id) {
      let orphaned = child_map(&cells_by_id, &cell_id)
        .and_then(|cell| map_string_opt(&cell, "column_id"))
        .as_deref()
        == Some(column_id_str.as_str());
      if orphaned {
        cells_by_id.delete(&cell_id)?;
      }
    }
  }
  Ok(true)
}

/// §P2b `ReplaceTableCell`: `ensure` the cell at the deterministic
/// `(row_id, column_id)` coordinate and rewrite its spans + flow text from `cell`.
pub(super) fn replace_table_cell(
  doc: &LoroDoc,
  table_block_id: BlockId,
  row_id: RowId,
  column_id: ColumnId,
  cell: &InputTableCell,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, "skipping table cell replace because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let cells_by_id = table.ensure_mergeable_map(TABLE_CELLS_BY_ID)?;
  let cell_id_str = cell_loro_id(CellId::from_coordinate(row_id, column_id));
  let cell_map = cells_by_id.ensure_mergeable_map(&cell_id_str)?;
  update_table_cell_map_from_input(doc, &cell_map, &cell_id_str, &row_loro_id(row_id), &column_loro_id(column_id), cell)?;
  Ok(true)
}

/// §P2b `SetTableCellSpan`: set the two span fields on the deterministic
/// `(row_id, column_id)` cell. No-op when the coordinate cell does not exist yet
/// (the topology repair materializes it, then a re-applied span converges).
pub(super) fn set_table_cell_span(
  doc: &LoroDoc,
  table_block_id: BlockId,
  row_id: RowId,
  column_id: ColumnId,
  row_span: u16,
  column_span: u16,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, "skipping table span command because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(cells_by_id) = child_map(&table, TABLE_CELLS_BY_ID) else {
    return Ok(false);
  };
  let Some(cell) = child_map(&cells_by_id, &cell_loro_id_for(row_id, column_id)) else {
    return Ok(false);
  };
  cell.insert("row_span", i64::from(row_span.max(1)))?;
  cell.insert("column_span", i64::from(column_span.max(1)))?;
  Ok(true)
}

/// §P2b `SetTableColumnWidth`: the frozen command still addresses the column by
/// its position (`column_ix`), so resolve that index against `column_order` to the
/// durable column id, then write the width onto the id-keyed column record.
pub(super) fn set_table_column_width(
  doc: &LoroDoc,
  table_block_id: BlockId,
  column_ix: usize,
  width: &InputTableColumnWidth,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(column_order) = child_movable_list(&table, TABLE_COLUMN_ORDER) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because the table has no column order");
    return Ok(false);
  };
  let column_ids = movable_list_strings(&column_order);
  let Some(column_id) = column_ids.get(column_ix) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because the column index is out of range");
    return Ok(false);
  };
  let Some(columns_by_id) = child_map(&table, TABLE_COLUMNS_BY_ID) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because the table has no columns map");
    return Ok(false);
  };
  let column = columns_by_id.ensure_mergeable_map(column_id)?;
  write_table_column_width(&column, width)?;
  Ok(true)
}
