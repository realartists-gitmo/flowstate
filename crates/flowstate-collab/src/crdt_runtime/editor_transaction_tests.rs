use std::{collections::HashSet, sync::Arc};

use anyhow::Result;
use flowstate_document::{AssetId, AssetRecord, BlockId, DocumentOffset, ParagraphId, ParagraphStyle, RunStyles, document_from_loro};
use gpui_flowtext::{EditorSelection, SemanticEditCommand as EditorSemanticCommand};

use super::{CrdtRuntime, RuntimeEvent};

fn projection_event_count(events: &[RuntimeEvent]) -> usize {
  events
    .iter()
    .filter(|event| {
      matches!(
        event,
        RuntimeEvent::ProjectionPatched { .. } | RuntimeEvent::ProjectionUpdated { .. } | RuntimeEvent::RevisionOpened { .. }
      )
    })
    .count()
}

pub(super) fn assert_semantic_projection_eq(left: &flowstate_document::DocumentProjection, right: &flowstate_document::DocumentProjection, context: &str) {
  assert_eq!(left.ids, right.ids, "identity mismatch: {context}");
  assert_eq!(left.sections, right.sections, "section mismatch: {context}");
  assert_eq!(left.frontier, right.frontier, "frontier mismatch: {context}");
  assert_eq!(left.paragraphs.len(), right.paragraphs.len(), "paragraph count mismatch: {context}");
  for paragraph_ix in 0..left.paragraphs.len() {
    let left_paragraph = &left.paragraphs[paragraph_ix];
    let right_paragraph = &right.paragraphs[paragraph_ix];
    assert_eq!(
      left_paragraph.style, right_paragraph.style,
      "paragraph style mismatch at {paragraph_ix}: {context}"
    );
    assert_eq!(
      left_paragraph.runs, right_paragraph.runs,
      "paragraph runs mismatch at {paragraph_ix}: {context}"
    );
    assert_eq!(
      flowstate_document::paragraph_text(left, paragraph_ix),
      flowstate_document::paragraph_text(right, paragraph_ix),
      "paragraph text mismatch at {paragraph_ix}: {context}",
    );
  }
  assert_eq!(left.blocks.len(), right.blocks.len(), "block count mismatch: {context}");
  for (block_ix, (left_block, right_block)) in left.blocks.iter().zip(right.blocks.iter()).enumerate() {
    match (left_block, right_block) {
      (flowstate_document::Block::Paragraph(_), flowstate_document::Block::Paragraph(_)) => {},
      _ => assert_eq!(left_block, right_block, "object block mismatch at {block_ix}: {context}"),
    }
  }
}

fn apply_projection_events(materialized: &mut flowstate_document::DocumentProjection, events: &[RuntimeEvent]) -> Result<()> {
  for event in events {
    match event {
      RuntimeEvent::ProjectionPatched { batch, .. } => {
        gpui_flowtext::apply_projection_patch_batch(materialized, batch)?;
      },
      RuntimeEvent::ProjectionUpdated { document, .. } | RuntimeEvent::RevisionOpened { document, .. } => {
        materialized.clone_from(document);
      },
      RuntimeEvent::LocalUpdate { .. }
      | RuntimeEvent::RemoteUpdateApplied { .. }
      | RuntimeEvent::RevisionForked { .. }
      | RuntimeEvent::SelectionRestored { .. } => {},
    }
  }
  Ok(())
}

