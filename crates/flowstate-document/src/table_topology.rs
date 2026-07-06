//! Pure table-topology normalization (§P2b, FS-010).
//!
//! Turns the raw set of durable-id cell records read from canonical Loro
//! (`row_order` / `column_order` / `cells_by_id`) into a full, well-formed grid
//! plus a list of [`TableTopologyDefect`]s describing what had to be repaired.
//!
//! This module is deliberately **pure**: it depends only on `std`, the durable
//! id types ([`RowId`], [`ColumnId`], [`CellId`]) and their canonical
//! [`CellId::from_coordinate`] mix. It has no Loro, GPUI, or I/O dependency, so
//! the identical normalization runs on both sides of the refactor — the
//! read-side projector (which builds [`RawCellRecord`]s from Loro) and the
//! runtime repair transaction (which turns the reported defects into convergent
//! canonical mutations). Every output is a deterministic function of the inputs,
//! so two peers holding the same canonical state produce byte-identical grids
//! and defect lists.

use std::collections::hash_map::Entry;

use gpui_flowtext::{CellId, ColumnId, RowId};
// §perf: Fx hashing for these u128 durable-id keyed maps/sets. The keys are trusted
// canonical ids (not attacker-controlled) and the map is documented as never iterated,
// so switching from SipHash is faster and does not affect determinism.
use rustc_hash::{FxHashMap, FxHashSet};

/// A single cell record as read from canonical `cells_by_id` (§P2b).
///
/// The projector builds one of these per cell container actually present in
/// Loro; [`normalize`] reconciles them against the ordered `row_order` /
/// `column_order` id lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawCellRecord {
  /// Durable id of the row this cell claims to occupy.
  pub row_id: RowId,
  /// Durable id of the column this cell claims to occupy.
  pub column_id: ColumnId,
  /// Durable id of the cell container itself, as read (never recomputed).
  pub cell_id: CellId,
  /// Row span as stored, before clamping to the grid edge.
  pub row_span: u16,
  /// Column span as stored, before clamping to the grid edge.
  pub col_span: u16,
}

/// A normalized cell occupying one `(row, column)` coordinate of the full grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalizedCell {
  /// Durable id of the row this cell occupies.
  pub row_id: RowId,
  /// Durable id of the column this cell occupies.
  pub column_id: ColumnId,
  /// Durable id of the cell container. Left as read for a present cell; set via
  /// [`CellId::from_coordinate`] for a synthesized one.
  pub cell_id: CellId,
  /// Row span, clamped so it never runs past the last row.
  pub row_span: u16,
  /// Column span, clamped so it never runs past the last column.
  pub col_span: u16,
  /// True when this cell was synthesized to fill a missing coordinate (had no
  /// [`RawCellRecord`]).
  pub synthesized: bool,
}

/// A well-formedness anomaly found while normalizing a table grid (§P2b,
/// FS-010).
///
/// Each variant names the `(row, column)` coordinate it concerns so the runtime
/// repair transaction can key an idempotent, convergent canonical mutation on
/// the stable durable ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableTopologyDefect {
  /// No [`RawCellRecord`] existed for this coordinate; a cell was synthesized.
  MissingCell { row_id: RowId, column_id: ColumnId },
  /// Two or more raw records claimed this coordinate; one was kept
  /// deterministically and the rest dropped.
  DuplicateCoordinate { row_id: RowId, column_id: ColumnId },
  /// A present cell's `row_span` / `col_span` had to be clamped to the grid
  /// edge.
  InvalidSpan { row_id: RowId, column_id: ColumnId },
  /// A raw record named a `row_id` or `column_id` absent from the order lists;
  /// it was dropped.
  OrphanCell { row_id: RowId, column_id: ColumnId },
}

