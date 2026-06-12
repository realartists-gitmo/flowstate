use super::*;
use crate::{
  AuthoritativeEditController, AuthoritativeSourceEditRequest, AuthoritativeSourceOperation, AuthoritativeSourcePosition,
  AuthoritativeSourceSelection, DocumentTheme, InputParagraph, InputRun, document_from_input,
};

#[test]
fn controller_migrates_and_applies_source_first_text_split_join_and_undo() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "abcd".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let mut controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let paragraph = controller.projection().ids.paragraph_ids[0];
  let insert = controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 2,
        },
        text: "X".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  assert_eq!(insert.projection.replaced_blocks_before, 0..1);
  assert_eq!(insert.projection.replacement_blocks_after, 0..1);
  assert_eq!(insert.projection.affected_paragraphs_after, 0..1);
  assert_eq!(paragraph_text(controller.projection(), 0), "abXcd");
  let second = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let split = controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::SplitParagraph {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 3,
        },
        new_paragraph_id: second,
        style: ParagraphStyle::Normal,
      },
    )
    .unwrap();
  assert_eq!(split.projection.replaced_blocks_before, 0..1);
  assert_eq!(split.projection.replacement_blocks_after, 0..2);
  assert_eq!(paragraph_text(controller.projection(), 0), "abX");
  assert_eq!(paragraph_text(controller.projection(), 1), "cd");
  controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::JoinParagraph {
        second_paragraph_id: second,
      },
    )
    .unwrap();
  assert_eq!(paragraph_text(controller.projection(), 0), "abXcd");
  assert_eq!(controller.projection().ids.paragraph_ids[0], paragraph);
  controller.undo(Role::Owner).unwrap().unwrap();
  assert_eq!(controller.projection().paragraphs.len(), 2);
}

#[test]
fn controller_inserts_styled_multi_paragraph_fragment_atomically() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "tail".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let first = left.projection().ids.paragraph_ids[0];
  let second = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();
  let underlined = RunStyles {
    direct_underline: true,
    ..RunStyles::default()
  };

  let commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertParagraphFragment {
        at: Db8SourcePosition {
          paragraph_id: first,
          byte: 0,
        },
        paragraphs: vec![
          InputParagraph {
            style: ParagraphStyle::Custom(1),
            runs: vec![InputRun {
              text: "styled".to_string(),
              styles: underlined,
            }],
          },
          InputParagraph {
            style: ParagraphStyle::Custom(2),
            runs: vec![InputRun {
              text: "plain".to_string(),
              styles: RunStyles::default(),
            }],
          },
        ],
        new_paragraph_ids: vec![second],
      },
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();

  assert_eq!(paragraph_text(left.projection(), 0), "styled");
  assert_eq!(left.projection().paragraphs[0].style, ParagraphStyle::Normal);
  assert_eq!(left.projection().paragraphs[0].runs[0].styles, underlined);
  assert_eq!(paragraph_text(left.projection(), 1), "plaintail");
  assert_eq!(left.projection().paragraphs[1].style, ParagraphStyle::Custom(2));
  assert_eq!(left.projection().paragraphs[1].runs[0].styles, RunStyles::default());
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
  assert_eq!(left.projection().text, right.projection().text);
}

#[test]
fn source_request_inserts_styled_fragment_and_resolves_selection() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "tail".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let first = controller.projection().ids.paragraph_ids[0];
  let second = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let styles = RunStyles {
    direct_underline: true,
    ..RunStyles::default()
  };
  let mut authority = Db8EditorAuthority::new(controller, Role::Owner);

  let response = authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 0,
      },
      head: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 0,
      },
    },
    planned_selection: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: second,
        byte: "next".len(),
      },
      head: AuthoritativeSourcePosition {
        paragraph: second,
        byte: "next".len(),
      },
    },
    operations: vec![AuthoritativeSourceOperation::InsertParagraphFragment {
      at: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 0,
      },
      paragraphs: vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "styled".to_string(),
            styles,
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Custom(3),
          runs: vec![InputRun {
            text: "next".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
      new_paragraphs: vec![second],
    }],
  });

  assert!(response.error.is_none());
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "styled");
  assert_eq!(authority.controller().projection().paragraphs[0].runs[0].styles, styles);
  assert_eq!(paragraph_text(authority.controller().projection(), 1), "nexttail");
  assert_eq!(authority.controller().projection().paragraphs[1].style, ParagraphStyle::Custom(3));
  assert_eq!(
    response.projection.selection.unwrap().head,
    DocumentOffset {
      paragraph: 1,
      byte: "next".len(),
    }
  );
  assert_eq!(authority.drain_commits().count(), 1);
}