#[test]
fn grouped_enter_text_batch_is_one_loro_change_and_preserves_client_ids() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Grouped editor transaction")?;
  runtime.doc().set_change_merge_interval(-1);
  let initial = runtime.projection_snapshot()?;
  let source_paragraph = initial.ids.paragraph_ids[0];
  let source_block = initial.ids.block_ids[0];
  let paragraph_ids = [ParagraphId(0x1001), ParagraphId(0x1002), ParagraphId(0x1003)];
  let block_ids = [BlockId(0x2001), BlockId(0x2002), BlockId(0x2003)];
  let commands = vec![
    EditorSemanticCommand::SplitParagraph {
      at: DocumentOffset { paragraph: 0, byte: 0 },
      source_paragraph,
      source_block,
      new_paragraph: paragraph_ids[0],
      new_block: block_ids[0],
      inherited_style: ParagraphStyle::Normal,
    },
    EditorSemanticCommand::InsertText {
      at: DocumentOffset { paragraph: 1, byte: 0 },
      text: "a".to_string(),
      styles: RunStyles::default(),
    },
    EditorSemanticCommand::SplitParagraph {
      at: DocumentOffset { paragraph: 1, byte: 1 },
      source_paragraph: paragraph_ids[0],
      source_block: block_ids[0],
      new_paragraph: paragraph_ids[1],
      new_block: block_ids[1],
      inherited_style: ParagraphStyle::Normal,
    },
    EditorSemanticCommand::InsertText {
      at: DocumentOffset { paragraph: 2, byte: 0 },
      text: "b".to_string(),
      styles: RunStyles::default(),
    },
    EditorSemanticCommand::SplitParagraph {
      at: DocumentOffset { paragraph: 2, byte: 1 },
      source_paragraph: paragraph_ids[1],
      source_block: block_ids[1],
      new_paragraph: paragraph_ids[2],
      new_block: block_ids[2],
      inherited_style: ParagraphStyle::Normal,
    },
    EditorSemanticCommand::InsertText {
      at: DocumentOffset { paragraph: 3, byte: 0 },
      text: "c".to_string(),
      styles: RunStyles::default(),
    },
  ];
  let selection = EditorSelection::collapsed(DocumentOffset { paragraph: 3, byte: 1 });
  let changes_before = runtime.doc().len_changes();
  let commit = runtime.apply_editor_commands(0xabc, &initial.frontier, &commands, Some(&selection))?;

  assert_eq!(runtime.doc().len_changes(), changes_before + 1);
  assert_eq!(commit.projection_event_count(), 1);
  assert_eq!(commit.transaction_id, 0xabc);
  let projection = runtime.projection_snapshot()?;
  assert_eq!(projection.paragraphs.len(), 4);
  assert_eq!(flowstate_document::paragraph_text(&projection, 0), "");
  assert_eq!(flowstate_document::paragraph_text(&projection, 1), "a");
  assert_eq!(flowstate_document::paragraph_text(&projection, 2), "b");
  assert_eq!(flowstate_document::paragraph_text(&projection, 3), "c");
  assert_eq!(&projection.ids.paragraph_ids[1..], &paragraph_ids);
  assert_eq!(&projection.ids.block_ids[1..], &block_ids);
  assert_eq!(projection.frontier, commit.new_frontier);

  let unique_paragraphs = projection
    .ids
    .paragraph_ids
    .iter()
    .copied()
    .collect::<HashSet<_>>();
  let unique_blocks = projection
    .ids
    .block_ids
    .iter()
    .copied()
    .collect::<HashSet<_>>();
  assert_eq!(unique_paragraphs.len(), projection.ids.paragraph_ids.len());
  assert_eq!(unique_blocks.len(), projection.ids.block_ids.len());

  let fresh = document_from_loro(runtime.doc())?;
  assert_semantic_projection_eq(&projection, &fresh, "grouped Enter/text batch");
  Ok(())
}