/// The result of [`normalize`]: a full grid plus the defects that were repaired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedTable {
  /// Row-major full grid: exactly one [`NormalizedCell`] for every
  /// `(row_id, column_id)` coordinate, ordered by `row_ids` then `column_ids`
  /// (the caller-supplied order). Always `row_ids.len() * column_ids.len()`
  /// long.
  pub cells: Vec<NormalizedCell>,
  /// Defects found, in the deterministic order documented on [`normalize`].
  pub defects: Vec<TableTopologyDefect>,
}

/// Normalize a raw table grid into a full, well-formed grid plus a defect list
/// (§P2b, FS-010).
///
/// `row_ids` / `column_ids` are the ordered durable ids from `row_order` /
/// `column_order`; `raw` is every cell record read from `cells_by_id`.
///
/// Rules — all deterministic, so two peers with the same inputs produce
/// byte-identical output:
/// * **Missing:** for every `(row, column)` coordinate with no raw record,
///   synthesize a [`NormalizedCell`] with
///   `cell_id = CellId::from_coordinate(row, column)`, `row_span` = 1,
///   `col_span` = 1, `synthesized` = true, and record a `MissingCell` defect.
/// * **Orphan:** a raw record whose `row_id` or `column_id` is absent from the
///   order lists is dropped and reported as an `OrphanCell` defect.
/// * **Duplicate:** when two or more raw records share a coordinate, keep
///   exactly one deterministically — the smallest `cell_id.0`, tie-broken by the
///   smallest `(row_span, col_span)` — and record `DuplicateCoordinate` once.
/// * **Span clamp:** `row_span` / `col_span` are clamped to at least 1 and at
///   most the number of rows / columns remaining from this cell's position, so a
///   span never runs past the grid edge. If a clamp changed a value, record an
///   `InvalidSpan` defect. A synthesized cell always has spans 1/1 and is never
///   `InvalidSpan`.
///
/// A present-and-kept cell's `cell_id` is left exactly as read (never
/// recomputed); only synthesized cells get [`CellId::from_coordinate`]. The
/// returned `cells` is always exactly `row_ids.len() * column_ids.len()` long.
///
/// Defect ordering is fully deterministic: first every `OrphanCell` in `raw`
/// input order, then a row-major pass over the grid appending, per coordinate,
/// either `MissingCell` (absent) or — for a present coordinate —
/// `DuplicateCoordinate` then `InvalidSpan`.
#[must_use]
pub fn normalize(row_ids: &[RowId], column_ids: &[ColumnId], raw: &[RawCellRecord]) -> NormalizedTable {
  let row_set: FxHashSet<u128> = row_ids.iter().map(|row| row.0).collect();
  let column_set: FxHashSet<u128> = column_ids.iter().map(|column| column.0).collect();

  let mut defects: Vec<TableTopologyDefect> = Vec::new();

  // Single pass over `raw`: orphans are reported here (in input order, before
  // any grid defect); surviving records are resolved into `chosen`. The map is
  // only ever queried by coordinate below, never iterated, so output order is
  // driven purely by the ordered id lists and stays deterministic regardless of
  // `HashMap` iteration order.
  let mut chosen: FxHashMap<(u128, u128), RawCellRecord> = FxHashMap::default();
  let mut duplicate_coords: FxHashSet<(u128, u128)> = FxHashSet::default();
  for record in raw {
    if !row_set.contains(&record.row_id.0) || !column_set.contains(&record.column_id.0) {
      defects.push(TableTopologyDefect::OrphanCell {
        row_id: record.row_id,
        column_id: record.column_id,
      });
      continue;
    }
    let key = (record.row_id.0, record.column_id.0);
    match chosen.entry(key) {
      Entry::Vacant(slot) => {
        slot.insert(*record);
      },
      Entry::Occupied(mut slot) => {
        duplicate_coords.insert(key);
        // Keep the global minimum survivor key, so the choice is independent of
        // the order duplicates appear in `raw`.
        if survivor_key(record) < survivor_key(slot.get()) {
          slot.insert(*record);
        }
      },
    }
  }

  // Row-major grid pass: fill every coordinate and append its per-coordinate
  // defects in grid order.
  let mut cells: Vec<NormalizedCell> = Vec::with_capacity(row_ids.len().saturating_mul(column_ids.len()));
  for (row_index, row_id) in row_ids.iter().enumerate() {
    let rows_remaining = row_ids.len() - row_index;
    for (column_index, column_id) in column_ids.iter().enumerate() {
      let columns_remaining = column_ids.len() - column_index;
      let key = (row_id.0, column_id.0);
      match chosen.get(&key) {
        None => {
          cells.push(NormalizedCell {
            row_id: *row_id,
            column_id: *column_id,
            cell_id: CellId::from_coordinate(*row_id, *column_id),
            row_span: 1,
            col_span: 1,
            synthesized: true,
          });
          defects.push(TableTopologyDefect::MissingCell {
            row_id: *row_id,
            column_id: *column_id,
          });
        },
        Some(record) => {
          if duplicate_coords.contains(&key) {
            defects.push(TableTopologyDefect::DuplicateCoordinate {
              row_id: *row_id,
              column_id: *column_id,
            });
          }
          let row_span = clamp_span(record.row_span, rows_remaining);
          let col_span = clamp_span(record.col_span, columns_remaining);
          if row_span != record.row_span || col_span != record.col_span {
            defects.push(TableTopologyDefect::InvalidSpan {
              row_id: *row_id,
              column_id: *column_id,
            });
          }
          cells.push(NormalizedCell {
            row_id: *row_id,
            column_id: *column_id,
            cell_id: record.cell_id,
            row_span,
            col_span,
            synthesized: false,
          });
        },
      }
    }
  }

  NormalizedTable { cells, defects }
}