#[test]
fn randomized_multi_controller_root_projection_schedule_converges() {
  #[derive(Clone, Copy)]
  struct Rng(u64);

  impl Rng {
    fn next(&mut self) -> u64 {
      self.0 ^= self.0 << 13;
      self.0 ^= self.0 >> 7;
      self.0 ^= self.0 << 17;
      self.0
    }

    fn index(&mut self, len: usize) -> usize {
      (self.next() as usize) % len
    }
  }

  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "abcdef".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let seed = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let document_id = seed.source().document_id();
  let snapshot = seed.source().export_snapshot().unwrap();
  let mut controllers = (0..3)
    .map(|_| {
      Db8DocumentController::from_source(
        FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap(),
        AssetStore::default(),
      )
      .unwrap()
    })
    .collect::<Vec<_>>();
  let mut rng = Rng(0xbb67_ae85_84ca_a73b);
  let inserts = ["a", "β", "文", "🙂"];

  for round in 0..24 {
    let mut updates = Vec::new();
    for (controller_ix, controller) in controllers.iter_mut().enumerate() {
      let paragraph_ix = rng.index(controller.projection().paragraphs.len());
      let paragraph_id = controller.projection().ids.paragraph_ids[paragraph_ix];
      let text = paragraph_text(controller.projection(), paragraph_ix);
      let boundaries = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
      let boundary_ix = rng.index(boundaries.len());
      let byte = boundaries[boundary_ix];
      let commit = match rng.index(6) {
        0 => controller
          .apply_intent(
            Role::Editor,
            Db8EditIntent::InsertText {
              at: Db8SourcePosition { paragraph_id, byte },
              text: inserts[rng.index(inserts.len())].to_string(),
              styles: RunStyles::default(),
            },
          )
          .ok(),
        1 if boundaries.len() > 1 => {
          let start_ix = rng.index(boundaries.len() - 1);
          controller
            .apply_intent(
              Role::Editor,
              Db8EditIntent::DeleteText {
                start: Db8SourcePosition {
                  paragraph_id,
                  byte: boundaries[start_ix],
                },
                end: Db8SourcePosition {
                  paragraph_id,
                  byte: boundaries[start_ix + 1],
                },
              },
            )
            .ok()
        },
        2 => controller
          .apply_intent(
            Role::Editor,
            Db8EditIntent::SplitParagraph {
              at: Db8SourcePosition { paragraph_id, byte },
              new_paragraph_id: ParagraphId(uuid::Uuid::new_v4().as_u128()),
              style: ParagraphStyle::Custom((round % 4) as u8),
            },
          )
          .ok(),
        3 if paragraph_ix > 0 => controller
          .apply_intent(
            Role::Editor,
            Db8EditIntent::JoinParagraph {
              second_paragraph_id: paragraph_id,
            },
          )
          .ok(),
        4 => controller
          .apply_intent(
            Role::Editor,
            Db8EditIntent::SetParagraphStyle {
              paragraph_id,
              style: ParagraphStyle::Custom(((round + controller_ix) % 6) as u8),
            },
          )
          .ok(),
        5 => controller.undo(Role::Editor).ok().flatten(),
        _ => None,
      };
      if let Some(commit) = commit {
        let rebuilt = materialize_db8_flow_document(controller.source(), AssetStore::default()).unwrap();
        assert_eq!(
          controller.projection().text,
          rebuilt.text,
          "local incremental projection diverged at round {round} for controller {controller_ix}"
        );
        updates.push((controller_ix, controller.source().peer_id(), commit.source.update));
      }
    }
    while !updates.is_empty() {
      let update_ix = rng.index(updates.len());
      let (origin, peer_id, update) = updates.swap_remove(update_ix);
      for (controller_ix, controller) in controllers.iter_mut().enumerate() {
        if controller_ix != origin {
          controller
            .apply_remote_update(&update, &FlowImportPolicy::editor_from_peer(peer_id))
            .unwrap();
          let rebuilt = materialize_db8_flow_document(controller.source(), AssetStore::default()).unwrap();
          assert_eq!(
            controller.projection().text,
            rebuilt.text,
            "remote incremental projection diverged at round {round}, origin {origin}, target {controller_ix}"
          );
        }
      }
    }

    let expected = controllers[0].projection();
    for (controller_ix, controller) in controllers[1..].iter().enumerate() {
      assert_eq!(
        controller.projection().text,
        expected.text,
        "projection text diverged at round {round} for controller {}",
        controller_ix + 1
      );
      assert_eq!(controller.projection().paragraphs, expected.paragraphs);
      assert_eq!(controller.projection().ids.paragraph_ids, expected.ids.paragraph_ids);
      assert_eq!(controller.source().source_hash().unwrap(), controllers[0].source().source_hash().unwrap());
    }
  }

  let restarted = Db8DocumentController::from_source(
    FlowDocument::from_snapshot(
      &controllers[0].source().export_snapshot().unwrap(),
      Some(document_id),
      ReplicaId::new(),
    )
    .unwrap(),
    AssetStore::default(),
  )
  .unwrap();
  assert_eq!(restarted.projection().text, controllers[0].projection().text);
  assert_eq!(restarted.projection().paragraphs, controllers[0].projection().paragraphs);
}