// KNOWN-FAILING repro (ignored) for the `incremental-vs-full-divergence` seen on the
// impact-defense doc. ROOT CAUSE (traced): a projection<->Loro coordinate mismatch from
// coalescing. `push_flow_blocks` coalesces an empty paragraph whose previous block is an
// object, so the projection is [before, image, after] (2 paras) even though the empty
// paragraph's boundary '\n' is still PHYSICALLY in the Loro body. `ProjectionRuntimeIndex`
// (crdt_runtime.rs ~182-189) computes `paragraph_body_unicode_starts` from the coalesced
// projection, so it runs SHORT of the real Loro body by one unicode per coalesced empty.
// Result: InsertText at the following paragraph's byte 0 resolves to a body-unicode that
// is one short, landing the char in the phantom empty slot -> document_from_loro then
// un-coalesces it (3 paras) while the incremental projection stays 2 -> preflight assert
// at crdt_runtime.rs ~1249 fires. FIX is a design choice (all convergence-critical):
//   (a) resolve projection->Loro offsets via each paragraph's durable Loro boundary
//       cursor instead of the projection-derived body-unicode index; OR
//   (b) coalesce PHANTOM empties only (a '\n' with no durable paragraph record), so a
//       real empty paragraph survives and the projection mirrors Loro; OR
//   (c) prune a coalesced empty paragraph's '\n'/records from Loro so it matches.
// Un-ignore once chosen+implemented; this then asserts the convergence invariant.
#[test]
fn object_adjacent_empty_paragraph_incremental_matches_full_rebuild() -> Result<()> {
  fn para(text: &str) -> flowstate_document::InputBlock {
    flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
      style: ParagraphStyle::Normal,
      runs: if text.is_empty() {
        Vec::new()
      } else {
        vec![flowstate_document::InputRun { text: text.to_string(), styles: RunStyles::default() }]
      },
    })
  }
  let source = flowstate_document::document_from_input_blocks(
    flowstate_document::flowstate_document_theme(),
    vec![
      para("before"),
      flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
        asset_id: AssetId(1),
        alt_text: "img".to_string(),
        caption: None,
        sizing: flowstate_document::InputImageSizing::Intrinsic,
        alignment: flowstate_document::InputBlockAlignment::Left,
      }),
      para(""), // empty paragraph immediately after the object
      para("after"),
    ],
  );
  let doc = flowstate_document::document_to_loro(&source, "Object empty paragraph")?;
  let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

  let at_rest = runtime.projection_snapshot()?;
  let fresh_at_rest = document_from_loro(runtime.doc())?;
  assert_semantic_projection_eq(&at_rest, &fresh_at_rest, "object+empty at rest");

  let target_paragraph = at_rest.paragraphs.len() - 1; // the "after" paragraph
  let command = EditorSemanticCommand::InsertText {
    at: DocumentOffset { paragraph: target_paragraph, byte: 0 },
    text: "X".to_string(),
    styles: RunStyles::default(),
  };
  let selection = EditorSelection::collapsed(DocumentOffset { paragraph: target_paragraph, byte: 1 });
  runtime.apply_editor_commands(0xabc, &at_rest.frontier, &[command], Some(&selection))?;

  let after_edit = runtime.projection_snapshot()?;
  let fresh_after_edit = document_from_loro(runtime.doc())?;
  assert_semantic_projection_eq(&after_edit, &fresh_after_edit, "object+empty after edit");
  Ok(())
}

#[test]
fn grouped_editor_preflight_rejects_wrong_stable_ids_without_mutation() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Invalid stable identity")?;
  let initial = runtime.projection_snapshot()?;
  let body_before = flowstate_document::loro_schema::body_text(runtime.doc()).to_string();
  let changes_before = runtime.doc().len_changes();
  let command = EditorSemanticCommand::SplitParagraph {
    at: DocumentOffset { paragraph: 0, byte: 0 },
    source_paragraph: ParagraphId(u128::MAX - 1),
    source_block: initial.ids.block_ids[0],
    new_paragraph: ParagraphId(0x3001),
    new_block: BlockId(0x4001),
    inherited_style: ParagraphStyle::Normal,
  };

  let error = runtime
    .apply_editor_commands(7, &initial.frontier, &[command], None)
    .expect_err("wrong stable source id must reject the whole batch");
  assert!(error.to_string().contains("stable-identity preflight"));
  assert_eq!(runtime.doc().len_changes(), changes_before);
  assert_eq!(flowstate_document::loro_schema::body_text(runtime.doc()).to_string(), body_before);
  assert_eq!(runtime.projection_snapshot()?.frontier, initial.frontier);
  Ok(())
}

#[test]
fn asset_byte_arrival_does_not_advance_canonical_frontier() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Asset bytes")?;
  let bytes = Arc::new(vec![1, 2, 3, 4]);
  let record = AssetRecord {
    id: AssetId(91),
    mime_type: "image/png".into(),
    original_name: Some("figure.png".into()),
    content_hash: AssetRecord::stable_content_hash(bytes.as_slice()),
    bytes: bytes.clone(),
  };
  runtime.merge_asset_records(vec![record.clone()])?;
  runtime
    .projection
    .assets
    .assets
    .get_mut(&record.id)
    .expect("asset should be cached")
    .bytes = Arc::new(Vec::new());

  let frontier_before = runtime.doc().state_frontiers().encode();
  let changes_before = runtime.doc().len_changes();
  let events = runtime.merge_asset_records(vec![record.clone()])?;
  assert!(events.is_empty());
  assert_eq!(runtime.doc().state_frontiers().encode(), frontier_before);
  assert_eq!(runtime.doc().len_changes(), changes_before);
  assert_eq!(
    runtime.projection.assets.assets[&record.id]
      .bytes
      .as_slice(),
    bytes.as_slice()
  );
  Ok(())
}

