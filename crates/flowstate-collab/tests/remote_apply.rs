#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use flowstate_collab::{
    SessionId,
    binding::DocBinding,
    local_apply::LocalApplier,
    patch_apply::apply_patches,
    projection, schema,
  };
  use gpui_flowtext::{
    Block, CanonicalOperation, CollabPatch, Document, DocumentOffset, DocumentTheme, InputBlock, InputEquationBlock, InputEquationDisplay,
    InputEquationSyntax, InputParagraph, ParagraphStyle, RunStyle, RunStyles, block_from_input_block, document_from_input_blocks,
    insert_block_id, insert_text_at, mutate_runs_in_range, paragraph_text, paragraphs_mut, plain, remove_block_ids, update_paragraph_block,
  };
  use loro::{ExportMode, LoroDoc, Subscription, event::Subscriber};

  const MULTIBYTE: &str = "aé🌍\u{2028}x";

  #[test]
  fn remote_text_insert_produces_paragraph_text_patch() {
    let (mut source, mut target) = pair(vec![paragraph_block(MULTIBYTE)]);
    let paragraph = source.document.ids.paragraph_ids[0];
    let styles = RunStyles::default().with(RunStyle::Semantic(3));

    insert_text_at(&mut source.document, 0, "a".len(), "Zé", styles);
    let updates = source.apply(&[CanonicalOperation::InsertText {
      paragraph,
      byte: "a".len(),
      text: "Zé".to_string(),
      styles,
    }]);
    let patches = target.import_updates(&updates);

    assert!(matches!(patches.as_slice(), [CollabPatch::ParagraphText { row: 0, .. }]));
    assert_documents_match(&source.document, &target.document, &target.loro);
  }

  #[test]
  fn remote_run_and_paragraph_styles_produce_style_patches() {
    let (mut source, mut target) = pair(vec![paragraph_block(MULTIBYTE)]);
    let paragraph = source.document.ids.paragraph_ids[0];

    paragraphs_mut(&mut source.document)[0].style = ParagraphStyle::Custom(7);
    update_paragraph_block(&mut source.document, 0);
    let paragraph_style_updates = source.apply(&[CanonicalOperation::SetParagraphStyle {
      paragraph,
      style: ParagraphStyle::Custom(7),
    }]);
    let paragraph_style_patches = target.import_updates(&paragraph_style_updates);
    assert!(matches!(paragraph_style_patches.as_slice(), [CollabPatch::ParagraphStyle { row: 0, style: ParagraphStyle::Custom(7) }]));

    let range = "a".len().."aé🌍".len();
    let styles = RunStyles::default()
      .with(RunStyle::Highlight(2))
      .with_direct_underline();
    mutate_runs_in_range(
      &mut source.document,
      DocumentOffset { paragraph: 0, byte: range.start }..DocumentOffset { paragraph: 0, byte: range.end },
      |run_styles| *run_styles = styles,
    );
    let run_style_updates = source.apply(&[CanonicalOperation::SetRunStyles { paragraph, range, styles }]);
    let run_style_patches = target.import_updates(&run_style_updates);
    assert!(matches!(run_style_patches.as_slice(), [CollabPatch::ParagraphRuns { row: 0, .. }]));
    assert_documents_match(&source.document, &target.document, &target.loro);
  }

  #[test]
  fn remote_object_data_rev_replacement_emits_one_patch() {
    let (mut source, mut target) = pair(vec![paragraph_block(MULTIBYTE), equation_block("x = 1")]);
    replace_equation(&mut source.document, 1, "x = aé + 🌍", 1);

    let updates = source.apply(&[CanonicalOperation::ReplaceBlock {
      block: Some(source.document.ids.block_ids[1]),
    }]);
    let patches = target.import_updates(&updates);

    assert!(matches!(patches.as_slice(), [CollabPatch::ReplaceObjectBlock { row: 1, .. }]));
    assert_documents_match(&source.document, &target.document, &target.loro);
  }

  #[test]
  fn remote_list_insert_delete_and_move_emit_structural_patches() {
    let (mut source, mut target) = pair(vec![paragraph_block(MULTIBYTE), paragraph_block("tail")]);

    insert_object_block(&mut source.document, 1, equation_block("insert aé"));
    let equation_id = source.document.ids.block_ids[1];
    let insert_updates = source.apply(&[CanonicalOperation::InsertBlock {
      block: equation_id,
      block_ix: 1,
    }]);
    let insert_patches = target.import_updates(&insert_updates);
    assert!(matches!(insert_patches.as_slice(), [CollabPatch::InsertBlocks { row: 1, blocks } ] if blocks.len() == 1));
    assert_documents_match(&source.document, &target.document, &target.loro);

    move_block(&mut source.document, 1, 2);
    let move_updates = source.apply(&[CanonicalOperation::MoveBlock {
      block: equation_id,
      new_block_ix: 2,
    }]);
    let move_patches = target.import_updates(&move_updates);
    assert!(move_patches.iter().any(|patch| matches!(patch, CollabPatch::MoveBlock { from: 1, to: 2 })));
    assert_documents_match(&source.document, &target.document, &target.loro);

    delete_object_block(&mut source.document, 2);
    let delete_updates = source.apply(&[CanonicalOperation::DeleteBlock { block: equation_id }]);
    let delete_patches = target.import_updates(&delete_updates);
    assert!(matches!(delete_patches.as_slice(), [CollabPatch::DeleteBlocks { row: 2, count: 1 }]));
    assert_documents_match(&source.document, &target.document, &target.loro);
  }

  struct SourcePeer {
    document: Document,
    loro: LoroDoc,
    binding: DocBinding,
    updates: Arc<Mutex<Vec<Vec<u8>>>>,
    _subscription: Subscription,
  }

  struct TargetPeer {
    document: Document,
    loro: LoroDoc,
    binding: DocBinding,
  }

  impl SourcePeer {
    fn apply(&mut self, ops: &[CanonicalOperation]) -> Vec<Vec<u8>> {
      LocalApplier {
        doc: &self.loro,
        binding: &mut self.binding,
      }
      .apply(&self.document, ops)
      .expect("local apply should succeed");
      let mut updates = self.updates.lock().expect("update lock should not be poisoned");
      std::mem::take(&mut *updates)
    }
  }

  impl TargetPeer {
    fn import_updates(&mut self, updates: &[Vec<u8>]) -> Vec<CollabPatch> {
      let mut all_patches = Vec::new();
      for update in updates {
        all_patches.extend(self.import_update(update));
      }
      all_patches
    }

    fn import_update(&mut self, update: &[u8]) -> Vec<CollabPatch> {
      let document = Arc::new(self.document.clone());
      let binding = Arc::new(Mutex::new(std::mem::take(&mut self.binding)));
      let patches = Arc::new(Mutex::new(Vec::new()));
      let error = Arc::new(Mutex::new(None::<String>));
      let doc = self.loro.clone();
      let callback: Subscriber = Arc::new({
        let binding = binding.clone();
        let patches = patches.clone();
        let error = error.clone();
        move |event| {
          let result = binding
            .lock()
            .map_err(|lock_error| anyhow::anyhow!("binding lock poisoned: {lock_error}"))
            .and_then(|mut binding| {
              flowstate_collab::remote_apply::RemoteApplier {
                doc: &doc,
                binding: &mut binding,
              }
              .apply_event(&document, &event)
            });
          match result {
            Ok(mut produced) => patches.lock().expect("patch lock should not be poisoned").append(&mut produced),
            Err(apply_error) => *error.lock().expect("error lock should not be poisoned") = Some(format!("{apply_error:#}")),
          }
        }
      });
      let subscription = self.loro.subscribe_root(callback);
      let import_result = self.loro.import_with(update, "remote");
      drop(subscription);
      self.binding = Arc::try_unwrap(binding)
        .expect("binding callback should be dropped")
        .into_inner()
        .expect("binding lock should not be poisoned");
      import_result.expect("remote import should succeed");
      let remote_error = error.lock().expect("error lock should not be poisoned").take();
      if let Some(error) = remote_error {
        panic!("remote apply failed: {error}");
      }
      let patches = Arc::try_unwrap(patches)
        .expect("patch callback should be dropped")
        .into_inner()
        .expect("patch lock should not be poisoned");
      apply_patches(&mut self.document, &mut self.binding, &self.loro, &patches).expect("patch apply should succeed");
      patches
    }
  }

  fn pair(blocks: Vec<InputBlock>) -> (SourcePeer, TargetPeer) {
    let session = SessionId::from_bytes([19; 32]);
    let document = document_from_input_blocks(DocumentTheme::default(), blocks);
    let loro = schema::new_configured_doc();
    projection::populate_from_document(&loro, session, "remote-apply", &document).expect("populate should succeed");
    let binding = DocBinding::build(&loro, &document).expect("binding should build");
    let updates = Arc::new(Mutex::new(Vec::new()));
    let captured = updates.clone();
    let subscription = loro.subscribe_local_update(Box::new(move |bytes| {
      captured.lock().expect("update lock should not be poisoned").push(bytes.clone());
      true
    }));
    let snapshot = loro.export(ExportMode::Snapshot).expect("snapshot export should succeed");

    let target_loro = schema::new_configured_doc();
    target_loro.import(&snapshot).expect("snapshot import should succeed");
    projection::verify_lineage(&target_loro, session).expect("lineage should match");
    let target_document = projection::document_from_loro(&target_loro, DocumentTheme::default()).expect("projection should succeed");
    let target_binding = DocBinding::build(&target_loro, &target_document).expect("binding should build");

    (
      SourcePeer {
        document,
        loro,
        binding,
        updates,
        _subscription: subscription,
      },
      TargetPeer {
        document: target_document,
        loro: target_loro,
        binding: target_binding,
      },
    )
  }

  fn paragraph_block(text: &str) -> InputBlock {
    InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain(text)],
    })
  }

  fn equation_block(source: &str) -> InputBlock {
    InputBlock::Equation(InputEquationBlock {
      source: source.to_string(),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })
  }

  fn insert_object_block(document: &mut Document, row: usize, block: InputBlock) {
    Arc::make_mut(&mut document.blocks).insert(row, block_from_input_block(&block));
    insert_block_id(document, row);
  }

  fn replace_equation(document: &mut Document, row: usize, source: &str, version: u64) {
    let mut block = block_from_input_block(&equation_block(source));
    if let Block::Equation(equation) = &mut block {
      equation.version = version;
    }
    Arc::make_mut(&mut document.blocks)[row] = block;
  }

  fn move_block(document: &mut Document, from: usize, to: usize) {
    let block = Arc::make_mut(&mut document.blocks).remove(from);
    let insert_ix = to.min(document.blocks.len());
    Arc::make_mut(&mut document.blocks).insert(insert_ix, block);
    let block_id = document.ids.block_ids.remove(from);
    document.ids.block_ids.insert(to.min(document.ids.block_ids.len()), block_id);
  }

  fn delete_object_block(document: &mut Document, row: usize) {
    Arc::make_mut(&mut document.blocks).remove(row);
    remove_block_ids(document, row..row + 1);
  }

  fn assert_documents_match(expected: &Document, actual: &Document, loro: &LoroDoc) {
    let projected = projection::document_from_loro(loro, DocumentTheme::default()).expect("projection should succeed");
    assert_document_shape_eq(expected, actual);
    assert_document_shape_eq(expected, &projected);
  }

  fn assert_document_shape_eq(left: &Document, right: &Document) {
    assert_eq!(left.blocks.len(), right.blocks.len());
    assert_eq!(left.paragraphs.len(), right.paragraphs.len());
    let mut paragraph_ix = 0;
    for (left_block, right_block) in left.blocks.iter().zip(right.blocks.iter()) {
      match (left_block, right_block) {
        (Block::Paragraph(left_paragraph), Block::Paragraph(right_paragraph)) => {
          assert_eq!(paragraph_text(left, paragraph_ix), paragraph_text(right, paragraph_ix));
          assert_eq!(left_paragraph.style, right_paragraph.style);
          assert_eq!(left_paragraph.runs, right_paragraph.runs);
        },
        (Block::Equation(left), Block::Equation(right)) => {
          assert_eq!(left.source, right.source);
          assert_eq!(left.syntax, right.syntax);
          assert_eq!(left.display, right.display);
        },
        (Block::Image(left), Block::Image(right)) => {
          assert_eq!(left.asset_id, right.asset_id);
          assert_eq!(left.alt_text, right.alt_text);
          assert_eq!(left.caption, right.caption);
          assert_eq!(left.sizing, right.sizing);
          assert_eq!(left.alignment, right.alignment);
        },
        (Block::Table(left), Block::Table(right)) => {
          assert_eq!(left.rows, right.rows);
          assert_eq!(left.column_widths, right.column_widths);
          assert_eq!(left.style, right.style);
        },
        _ => panic!("document block kind changed"),
      }
      if matches!(left_block, Block::Paragraph(_)) {
        paragraph_ix += 1;
      }
    }
  }

}