#[test]
fn controller_remote_update_uses_exact_loro_update_and_converges() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "a".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let paragraph = left.projection().ids.paragraph_ids[0];
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();
  let commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 1,
        },
        text: "b".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();
  assert_eq!(paragraph_text(left.projection(), 0), "ab");
  assert_eq!(left.projection().text.to_string(), right.projection().text.to_string());
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
}

#[test]
fn editor_authority_replays_retained_local_lineage_after_snapshot_recovery() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "a".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let replica = ReplicaId::new();
  let mut origin = Db8DocumentController::from_document(&document, ActorId::new(), replica).unwrap();
  let document_id = origin.source().document_id();
  let peer_id = origin.source().peer_id();
  let snapshot = origin.source().export_snapshot().unwrap();
  let paragraph_id = origin.projection().ids.paragraph_ids[0];
  let first_commit = origin
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id,
          byte: 1,
        },
        text: "b".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  let second_commit = origin
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id,
          byte: 2,
        },
        text: "c".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  let mut recovered = Db8EditorAuthority::from_snapshot(&snapshot, document_id, replica, AssetStore::default(), Role::Owner).unwrap();

  recovered
    .replay_retained_updates(peer_id, &[first_commit.source.update, second_commit.source.update])
    .unwrap();

  assert_eq!(paragraph_text(recovered.controller().projection(), 0), "abc");
  assert_eq!(recovered.controller().source().source_hash().unwrap(), origin.source().source_hash().unwrap());
  let selection = AuthoritativeSourceSelection {
    anchor: AuthoritativeSourcePosition {
      paragraph: paragraph_id,
      byte: 3,
    },
    head: AuthoritativeSourcePosition {
      paragraph: paragraph_id,
      byte: 3,
    },
  };
  let _response = recovered.undo(selection);
  assert_eq!(paragraph_text(recovered.controller().projection(), 0), "abc");
  assert_eq!(recovered.drain_commits().count(), 0);
}