#[test]
fn editor_commands_and_asset_metadata_share_one_commit_and_projection_transition() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Combined transaction")?;
  runtime.doc().set_change_merge_interval(-1);
  let initial = runtime.projection_snapshot()?;
  let bytes = Arc::new(vec![9, 8, 7]);
  let record = AssetRecord {
    id: AssetId(123),
    mime_type: "application/octet-stream".into(),
    original_name: Some("payload.bin".into()),
    content_hash: AssetRecord::stable_content_hash(bytes.as_slice()),
    bytes,
  };
  let command = EditorSemanticCommand::InsertText {
    at: DocumentOffset { paragraph: 0, byte: 0 },
    text: "x".to_string(),
    styles: RunStyles::default(),
  };
  let changes_before = runtime.doc().len_changes();
  let commit = runtime.apply_editor_transaction(8, &initial.frontier, &[command], std::slice::from_ref(&record), None)?;

  assert_eq!(runtime.doc().len_changes(), changes_before + 1);
  assert_eq!(commit.projection_event_count(), 1);
  assert_eq!(flowstate_document::paragraph_text(&runtime.projection, 0), "x");
  assert_eq!(runtime.projection.assets.assets[&record.id], record);
  Ok(())
}

#[test]
fn emitted_projection_batches_match_fresh_loro_projection_deterministically() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Differential editor projection")?;
  let mut materialized = runtime.projection_snapshot()?;
  let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
  for step in 0..96_u128 {
    seed = seed
      .wrapping_mul(6364136223846793005)
      .wrapping_add(1442695040888963407);
    let projection = runtime.projection_snapshot()?;
    let paragraph_ix = (seed as usize) % projection.paragraphs.len();
    let text = flowstate_document::paragraph_text(&projection, paragraph_ix);
    let action = ((seed >> 32) % 5) as usize;
    let command = match action {
      0 => {
        let byte = (seed as usize) % (text.len() + 1);
        EditorSemanticCommand::InsertText {
          at: DocumentOffset {
            paragraph: paragraph_ix,
            byte,
          },
          text: char::from(b'a' + (step % 26) as u8).to_string(),
          styles: RunStyles::default(),
        }
      },
      1 if projection.paragraphs.len() < 16 => {
        let byte = (seed as usize) % (text.len() + 1);
        let block_ix = flowstate_document::block_ix_for_paragraph(&projection, paragraph_ix).expect("every paragraph must have a block");
        EditorSemanticCommand::SplitParagraph {
          at: DocumentOffset {
            paragraph: paragraph_ix,
            byte,
          },
          source_paragraph: projection.ids.paragraph_ids[paragraph_ix],
          source_block: projection.ids.block_ids[block_ix],
          new_paragraph: ParagraphId(0x5000 + step),
          new_block: BlockId(0x6000 + step),
          inherited_style: projection.paragraphs[paragraph_ix].style,
        }
      },
      2 if !text.is_empty() => {
        let byte = (seed as usize) % text.len();
        EditorSemanticCommand::DeleteRange {
          range: DocumentOffset {
            paragraph: paragraph_ix,
            byte,
          }..DocumentOffset {
            paragraph: paragraph_ix,
            byte: byte + 1,
          },
        }
      },
      3 if projection.paragraphs.len() > 1 && paragraph_ix + 1 < projection.paragraphs.len() => EditorSemanticCommand::JoinParagraphs {
        first: projection.ids.paragraph_ids[paragraph_ix],
        second: projection.ids.paragraph_ids[paragraph_ix + 1],
      },
      _ => EditorSemanticCommand::SetParagraphStyle {
        paragraph: projection.ids.paragraph_ids[paragraph_ix],
        style: if step % 2 == 0 {
          ParagraphStyle::Normal
        } else {
          ParagraphStyle::Custom(2)
        },
      },
    };
    let commit = runtime.apply_editor_commands(step + 1, &projection.frontier, &[command], None)?;
    assert_eq!(projection_event_count(&commit.events), 1);
    apply_projection_events(&mut materialized, &commit.events)?;

    let incremental = runtime.projection_snapshot()?;
    let fresh = document_from_loro(runtime.doc())?;
    assert_semantic_projection_eq(&materialized, &fresh, &format!("materialized projection after step {step}"));
    assert_semantic_projection_eq(&incremental, &fresh, &format!("runtime projection after step {step}"));
  }
  Ok(())
}