/// Deterministic survivor ordering among duplicate records at one coordinate:
/// smallest `cell_id`, then smallest `(row_span, col_span)`.
fn survivor_key(record: &RawCellRecord) -> (u128, u16, u16) {
  (record.cell_id.0, record.row_span, record.col_span)
}

/// Clamp a span into `1..=max_remaining` so it never runs past the grid edge.
/// `max_remaining` is the count of rows/columns from this cell's position to the
/// last one and is always at least 1.
fn clamp_span(span: u16, max_remaining: usize) -> u16 {
  let ceiling = u16::try_from(max_remaining).unwrap_or(u16::MAX);
  span.clamp(1, ceiling)
}

#[cfg(test)]
mod tests {
  use super::{RawCellRecord, TableTopologyDefect, normalize};
  use gpui_flowtext::{CellId, ColumnId, RowId};
  use std::collections::HashSet;

  #[test]
  fn full_grid_synthesis() {
    let r0 = RowId(10);
    let r1 = RowId(20);
    let c0 = ColumnId(30);
    let c1 = ColumnId(40);
    let present_00 = CellId(1000);
    let present_11 = CellId(2000);
    let raw = [
      RawCellRecord {
        row_id: r0,
        column_id: c0,
        cell_id: present_00,
        row_span: 1,
        col_span: 1,
      },
      RawCellRecord {
        row_id: r1,
        column_id: c1,
        cell_id: present_11,
        row_span: 1,
        col_span: 1,
      },
    ];

    let table = normalize(&[r0, r1], &[c0, c1], &raw);

    // Full 2x2 grid, row-major: (r0,c0),(r0,c1),(r1,c0),(r1,c1).
    assert_eq!(table.cells.len(), 4);
    assert_eq!(table.cells[0].cell_id, present_00);
    assert!(!table.cells[0].synthesized);
    assert_eq!(table.cells[3].cell_id, present_11);
    assert!(!table.cells[3].synthesized);

    // The two absent coordinates are synthesized with from_coordinate ids + 1/1.
    let synth_01 = table.cells[1];
    assert!(synth_01.synthesized);
    assert_eq!(synth_01.cell_id, CellId::from_coordinate(r0, c1));
    assert_eq!((synth_01.row_span, synth_01.col_span), (1, 1));
    let synth_10 = table.cells[2];
    assert!(synth_10.synthesized);
    assert_eq!(synth_10.cell_id, CellId::from_coordinate(r1, c0));
    assert_eq!((synth_10.row_span, synth_10.col_span), (1, 1));

    assert_eq!(
      table.defects,
      vec![
        TableTopologyDefect::MissingCell { row_id: r0, column_id: c1 },
        TableTopologyDefect::MissingCell { row_id: r1, column_id: c0 },
      ]
    );
  }