#[test]
fn migration_round_trips_rich_blocks_through_reachable_child_flows() {
  use crate::{
    EquationBlock, EquationDisplay, EquationSyntax, TableBlock, TableCell, TableCellBlock, TableCellParagraph, TableColumnWidth, TableRow,
    TableStyle,
  };

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
  let equation = Block::Equation(EquationBlock {
    source: "x^2 + y^2".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  });
  let table = Block::Table(TableBlock {
    rows: vec![TableRow {
      cells: vec![TableCell {
        blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
          paragraph: crate::Paragraph {
            style: ParagraphStyle::Normal,
            byte_range: 0.."cell".len(),
            runs: vec![TextRun {
              len: "cell".len(),
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
    style: TableStyle { header_row: true },
    version: 0,
  });
  Arc::make_mut(&mut document.blocks).extend([equation.clone(), table.clone()]);
  document.ids.block_ids.extend([
    BlockId(uuid::Uuid::new_v4().as_u128()),
    BlockId(uuid::Uuid::new_v4().as_u128()),
  ]);

  let controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  assert_eq!(controller.projection().blocks.as_slice(), document.blocks.as_slice());
  assert_eq!(controller.source().materialize().unwrap().flows.len(), 5);
}

#[test]
fn source_first_rich_block_insert_replicates_with_child_flow_identity() {
  use crate::{EquationBlock, EquationDisplay, EquationSyntax};

  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "root".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();
  let block_id = BlockId(uuid::Uuid::new_v4().as_u128());
  let equation = Block::Equation(EquationBlock {
    source: "x^2 + y^2".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  });
  let commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertBlock {
        block_id,
        block_ix: 1,
        block: equation.clone(),
      },
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();

  assert_eq!(left.projection().blocks[1], equation);
  assert_eq!(left.projection().ids.block_ids[1], block_id);
  assert_eq!(left.projection().blocks, right.projection().blocks);
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
}

#[test]
fn source_first_image_insert_records_content_addressed_reference_and_installs_bytes_separately() {
  use crate::{AssetId, AssetRecord, BlockAlignment, ImageBlock, ImageSizing};

  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "root".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();
  let asset = AssetRecord {
    id: AssetId(uuid::Uuid::new_v4().as_u128()),
    mime_type: "image/png".into(),
    original_name: Some("figure.png".into()),
    content_hash: 7,
    bytes: Arc::new(vec![1, 2, 3, 4]),
  };
  let image = Block::Image(ImageBlock {
    asset_id: asset.id,
    alt_text: "figure".into(),
    caption: None,
    sizing: ImageSizing::FitWidth,
    alignment: BlockAlignment::Center,
    version: 0,
  });
  let commit = left
    .apply_intents(
      Role::Owner,
      &[
        Db8EditIntent::RegisterAsset { asset: asset.clone() },
        Db8EditIntent::InsertBlock {
          block_id: BlockId(uuid::Uuid::new_v4().as_u128()),
          block_ix: 1,
          block: image.clone(),
        },
      ],
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();

  let reference = left
    .source()
    .asset_references()
    .unwrap()
    .remove(&FlowAssetId(uuid::Uuid::from_u128(asset.id.0)))
    .unwrap();
  assert_eq!(reference.blake3_hash, flowstate_collab::blake3_hash(&asset.bytes));
  assert_eq!(left.projection().blocks[1], image);
  assert!(left.projection().assets.assets.contains_key(&asset.id));
  assert!(!right.projection().assets.assets.contains_key(&asset.id));
  right
    .install_verified_asset_bytes(reference.blake3_hash, asset.bytes.as_ref().clone())
    .unwrap();
  assert!(right.projection().assets.assets.contains_key(&asset.id));
  let updated_image = ImageBlock {
    asset_id: asset.id,
    alt_text: "updated figure".into(),
    caption: None,
    sizing: ImageSizing::Fixed {
      width_px: 640,
      height_px: None,
    },
    alignment: BlockAlignment::Right,
    version: 0,
  };
  let commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::SetImageProperties {
        block_id: left.projection().ids.block_ids[1],
        image: updated_image.clone(),
      },
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();
  assert_eq!(left.projection().blocks[1], Block::Image(updated_image));
  assert_eq!(left.projection().blocks, right.projection().blocks);
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
}

#[test]
fn source_first_equation_edit_replicates_without_replacing_child_flow_identity() {
  use crate::{EquationBlock, EquationDisplay, EquationSyntax};

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
  let block_id = BlockId(uuid::Uuid::new_v4().as_u128());
  Arc::make_mut(&mut document.blocks).push(Block::Equation(EquationBlock {
    source: "x".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  }));
  document.ids.block_ids.push(block_id);

  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let before = left.source().materialize().unwrap();
  let equation_node_id = flow_node_id_from_block(block_id);
  let equation = before
    .flows[&before.root_flow_id]
    .nodes
    .iter()
    .find(|node| node.record().id == equation_node_id)
    .unwrap();
  let source_flow_id = equation.record().child_flows[0];
  let source_paragraph_id = before.flows[&source_flow_id].nodes[0].record().id;

  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();
  let commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::SetEquationSource {
        block_id,
        source: "x + y".into(),
      },
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();

  assert_eq!(
    left.projection().blocks[1],
    Block::Equation(EquationBlock {
      source: "x + y".into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    })
  );
  let after = left.source().materialize().unwrap();
  let equation = after
    .flows[&after.root_flow_id]
    .nodes
    .iter()
    .find(|node| node.record().id == equation_node_id)
    .unwrap();
  assert_eq!(equation.record().child_flows, vec![source_flow_id]);
  assert_eq!(after.flows[&source_flow_id].nodes[0].record().id, source_paragraph_id);
  assert_eq!(left.projection().blocks, right.projection().blocks);
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
}

#[test]
fn source_first_table_cell_text_replicates_with_stable_row_cell_and_paragraph_ids() {
  use crate::{TableBlock, TableCell, TableCellBlock, TableCellBlockIdentity, TableCellParagraph, TableColumnWidth, TableRow, TableStyle};

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
  let block_id = BlockId(uuid::Uuid::new_v4().as_u128());
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
  document.ids.block_ids.push(block_id);

  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let crate::RichBlockIdentity::Table(identity) = &left.projection().ids.rich_block_ids[&block_id] else {
    panic!("table identity missing");
  };
  let row_id = identity.rows[0].id;
  let cell_id = identity.rows[0].cells[0].id;
  let TableCellBlockIdentity::Paragraph(paragraph_id) = identity.rows[0].cells[0].blocks[0] else {
    panic!("table cell paragraph identity missing");
  };
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();

  let commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id,
          byte: 4,
        },
        text: "!".into(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();

  let Block::Table(table) = &left.projection().blocks[1] else {
    panic!("table projection missing");
  };
  let TableCellBlock::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("table cell paragraph projection missing");
  };
  assert_eq!(paragraph.text, "cell!");
  let crate::RichBlockIdentity::Table(identity) = &left.projection().ids.rich_block_ids[&block_id] else {
    panic!("table identity missing");
  };
  assert_eq!(identity.rows[0].id, row_id);
  assert_eq!(identity.rows[0].cells[0].id, cell_id);
  assert_eq!(identity.rows[0].cells[0].blocks[0], TableCellBlockIdentity::Paragraph(paragraph_id));
  assert_eq!(left.projection().blocks, right.projection().blocks);
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
}

#[test]
fn source_first_table_structure_mutations_are_atomic_and_convergent() {
  use crate::{TableBlock, TableCell, TableCellBlock, TableCellParagraph, TableColumnWidth, TableRow, TableStyle};

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
            byte_range: 0..0,
            runs: Vec::new(),
            version: 0,
          },
          text: String::new(),
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

  let actor = ActorId::new();
  let mut left = Db8DocumentController::from_document(&document, actor, ReplicaId::new()).unwrap();
  let crate::RichBlockIdentity::Table(identity) = &left.projection().ids.rich_block_ids[&table_id] else {
    panic!("table identity missing");
  };
  let first_row = identity.rows[0].id;
  let first_cell = identity.rows[0].cells[0].id;
  let new_row = BlockId(uuid::Uuid::new_v4().as_u128());
  let new_row_cell = BlockId(uuid::Uuid::new_v4().as_u128());
  let new_row_paragraph = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let new_column_cell = BlockId(uuid::Uuid::new_v4().as_u128());
  let new_column_paragraph = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();

  let commit = left
    .apply_intents(
      Role::Owner,
      &[
        Db8EditIntent::InsertTableRow {
          table_id,
          after_row_id: Some(first_row),
          row_id: new_row,
          cells: vec![(new_row_cell, new_row_paragraph)],
        },
        Db8EditIntent::InsertTableCell {
          row_id: first_row,
          after_cell_id: Some(first_cell),
          cell_id: new_column_cell,
          paragraph_id: new_column_paragraph,
        },
        Db8EditIntent::SetTableProperties {
          table_id,
          column_widths: vec![TableColumnWidth::Fraction(1), TableColumnWidth::Fraction(1)],
          style: TableStyle { header_row: true },
        },
      ],
    )
    .unwrap();
  right
    .apply_remote_update(&commit.source.update, &FlowImportPolicy::editor_from_peer(left.source().peer_id()))
    .unwrap();

  let Block::Table(table) = &left.projection().blocks[1] else {
    panic!("table projection missing");
  };
  assert_eq!(table.rows.len(), 2);
  assert_eq!(table.rows[0].cells.len(), 2);
  assert!(table.style.header_row);
  let crate::RichBlockIdentity::Table(identity) = &left.projection().ids.rich_block_ids[&table_id] else {
    panic!("table identity missing");
  };
  assert_eq!(identity.rows[0].id, first_row);
  assert_eq!(identity.rows[0].cells[0].id, first_cell);
  assert_eq!(identity.rows[0].cells[1].id, new_column_cell);
  assert_eq!(identity.rows[1].id, new_row);
  assert_eq!(identity.rows[1].cells[0].id, new_row_cell);
  assert_eq!(left.projection().blocks, right.projection().blocks);
  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
}

#[test]
fn concurrent_source_first_table_rows_merge_with_stable_cell_graphs() {
  use crate::{TableBlock, TableCell, TableCellBlock, TableCellParagraph, TableColumnWidth, TableRow, TableStyle};

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
            byte_range: 0..0,
            runs: Vec::new(),
            version: 0,
          },
          text: String::new(),
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

  let mut left = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let crate::RichBlockIdentity::Table(identity) = &left.projection().ids.rich_block_ids[&table_id] else {
    panic!("table identity missing");
  };
  let first_row = identity.rows[0].id;
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(left.source().document_id()), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();

  let left_row = BlockId(uuid::Uuid::new_v4().as_u128());
  let left_cell = BlockId(uuid::Uuid::new_v4().as_u128());
  let left_paragraph = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let right_row = BlockId(uuid::Uuid::new_v4().as_u128());
  let right_cell = BlockId(uuid::Uuid::new_v4().as_u128());
  let right_paragraph = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let left_commit = left
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertTableRow {
        table_id,
        after_row_id: Some(first_row),
        row_id: left_row,
        cells: vec![(left_cell, left_paragraph)],
      },
    )
    .unwrap();
  let right_commit = right
    .apply_intent(
      Role::Editor,
      Db8EditIntent::InsertTableRow {
        table_id,
        after_row_id: Some(first_row),
        row_id: right_row,
        cells: vec![(right_cell, right_paragraph)],
      },
    )
    .unwrap();

  left
    .apply_remote_update(
      &right_commit.source.update,
      &FlowImportPolicy::editor_from_peer(right.source().peer_id()),
    )
    .unwrap();
  right
    .apply_remote_update(
      &left_commit.source.update,
      &FlowImportPolicy::editor_from_peer(left.source().peer_id()),
    )
    .unwrap();

  assert_eq!(left.source().source_hash().unwrap(), right.source().source_hash().unwrap());
  assert_eq!(left.projection().blocks, right.projection().blocks);
  let crate::RichBlockIdentity::Table(identity) = &left.projection().ids.rich_block_ids[&table_id] else {
    panic!("table identity missing");
  };
  assert_eq!(identity.rows.len(), 3);
  let cells_for = |row_id| {
    identity
      .rows
      .iter()
      .find(|row| row.id == row_id)
      .unwrap()
      .cells
      .iter()
      .map(|cell| cell.id)
      .collect::<Vec<_>>()
  };
  assert_eq!(cells_for(left_row), vec![left_cell]);
  assert_eq!(cells_for(right_row), vec![right_cell]);
}

#[test]
fn anchored_selection_round_trips_and_tracks_text_across_split() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "abcd".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let mut controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let selection = EditorSelection {
    anchor: DocumentOffset { paragraph: 0, byte: 1 },
    head: DocumentOffset { paragraph: 0, byte: 3 },
  };
  let anchored = controller.anchor_selection(&selection).unwrap();
  let encoded = serialize_db8_anchored_selection(&anchored).unwrap();
  assert_eq!(parse_db8_anchored_selection(&encoded).unwrap(), anchored);

  controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::SplitParagraph {
        at: Db8SourcePosition {
          paragraph_id: controller.projection().ids.paragraph_ids[0],
          byte: 2,
        },
        new_paragraph_id: ParagraphId(uuid::Uuid::new_v4().as_u128()),
        style: ParagraphStyle::Normal,
      },
    )
    .unwrap();
  assert_eq!(
    controller.resolve_selection(&anchored).unwrap(),
    EditorSelection {
      anchor: DocumentOffset { paragraph: 0, byte: 1 },
      head: DocumentOffset { paragraph: 1, byte: 1 },
    }
  );
}

#[test]
fn opaque_source_selection_anchor_tracks_text_across_split() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "abcd".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let first = controller.projection().ids.paragraph_ids[0];
  let second = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let mut authority = Db8EditorAuthority::new(controller, Role::Owner);
  let anchor = authority
    .capture_source_selection_anchor(AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 3,
      },
      head: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 3,
      },
    })
    .unwrap()
    .unwrap();

  authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 3,
      },
      head: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 3,
      },
    },
    planned_selection: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 0,
      },
      head: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 0,
      },
    },
    operations: vec![AuthoritativeSourceOperation::SplitParagraph {
      at: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 2,
      },
      new_paragraph: second,
      style: ParagraphStyle::Normal,
    }],
  });

  assert_eq!(
    authority.resolve_source_selection_anchor(&anchor).unwrap().unwrap(),
    AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 1,
      },
      head: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 1,
      },
    }
  );
}

