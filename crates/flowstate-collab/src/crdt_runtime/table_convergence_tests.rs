//! §P2b multi-peer table convergence. Durable row/column/cell ids must let
//! concurrent structural table edits merge without misalignment, and the
//! projection-repair pipeline must deterministically fill the concurrent
//! add-row × add-column cross-cell gap (FS-010) so every peer reads the
//! identical grid. Drives two `CrdtRuntime`s from a shared table base and
//! cross-applies their updates, then asserts byte-identical projections.

use anyhow::Result;
use flowstate_document::{
  BlockId, ColumnId, DocumentProjection, InputBlock, InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock,
  InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RowId, RunStyles, document_from_input_blocks,
  document_to_loro, flowstate_document_theme,
};
use gpui_flowtext::{CellId, SemanticEditCommand as EditorSemanticCommand};
use loro::{ExportMode, LoroDoc};

use super::{CrdtRuntime, RuntimeEvent, editor_transaction_tests::assert_semantic_projection_eq};

const R1: RowId = RowId(1);
const R2: RowId = RowId(2);
const C1: ColumnId = ColumnId(1);
const C2: ColumnId = ColumnId(2);

fn input_paragraph(text: &str) -> InputParagraph {
  InputParagraph {
    style: ParagraphStyle::Normal,
    runs: if text.is_empty() {
      Vec::new()
    } else {
      vec![InputRun {
        text: text.to_string(),
        styles: RunStyles::default(),
      }]
    },
  }
}

fn input_cell(row_id: RowId, column_id: ColumnId, text: &str) -> InputTableCell {
  InputTableCell {
    id: CellId::from_coordinate(row_id, column_id),
    row_id,
    column_id,
    blocks: vec![InputTableCellBlock::Paragraph(input_paragraph(text))],
    row_span: 1,
    col_span: 1,
  }
}

fn input_row(row_id: RowId, cells: Vec<InputTableCell>) -> InputTableRow {
  InputTableRow { id: row_id, cells }
}

/// A 2x2 table with cells a/b over c/d and durable ids `row.1/row.2`,
/// `column.1/column.2`.
fn base_table() -> InputTableBlock {
  InputTableBlock {
    rows: vec![
      input_row(R1, vec![input_cell(R1, C1, "a"), input_cell(R1, C2, "b")]),
      input_row(R2, vec![input_cell(R2, C1, "c"), input_cell(R2, C2, "d")]),
    ],
    columns: vec![
      InputTableColumn {
        id: C1,
        width: InputTableColumnWidth::Auto,
      },
      InputTableColumn {
        id: C2,
        width: InputTableColumnWidth::Auto,
      },
    ],
    style: InputTableStyle { header_row: false },
  }
}

/// Two peers sharing the same base document: a leading paragraph then the 2x2
/// table (so the table block is at projected index 1).
fn seeded_table_peers(peer_count: usize) -> Result<Vec<CrdtRuntime>> {
  let source = document_from_input_blocks(
    flowstate_document_theme(),
    vec![InputBlock::Paragraph(input_paragraph("body")), InputBlock::Table(base_table())],
  );
  let doc = document_to_loro(&source, "Table convergence")?;
  let snapshot = doc.export(ExportMode::Snapshot)?;
  let mut peers = (0..peer_count)
    .map(|peer_ix| {
      let doc = LoroDoc::new();
      doc.set_peer_id(0x1000 + peer_ix as u64)?;
      let status = doc.import(&snapshot)?;
      assert!(status.pending.is_none(), "seed snapshot import must not leave pending dependencies");
      CrdtRuntime::from_doc(doc, None, None)
    })
    .collect::<Result<Vec<_>>>()?;
  // Settle the per-replica startup ops each peer committed in `from_doc` (replica
  // registration, style-mark repair) so every peer shares one frontier and each
  // projection's tagged frontier matches its live doc before edits are serialized.
  synchronize(&mut peers)?;
  Ok(peers)
}

fn local_update_bytes(events: &[RuntimeEvent]) -> Vec<Vec<u8>> {
  events
    .iter()
    .filter_map(|event| match event {
      RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
      _ => None,
    })
    .collect()
}

fn all_vv_equal(peers: &[CrdtRuntime]) -> bool {
  let first = peers[0].doc().state_vv();
  peers.iter().skip(1).all(|peer| peer.doc().state_vv() == first)
}

