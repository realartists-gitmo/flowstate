use std::sync::Arc;

use flowstate_collab::{ActorId, ReplicaId, Role};

use super::*;
use crate::{
  AssetStore, Block, BlockId, DocumentParagraphInput, DocumentRunInput, DocumentTheme, ParagraphId, ParagraphStyle, RichBlockIdentity,
  RunStyles, TableBlock, TableCell, TableCellBlock, TableCellBlockIdentity, TableCellParagraph, TableColumnWidth, TableRow, TableStyle,
  TextRun, document_from_paragraphs,
};

fn controller_with_full_materialization(document: &Document) -> Db8DocumentController {
  let actor_id = ActorId::new();
  let replica_id = ReplicaId::new();
  let serialized = crate::persistence::io::document_for_serialization(document);
  let seed = db8_flow_seed(&serialized).unwrap();
  let document_id = CollabDocumentId(uuid::Uuid::from_u128(serialized.ids.document_id));
  let source = FlowDocument::from_seed(document_id, actor_id, replica_id, &seed).unwrap();
  Db8DocumentController::from_source(source, serialized.assets.clone()).unwrap()
}

#[test]
fn rich_child_changes_use_incremental_projection_and_match_full_rebuild() {
  let mut document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "root".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let table_id = BlockId(uuid::Uuid::new_v4().as_u128());
  Arc::make_mut(&mut document.blocks).push(Block::Table(TableBlock {
    rows: vec![TableRow {
      cells: vec![TableCell {
        blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
          paragraph: crate::Paragraph {
            style: ParagraphStyle::Normal,
            byte_range: 0..4,
            runs: vec![TextRun {
              len: 4,
              styles: RunStyles::default(),
            }],
            version: 0,
          },
          text: "cell".to_string(),
        })],
        row_span: 1,
        col_span: 1,
      }],
    }],
    column_widths: vec![TableColumnWidth::Fraction(1)],
    style: TableStyle { header_row: false },
    version: 0,
  }));
  document.ids.block_ids.push(table_id);

  let mut controller = controller_with_full_materialization(&document);
  let RichBlockIdentity::Table(identity) = &controller.projection.ids.rich_block_ids[&table_id] else {
    panic!("table identity missing");
  };
  let first_row = identity.rows[0].id;
  let TableCellBlockIdentity::Paragraph(cell_paragraph) = identity.rows[0].cells[0].blocks[0] else {
    panic!("table-cell paragraph identity missing");
  };

  let before = controller.projection.clone();
  let edit = controller
    .flow_edit_for_intent(&Db8EditIntent::InsertText {
      at: Db8SourcePosition {
        paragraph_id: cell_paragraph,
        byte: 4,
      },
      text: "!".to_string(),
      styles: RunStyles::default(),
    })
    .unwrap();
  let commit = controller.source.apply_edits(Role::Owner, &[edit]).unwrap();
  let mut incremental = before.clone();
  let index = Db8ProjectionIndex::build(&incremental);
  let impact = patch_projection_incremental(&controller.source, &mut incremental, &index, &commit.changes).unwrap();
  let rebuilt = materialize_db8_flow_document(&controller.source, AssetStore::default()).unwrap();
  assert_projection_eq(&incremental, &rebuilt);
  assert_eq!(impact.replaced_blocks_before, 1..2);
  assert_eq!(impact.replacement_blocks_after, 1..2);
  controller.projection = incremental;

  let before = controller.projection.clone();
  let edit = controller
    .flow_edit_for_intent(&Db8EditIntent::InsertTableRow {
      table_id,
      after_row_id: Some(first_row),
      row_id: BlockId(uuid::Uuid::new_v4().as_u128()),
      cells: vec![(
        BlockId(uuid::Uuid::new_v4().as_u128()),
        ParagraphId(uuid::Uuid::new_v4().as_u128()),
      )],
    })
    .unwrap();
  let commit = controller.source.apply_edits(Role::Owner, &[edit]).unwrap();
  let mut incremental = before.clone();
  let index = Db8ProjectionIndex::build(&incremental);
  let impact = patch_projection_incremental(&controller.source, &mut incremental, &index, &commit.changes).unwrap();
  let rebuilt = materialize_db8_flow_document(&controller.source, AssetStore::default()).unwrap();
  assert_projection_eq(&incremental, &rebuilt);
  assert_eq!(impact.replaced_blocks_before, 1..2);
  assert_eq!(impact.replacement_blocks_after, 1..2);
}

fn assert_projection_eq(actual: &Document, expected: &Document) {
  assert_eq!(actual.text.to_string(), expected.text.to_string());
  assert_eq!(actual.paragraphs, expected.paragraphs);
  assert_eq!(actual.blocks, expected.blocks);
  assert_eq!(actual.ids.paragraph_ids, expected.ids.paragraph_ids);
  assert_eq!(actual.ids.block_ids, expected.ids.block_ids);
  assert_eq!(actual.ids.rich_block_ids, expected.ids.rich_block_ids);
}