#[test]
fn stable_source_request_places_selection_after_split_and_cross_paragraph_delete() {
  let document = document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: "abcd".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let first = controller.projection().ids.paragraph_ids[0];
  let second = ParagraphId(uuid::Uuid::new_v4().as_u128());
  let mut authority = Db8EditorAuthority::new(controller, Role::Owner);
  let response = authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 2,
      },
      head: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 2,
      },
    },
    planned_selection: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 0,
      },
      head: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 0,
      },
    },
    operations: vec![AuthoritativeSourceOperation::SplitParagraph {
      at: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 2,
      },
      new_paragraph: second,
      style: ParagraphStyle::Custom(2),
    }],
  });
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 1, byte: 0 });
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "ab");
  assert_eq!(paragraph_text(authority.controller().projection(), 1), "cd");
  assert_eq!(authority.controller().projection().paragraphs[1].style, ParagraphStyle::Custom(2));

  let response = authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 2,
      },
      head: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 2,
      },
    },
    planned_selection: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 1,
      },
      head: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 1,
      },
    },
    operations: vec![AuthoritativeSourceOperation::DeleteText {
      start: AuthoritativeSourcePosition {
        paragraph: first,
        byte: 1,
      },
      end: AuthoritativeSourcePosition {
        paragraph: second,
        byte: 1,
      },
    }],
  });
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 1 });
  assert_eq!(authority.controller().projection().paragraphs.len(), 1);
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "ad");
}