/// Anti-entropy exchange until the peers share a version vector. This also
/// carries any import-time repair updates (which `import_remote_update` commits
/// but does not surface as a `LocalUpdate`) because `export_updates_for` diffs
/// the actual canonical state.
fn synchronize(peers: &mut [CrdtRuntime]) -> Result<()> {
  for _ in 0..8 {
    if all_vv_equal(peers) {
      return Ok(());
    }
    for source_ix in 0..peers.len() {
      for target_ix in 0..peers.len() {
        if source_ix == target_ix {
          continue;
        }
        let update = peers[source_ix].export_updates_for(&peers[target_ix].doc().state_vv())?;
        if !update.is_empty() {
          peers[target_ix].import_remote_update(&update)?;
        }
      }
    }
  }
  anyhow::bail!("table peers failed to converge during direct synchronization")
}

fn table_block_id(projection: &DocumentProjection) -> BlockId {
  projection.ids.block_ids[1]
}

fn projected_table(projection: &DocumentProjection) -> &gpui_flowtext::TableBlock {
  match &projection.blocks[1] {
    flowstate_document::Block::Table(table) => table,
    other => panic!("expected a table at block index 1, got {other:?}"),
  }
}

fn cell_text(table: &gpui_flowtext::TableBlock, row_ix: usize, col_ix: usize) -> String {
  let cell = &table.rows[row_ix].cells[col_ix];
  cell
    .blocks
    .iter()
    .filter_map(|block| match block {
      gpui_flowtext::TableCellBlock::Paragraph(paragraph) => Some(paragraph.text.as_str()),
      gpui_flowtext::TableCellBlock::Table(_) => None,
    })
    .collect::<String>()
}

fn assert_converged(peers: &[CrdtRuntime], context: &str) -> Result<()> {
  for peer_ix in 1..peers.len() {
    assert_eq!(
      peers[0].doc().state_vv(),
      peers[peer_ix].doc().state_vv(),
      "version vector mismatch: peer 0 vs peer {peer_ix} ({context})"
    );
    let left = peers[0].projection_snapshot()?;
    let right = peers[peer_ix].projection_snapshot()?;
    assert_semantic_projection_eq(&left, &right, &format!("peer 0 vs peer {peer_ix} ({context})"));
  }
  // Materializer equivalence: the incrementally-maintained projection equals a
  // fresh full projection of the same canonical Loro state.
  for (peer_ix, peer) in peers.iter().enumerate() {
    let incremental = peer.projection_snapshot()?;
    let fresh = flowstate_document::document_from_loro(peer.doc())?;
    assert_semantic_projection_eq(&incremental, &fresh, &format!("peer {peer_ix} incremental vs fresh ({context})"));
  }
  Ok(())
}

#[test]
fn concurrent_row_inserts_after_same_anchor_both_survive() -> Result<()> {
  let mut peers = seeded_table_peers(2)?;
  let base = peers[0].projection_snapshot()?;
  let table = table_block_id(&base);

  // Both peers insert a row immediately after row.1, against the same base.
  let row_a = RowId(0x5000_0001);
  let row_b = RowId(0x5000_0002);
  let commit_a = peers[0].apply_editor_commands(
    1,
    &base.frontier,
    &[EditorSemanticCommand::InsertTableRow {
      table,
      new_row_id: row_a,
      after_row: Some(R1),
      row: input_row(row_a, vec![input_cell(row_a, C1, "A0"), input_cell(row_a, C2, "A1")]),
    }],
    None,
  )?;
  let commit_b = peers[1].apply_editor_commands(
    2,
    &base.frontier,
    &[EditorSemanticCommand::InsertTableRow {
      table,
      new_row_id: row_b,
      after_row: Some(R1),
      row: input_row(row_b, vec![input_cell(row_b, C1, "B0"), input_cell(row_b, C2, "B1")]),
    }],
    None,
  )?;
  for update in local_update_bytes(&commit_a.events) {
    peers[1].import_remote_update(&update)?;
  }
  for update in local_update_bytes(&commit_b.events) {
    peers[0].import_remote_update(&update)?;
  }
  synchronize(&mut peers)?;
  assert_converged(&peers, "concurrent row inserts")?;

  let projection = peers[0].projection_snapshot()?;
  let table_projection = projected_table(&projection);
  assert_eq!(table_projection.rows.len(), 4, "both inserted rows plus the two originals must survive");
  // The two originals are still present and unbroken.
  let texts: Vec<String> = (0..table_projection.rows.len())
    .map(|row_ix| format!("{}|{}", cell_text(table_projection, row_ix, 0), cell_text(table_projection, row_ix, 1)))
    .collect();
  assert!(texts.contains(&"a|b".to_string()), "original first row survives: {texts:?}");
  assert!(texts.contains(&"c|d".to_string()), "original second row survives: {texts:?}");
  assert!(texts.contains(&"A0|A1".to_string()), "peer A row survives: {texts:?}");
  assert!(texts.contains(&"B0|B1".to_string()), "peer B row survives: {texts:?}");
  Ok(())
}