  #[test]
  fn span_clamp() {
    let r0 = RowId(1);
    let r1 = RowId(2);
    let c0 = ColumnId(1);
    let raw = [
      RawCellRecord {
        row_id: r0,
        column_id: c0,
        cell_id: CellId(100),
        row_span: 9,
        col_span: 1,
      },
      RawCellRecord {
        row_id: r1,
        column_id: c0,
        cell_id: CellId(200),
        row_span: 1,
        col_span: 1,
      },
    ];

    let table = normalize(&[r0, r1], &[c0], &raw);

    assert_eq!(table.cells.len(), 2);
    // Row 0's span of 9 clamps to the two rows remaining from position 0.
    assert_eq!(table.cells[0].row_span, 2);
    assert_eq!(table.cells[0].col_span, 1);
    assert_eq!(
      table.defects,
      vec![TableTopologyDefect::InvalidSpan { row_id: r0, column_id: c0 }]
    );
  }

  #[test]
  fn orphan_drop() {
    let r0 = RowId(1);
    let c0 = ColumnId(1);
    let ghost_row = RowId(999);
    let raw = [RawCellRecord {
      row_id: ghost_row,
      column_id: c0,
      cell_id: CellId(7),
      row_span: 1,
      col_span: 1,
    }];

    let table = normalize(&[r0], &[c0], &raw);

    // The orphan is dropped; the grid is still full via synthesis.
    assert_eq!(table.cells.len(), 1);
    assert!(table.cells[0].synthesized);
    assert_eq!(table.cells[0].cell_id, CellId::from_coordinate(r0, c0));
    assert_eq!(
      table.defects,
      vec![
        TableTopologyDefect::OrphanCell {
          row_id: ghost_row,
          column_id: c0,
        },
        TableTopologyDefect::MissingCell { row_id: r0, column_id: c0 },
      ]
    );
  }

  #[test]
  fn duplicate_determinism() {
    let r0 = RowId(1);
    let c0 = ColumnId(1);
    let raw = [
      RawCellRecord {
        row_id: r0,
        column_id: c0,
        cell_id: CellId(5000),
        row_span: 1,
        col_span: 1,
      },
      RawCellRecord {
        row_id: r0,
        column_id: c0,
        cell_id: CellId(3000),
        row_span: 1,
        col_span: 1,
      },
    ];

    let first = normalize(&[r0], &[c0], &raw);
    let second = normalize(&[r0], &[c0], &raw);
    assert_eq!(first, second);

    assert_eq!(first.cells.len(), 1);
    // The smallest cell_id.0 survives.
    assert_eq!(first.cells[0].cell_id, CellId(3000));
    assert!(!first.cells[0].synthesized);
    assert_eq!(
      first.defects,
      vec![TableTopologyDefect::DuplicateCoordinate { row_id: r0, column_id: c0 }]
    );
  }

  #[test]
  fn stable_cell_uid_uniqueness() {
    let rows: Vec<RowId> = (1..=10).map(RowId).collect();
    let columns: Vec<ColumnId> = (1..=10).map(ColumnId).collect();

    let mut ids: HashSet<CellId> = HashSet::new();
    for row in &rows {
      for column in &columns {
        let id = CellId::from_coordinate(*row, *column);
        // Injective across the 10x10 distinct coordinates.
        assert!(ids.insert(id), "distinct coordinates must map to distinct cell ids");
        // Stable: recomputing the same coordinate yields the identical id.
        assert_eq!(id, CellId::from_coordinate(*row, *column));
      }
    }
    assert_eq!(ids.len(), 100);
  }
}