#[test]
fn large_document_edit_with_patch_roundtrips() {
  let theme = DocumentTheme::default();
  let inputs = (0..500)
    .map(|i| InputParagraph {
      style: ParagraphStyle::Custom((i % 8) as u8),
      runs: vec![InputRun {
        text: format!("paragraph {i} with some text content for the large doc edit test"),
        styles: RunStyles::default(),
      }],
    })
    .collect();
  let document = document_from_input(theme, inputs);
  let controller = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let first = controller.projection().ids.paragraph_ids[0];
  let mut authority = Db8EditorAuthority::new(controller, Role::Owner);

  let response = authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition { paragraph: first, byte: 0 },
      head: AuthoritativeSourcePosition { paragraph: first, byte: 0 },
    },
    planned_selection: AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition { paragraph: first, byte: 10 },
      head: AuthoritativeSourcePosition { paragraph: first, byte: 10 },
    },
    operations: vec![AuthoritativeSourceOperation::InsertText {
      at: AuthoritativeSourcePosition { paragraph: first, byte: 0 },
      text: "EDIT ".to_string(),
      styles: RunStyles::default(),
    }],
  });
  assert!(response.error.is_none());
  assert!(authority.controller().projection().paragraphs.len() >= 500);
  let first_text = paragraph_text(authority.controller().projection(), 0);
  assert!(first_text.starts_with("EDIT "));
}