#[test]
fn concurrent_distinct_cell_edits_are_independent() -> Result<()> {
  let mut peers = seeded_table_peers(2)?;
  let base = peers[0].projection_snapshot()?;
  let table = table_block_id(&base);

  // Peer 0 rewrites cell (row.1, column.1); peer 1 rewrites cell (row.2,
  // column.2). Different durable cells, so both edits must survive.
  let commit_a = peers[0].apply_editor_commands(
    1,
    &base.frontier,
    &[EditorSemanticCommand::ReplaceTableCell {
      table,
      row_id: R1,
      column_id: C1,
      cell: input_cell(R1, C1, "A-EDIT"),
    }],
    None,
  )?;
  let commit_b = peers[1].apply_editor_commands(
    2,
    &base.frontier,
    &[EditorSemanticCommand::ReplaceTableCell {
      table,
      row_id: R2,
      column_id: C2,
      cell: input_cell(R2, C2, "B-EDIT"),
    }],
    None,
  )?;
  for update in local_update_bytes(&commit_a.events) {
    peers[1].import_remote_update(&update)?;
  }
  for update in local_update_bytes(&commit_b.events) {
    peers[0].import_remote_update(&update)?;
  }
  synchronize(&mut peers)?;
  assert_converged(&peers, "concurrent distinct cell edits")?;

  let projection = peers[0].projection_snapshot()?;
  let table_projection = projected_table(&projection);
  assert_eq!(cell_text(table_projection, 0, 0), "A-EDIT", "peer 0's cell edit survives");
  assert_eq!(cell_text(table_projection, 1, 1), "B-EDIT", "peer 1's cell edit survives");
  assert_eq!(cell_text(table_projection, 0, 1), "b", "untouched cell unchanged");
  assert_eq!(cell_text(table_projection, 1, 0), "c", "untouched cell unchanged");
  Ok(())
}

#[test]
fn concurrent_add_row_and_add_column_repairs_cross_cell() -> Result<()> {
  let mut peers = seeded_table_peers(2)?;
  let base = peers[0].projection_snapshot()?;
  let table = table_block_id(&base);

  // Peer 0 adds a row; peer 1 adds a column. Neither creates the cell at the
  // (new row, new column) crossing — the topology-repair pass must synthesize
  // it deterministically so both peers read a full, identical grid (FS-010).
  let new_row = RowId(0x5000_00A0);
  let new_column = ColumnId(0x6000_00B0);
  let commit_row = peers[0].apply_editor_commands(
    1,
    &base.frontier,
    &[EditorSemanticCommand::InsertTableRow {
      table,
      new_row_id: new_row,
      after_row: Some(R2),
      row: input_row(new_row, vec![input_cell(new_row, C1, "r0"), input_cell(new_row, C2, "r1")]),
    }],
    None,
  )?;
  let commit_col = peers[1].apply_editor_commands(
    2,
    &base.frontier,
    &[EditorSemanticCommand::InsertTableColumn {
      table,
      new_column_id: new_column,
      after_column: Some(C2),
      width: InputTableColumnWidth::Auto,
      cells: vec![input_cell(R1, new_column, "x"), input_cell(R2, new_column, "y")],
    }],
    None,
  )?;
  for update in local_update_bytes(&commit_row.events) {
    peers[1].import_remote_update(&update)?;
  }
  for update in local_update_bytes(&commit_col.events) {
    peers[0].import_remote_update(&update)?;
  }
  synchronize(&mut peers)?;
  assert_converged(&peers, "add-row x add-column repair")?;

  let projection = peers[0].projection_snapshot()?;
  let table_projection = projected_table(&projection);
  assert_eq!(table_projection.rows.len(), 3, "two originals plus the inserted row");
  assert_eq!(table_projection.columns.len(), 3, "two originals plus the inserted column");
  // Every row is a full-width grid row — the cross cell was synthesized.
  for (row_ix, row) in table_projection.rows.iter().enumerate() {
    assert_eq!(row.cells.len(), 3, "row {row_ix} must be a full 3-column grid after repair");
  }
  // The synthesized cross cell (new row x new column) is a SINGLE empty
  // paragraph, not a `\n\n` doubled flow from two peers racing the repair — the
  // idempotent empty-flow repair path guarantees this.
  let cross_cell = &table_projection.rows[2].cells[2];
  assert_eq!(
    cross_cell.blocks.len(),
    1,
    "synthesized cross cell must be one empty paragraph, not a doubled flow (got {} blocks)",
    cross_cell.blocks.len()
  );
  assert_eq!(cell_text(table_projection, 2, 2), "", "synthesized cross cell must be empty");
  Ok(())
}