#[test]
fn patch_projection_in_place_rejects_unrepresentable_change_without_mutating_projection() {
  use flowstate_collab::{FlowChangeSummary, FlowId};
  let mut controller = Db8DocumentController::from_document(
    &document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "hello world".to_string(),
          styles: RunStyles::default(),
        }],
      }],
    ),
    ActorId::new(),
    ReplicaId::new(),
  )
  .unwrap();
  // A changeset referencing a non-existent flow is a projection correctness
  // failure. It must not silently trigger full materialization.
  let unknown_flow = FlowId(uuid::Uuid::nil());
  let unknown_changes = FlowChangeSummary {
    touched_flows: [unknown_flow].into(),
    touched_nodes: Default::default(),
    flow_text_changes: Default::default(),
  };
  // Split borrows: source() and projection are separate fields, but the
  // borrow checker cannot see through the method call. Use a raw pointer to
  // express that they do not alias — sound because patch_projection_in_place
  // takes `&FlowDocument` and `&mut Document` on non-overlapping memory.
  let source_ref: *const FlowDocument = controller.source();
  let projection_index = Db8ProjectionIndex::build(&controller.projection);
  let result = patch_projection_in_place(unsafe { &*source_ref }, &mut controller.projection, &projection_index, &unknown_changes);
  assert!(result.is_err(), "unrepresentable projection change must fail explicitly");
  assert_eq!(paragraph_text(&controller.projection, 0), "hello world");
}

#[test]
fn concurrent_independent_run_style_properties_merge() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "styled".to_string(),
        styles: RunStyles::default(),
      }],
    }],
  );
  let document_id = CollabDocumentId(uuid::Uuid::from_u128(document.ids.document_id));
  let mut left = Db8DocumentController::from_document(&document, ActorId::new(), ReplicaId::new()).unwrap();
  let snapshot = left.source().export_snapshot().unwrap();
  let mut right =
    Db8DocumentController::from_snapshot_with_projection(&snapshot, document_id, ReplicaId::new(), document.clone()).unwrap();
  let paragraph_id = document.ids.paragraph_ids[0];

  let underline = left
    .apply_intent(
      Role::Editor,
      Db8EditIntent::SetRunStyles {
        paragraph_id,
        range: 0.."styled".len(),
        patch: crate::RunStylePatch {
          direct_underline: Some(true),
          ..crate::RunStylePatch::default()
        },
      },
    )
    .unwrap();
  let highlight = right
    .apply_intent(
      Role::Editor,
      Db8EditIntent::SetRunStyles {
        paragraph_id,
        range: 0.."styled".len(),
        patch: crate::RunStylePatch {
          highlight: Some(Some(crate::HighlightStyle::Custom(2))),
          ..crate::RunStylePatch::default()
        },
      },
    )
    .unwrap();

  left
    .apply_remote_update(
      &highlight.source.update,
      &FlowImportPolicy::editor_from_peer(right.source().peer_id()),
    )
    .unwrap();
  right
    .apply_remote_update(
      &underline.source.update,
      &FlowImportPolicy::editor_from_peer(left.source().peer_id()),
    )
    .unwrap();

  for controller in [&left, &right] {
    let styles = controller.projection().paragraphs[0].runs[0].styles;
    assert!(styles.direct_underline);
    assert_eq!(styles.highlight, Some(crate::HighlightStyle::Custom(2)));
  }
}
