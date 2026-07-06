use std::{cell::RefCell, rc::Rc};

use gpui::AppContext as _;

struct SelectionEventRecorder {
  selections: Rc<RefCell<Vec<EditorSelection>>>,
  _subscription: gpui::Subscription,
}

#[gpui::test]
fn collab_capture_fast_path_emits_single_grapheme_deltas(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      assert!(editor.insert_single_grapheme_fast_path("a", cx));
      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      assert!(editor.insert_single_grapheme_fast_path("c", cx));
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 3);
  for (edit, (expected_text, expected_byte)) in edits.iter().zip([("a", 0), ("b", 1), ("c", 2)]) {
    let [SemanticEditCommand::InsertText { at, text, .. }] = edit.semantic_commands.as_slice() else {
      panic!("expected one semantic InsertText command, got {:?}", edit.semantic_commands);
    };
    assert_eq!(text, expected_text);
    assert_eq!(
      *at,
      DocumentOffset {
        paragraph: 0,
        byte: expected_byte,
      }
    );
  }

  let edits_after_undo = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
      editor.take_pending_session_edits()
    })
  });
  assert!(edits_after_undo.is_empty());

  let paste_edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      let fragment = RichClipboardFragment {
        format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
        paragraphs: vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("paste")],
        }],
        blocks: Vec::new(),
        assets: Vec::new(),
      };
      assert!(editor.insert_rich_fragment_paste_at_caret(&fragment, cx));
      editor.take_pending_session_edits()
    })
  });
  assert_eq!(paste_edits.len(), 1);
  assert!(matches!(
    paste_edits[0].semantic_commands.as_slice(),
    [SemanticEditCommand::ReplaceParagraphSpan { .. }]
  ));

  let edits_after_paste_undo = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
      editor.take_pending_session_edits()
    })
  });
  assert!(edits_after_paste_undo.is_empty());
}

#[gpui::test]
fn runtime_capture_fast_path_emits_single_grapheme_deltas(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      assert!(editor.insert_single_grapheme_fast_path("a", cx));
      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      assert!(editor.insert_single_grapheme_fast_path("c", cx));
      editor.take_pending_runtime_edits()
    })
  });

  assert_eq!(edits.len(), 3);
  for (edit, (expected_text, expected_byte)) in edits.iter().zip([("a", 0), ("b", 1), ("c", 2)]) {
    let [SemanticEditCommand::InsertText { at, text, .. }] = edit.semantic_commands.as_slice() else {
      panic!("expected one semantic InsertText command, got {:?}", edit.semantic_commands);
    };
    assert_eq!(text, expected_text);
    assert_eq!(
      *at,
      DocumentOffset {
        paragraph: 0,
        byte: expected_byte,
      }
    );
  }

  let edits_after_undo = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
      editor.take_pending_runtime_edits()
    })
  });
  assert!(edits_after_undo.is_empty());
}

#[gpui::test]
fn stale_runtime_caret_style_lookup_is_safe(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, _| {
      editor.selection = EditorSelection::collapsed(DocumentOffset { paragraph: 1, byte: 0 });
      assert_eq!(editor.styles_at_caret(), RunStyles::default());
    });
  });
}

#[gpui::test]
fn applying_collab_patches_does_not_arm_local_caret_scroll(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      assert!(!editor.pending_scroll_head_after_layout_for_test());
      let before_generation = editor.edit_generation();
      let block_id = editor.document().ids.block_ids[0];
      let paragraph_id = editor.document().ids.paragraph_ids[0];
      let base_frontier = editor.document().frontier.clone();
      editor
        .apply_projection_patch_batch(
          &ProjectionPatchBatch {
            transaction_id: 0,
            base_frontier,
            new_frontier: vec![1],
            patches: vec![ProjectionPatch::ParagraphText {
              block_id,
              paragraph_id,
              row_hint: 0,
              new: InputParagraph {
                style: ParagraphStyle::Normal,
                runs: vec![plain("remote")],
              },
              delta_utf8: vec![ProjectionTextDelta::Insert("remote".len())],
            }],
          },
          cx,
        )
        .expect("collab patch batch should apply cleanly");
      assert!(!editor.pending_scroll_head_after_layout_for_test());
      assert!(editor.edit_generation() > before_generation);
    });
  });
}

#[test]
fn projection_patch_batch_uses_stable_ids_over_row_hints() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("first")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("second")],
      },
    ],
  );
  document.frontier = vec![1];
  let block_id = document.ids.block_ids[1];
  let paragraph_id = document.ids.paragraph_ids[1];
  let batch = ProjectionPatchBatch {
    transaction_id: 7,
    base_frontier: document.frontier.clone(),
    new_frontier: vec![2],
    patches: vec![ProjectionPatch::ParagraphText {
      block_id,
      paragraph_id,
      row_hint: 0,
      new: InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("changed")],
      },
      delta_utf8: vec![ProjectionTextDelta::Delete("second".len()), ProjectionTextDelta::Insert("changed".len())],
    }],
  };

  apply_projection_patch_batch(&mut document, &batch).expect("stable ids should resolve stale row hints");

  assert_eq!(paragraph_text(&document, 0), "first");
  assert_eq!(paragraph_text(&document, 1), "changed");
  assert_eq!(document.frontier, vec![2]);
}

#[test]
fn projection_patch_batch_is_atomic_on_apply_error() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("before")],
    }],
  );
  document.frontier = vec![1];
  let original = document.clone();
  let batch = ProjectionPatchBatch {
    transaction_id: 8,
    base_frontier: document.frontier.clone(),
    new_frontier: vec![2],
    patches: vec![
      ProjectionPatch::ParagraphText {
        block_id: document.ids.block_ids[0],
        paragraph_id: document.ids.paragraph_ids[0],
        row_hint: 0,
        new: InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("after")],
        },
        delta_utf8: vec![ProjectionTextDelta::Delete("before".len()), ProjectionTextDelta::Insert("after".len())],
      },
      ProjectionPatch::DeleteBlocks {
        block_ids: vec![BlockId(u128::MAX)],
        row_hint: 0,
      },
    ],
  };

  let error = apply_projection_patch_batch(&mut document, &batch).expect_err("missing stable id should reject the full batch");

  assert!(matches!(error, ProjectionApplyError::MissingBlock { .. }));
  assert_eq!(paragraph_text(&document, 0), paragraph_text(&original, 0));
  assert_eq!(document.frontier, original.frontier);
}

#[test]
fn projection_patch_batch_moves_blocks_by_stable_anchor() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("first")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("second")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("third")],
      },
    ],
  );
  document.frontier = vec![1];
  let moved_block = document.ids.block_ids[2];
  let before_block = document.ids.block_ids[1];
  let batch = ProjectionPatchBatch {
    transaction_id: 9,
    base_frontier: document.frontier.clone(),
    new_frontier: vec![2],
    patches: vec![ProjectionPatch::MoveBlock {
      block_id: moved_block,
      before: Some(before_block),
      from_hint: 0,
      to_hint: 0,
    }],
  };

  apply_projection_patch_batch(&mut document, &batch).expect("stable ids should resolve stale move hints");

  assert_eq!(paragraph_text(&document, 0), "first");
  assert_eq!(paragraph_text(&document, 1), "third");
  assert_eq!(paragraph_text(&document, 2), "second");
  assert_eq!(document.ids.block_ids[1], moved_block);
  assert_eq!(document.ids.block_ids[2], before_block);
  assert_eq!(document.frontier, vec![2]);
}

#[test]
fn projection_patch_batch_inserts_paragraph_block_without_dropping_text() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("alpha")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("omega")],
      },
    ],
  );
  document.frontier = vec![1];
  let inserted_block = BlockId(0xabc);
  let inserted_paragraph = ParagraphId(0xdef);
  let before = document.ids.block_ids[1];
  let batch = ProjectionPatchBatch {
    transaction_id: 11,
    base_frontier: document.frontier.clone(),
    new_frontier: vec![2],
    patches: vec![ProjectionPatch::InsertBlocks {
      before: Some(before),
      row_hint: 1,
      blocks: vec![ProjectionStructuralBlock {
        block_id: inserted_block,
        paragraph_id: Some(inserted_paragraph),
        block: InputBlock::Paragraph(InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("inserted")],
        }),
      }],
    }],
  };

  apply_projection_patch_batch(&mut document, &batch).expect("paragraph block insert should apply");

  assert_eq!(paragraph_text(&document, 0), "alpha");
  assert_eq!(paragraph_text(&document, 1), "inserted");
  assert_eq!(paragraph_text(&document, 2), "omega");
  assert_eq!(document.ids.block_ids[1], inserted_block);
  assert_eq!(document.ids.paragraph_ids[1], inserted_paragraph);
  assert_eq!(document.frontier, vec![2]);
}

#[test]
fn deferred_section_rebuilds_wait_for_explicit_finalizer() {
  let mut theme = DocumentTheme::default();
  theme.set_custom_paragraph_style(
    0,
    CustomParagraphStyle {
      font_size: gpui::px(18.0),
      font_family: None,
      color: gpui::black(),
      bold: true,
      italic: false,
      underline: ThemeUnderline::None,
      align: CustomParagraphAlign::Left,
      spacing_before: gpui::px(0.0),
      spacing_after: gpui::px(0.0),
      border: None,
      section_kind: Some(0),
      section_level: Some(1),
    },
  );
  let mut document = document_from_input(
    theme,
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("Heading")],
    }],
  );
  assert!(document.outline.is_empty());

  let guard = defer_document_section_rebuilds();
  paragraphs_mut(&mut document)[0].style = ParagraphStyle::Custom(0);
  rebuild_document_sections(&mut document);
  assert!(deferred_document_section_rebuild_requested());
  assert!(document.outline.is_empty(), "deferred rebuild should not update outline eagerly");
  drop(guard);

  rebuild_document_sections_now(&mut document);
  assert!(!deferred_document_section_rebuild_requested());
  assert_eq!(document.outline.len(), 1);
  assert_eq!(document.outline[0].heading_paragraph, document.ids.paragraph_ids[0]);
}

#[test]
fn section_page_metadata_survives_outline_recompute_and_block_move() {
  let mut theme = DocumentTheme::default();
  theme.set_custom_paragraph_style(
    0,
    CustomParagraphStyle {
      font_size: gpui::px(18.0),
      font_family: None,
      color: gpui::black(),
      bold: true,
      italic: false,
      underline: ThemeUnderline::None,
      align: CustomParagraphAlign::Left,
      spacing_before: gpui::px(0.0),
      spacing_after: gpui::px(0.0),
      border: None,
      section_kind: Some(0),
      section_level: Some(1),
    },
  );
  let mut document = document_from_input(
    theme,
    vec![
      InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![plain("Heading")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("Body")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("Tail")],
      },
    ],
  );
  let page = SectionPageAttrs {
    page_size: SectionPageSize {
      width_twips: 10_080,
      height_twips: 12_960,
    },
    margins: SectionMargins {
      top_twips: 720,
      right_twips: 900,
      bottom_twips: 720,
      left_twips: 900,
    },
    columns: 2,
    orientation: SectionOrientation::Landscape,
    page_numbering: SectionPageNumbering {
      format: PageNumberFormat::UpperRoman,
      start: 7,
    },
    header_flow_id: Some("flow.header".to_string()),
    footer_flow_id: Some("flow.footer".to_string()),
  };
  let heading_paragraph = document.ids.paragraph_ids[0];
  std::sync::Arc::make_mut(&mut document.sections).push(DocumentSection {
    id: SectionId(1),
    parent_id: None,
    kind: SectionKind::Custom(0),
    heading_paragraph: Some(heading_paragraph),
    start_paragraph: heading_paragraph,
    end_paragraph_exclusive: None,
    page: Some(page.clone()),
  });

  rebuild_document_sections(&mut document);

  assert_eq!(document.sections[0].page, Some(page.clone()));
  assert_eq!(document.sections[0].heading_paragraph, Some(heading_paragraph));
  assert!(!document.outline.is_empty());

  document.frontier = vec![1];
  let moved_block = document.ids.block_ids[2];
  let before_block = document.ids.block_ids[1];
  let batch = ProjectionPatchBatch {
    transaction_id: 10,
    base_frontier: document.frontier.clone(),
    new_frontier: vec![2],
    patches: vec![ProjectionPatch::MoveBlock {
      block_id: moved_block,
      before: Some(before_block),
      from_hint: 2,
      to_hint: 1,
    }],
  };

  apply_projection_patch_batch(&mut document, &batch).expect("block move should preserve canonical section metadata");

  assert_eq!(document.sections[0].page, Some(page));
  assert_eq!(document.sections[0].heading_paragraph, Some(document.ids.paragraph_ids[0]));
  assert_eq!(paragraph_text(&document, 1), "Tail");
  assert_eq!(paragraph_text(&document, 2), "Body");
}

#[gpui::test]
fn applying_collab_asset_records_updates_asset_cache(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      let asset_id = AssetId(42);
      let bytes = vec![1, 2, 3, 4];
      let record = AssetRecord {
        id: asset_id,
        mime_type: "image/png".into(),
        original_name: Some("figure.png".into()),
        content_hash: AssetRecord::stable_content_hash(&bytes),
        bytes: std::sync::Arc::new(bytes),
      };
      let before_generation = editor.edit_generation();
      editor.apply_synced_asset_records(&[(asset_id, record.clone())], cx);

      assert_eq!(editor.document().assets.assets.get(&asset_id), Some(&record));
      // Asset bytes arrive out-of-band; caching them must NOT dirty the document
      // or advance the edit generation (FS-054).
      assert_eq!(
        editor.edit_generation(),
        before_generation,
        "asset availability must not mark the document changed",
      );
      assert!(!editor.pending_scroll_head_after_layout_for_test());
    });
  });
}

#[gpui::test]
fn object_insertion_emits_insert_block_semantic_command(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.insert_default_table(2, 2, cx);
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let [SemanticEditCommand::InsertBlock { block_ix, .. }] = edits[0].semantic_commands.as_slice() else {
    panic!("expected one semantic InsertBlock command, got {:?}", edits[0].semantic_commands);
  };
  assert_eq!(*block_ix, 0);
}

#[gpui::test]
fn paragraph_block_insertion_emits_paragraph_span_command(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.insert_toolkit_paragraphs_as_blocks(vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("inserted")],
      }], cx);
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let [SemanticEditCommand::ReplaceParagraphSpan { before, after, .. }] = edits[0].semantic_commands.as_slice() else {
    panic!(
      "expected one semantic ReplaceParagraphSpan command, got {:?}",
      edits[0].semantic_commands
    );
  };
  assert_eq!(before.paragraphs.len(), 1);
  assert_eq!(after.paragraphs.len(), 2);
  assert_eq!(after.text, "\ninserted");
}

#[gpui::test]
fn mixed_block_insertion_emits_paragraph_span_and_insert_block_commands(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.insert_block_fragment_for_test(
        RichClipboardFragment {
          format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
          paragraphs: Vec::new(),
          blocks: vec![
            InputBlock::Equation(InputEquationBlock {
              source: "x+1".to_string(),
              syntax: InputEquationSyntax::Latex,
              display: InputEquationDisplay::Display,
            }),
            InputBlock::Paragraph(InputParagraph {
              style: ParagraphStyle::Normal,
              runs: vec![plain("after")],
            }),
          ],
          assets: Vec::new(),
        },
        cx,
      );
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let commands = edits[0].semantic_commands.as_slice();
  assert_eq!(commands.len(), 2);
  assert!(matches!(commands[0], SemanticEditCommand::ReplaceParagraphSpan { .. }));
  assert!(matches!(commands[1], SemanticEditCommand::InsertBlock { block_ix: 1, .. }));
}

#[gpui::test]
fn block_insertion_over_text_selection_emits_structured_commands(cx: &mut gpui::TestAppContext) {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("alpha")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("omega")],
      },
    ],
  );
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 0, byte: 2 },
        DocumentOffset { paragraph: 1, byte: 2 },
        cx,
      );
      editor.insert_default_table(1, 1, cx);
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let commands = edits[0].semantic_commands.as_slice();
  assert_eq!(commands.len(), 2);
  assert!(matches!(commands[0], SemanticEditCommand::ReplaceParagraphSpan { .. }));
  assert!(matches!(commands[1], SemanticEditCommand::InsertBlock { .. }));
}

#[gpui::test]
fn table_column_width_edit_emits_structured_semantic_command(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.insert_default_table(2, 2, cx);
      editor.set_session_capture(true);
      editor.select_table_cell_for_test(0, 0, 1, cx);
      editor.widen_selected_table_column(cx);
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let [SemanticEditCommand::SetTableColumnWidth { column_ix, width, .. }] = edits[0].semantic_commands.as_slice() else {
    panic!(
      "expected one semantic SetTableColumnWidth command, got {:?}",
      edits[0].semantic_commands
    );
  };
  assert_eq!(*column_ix, 1);
  assert!(matches!(width, InputTableColumnWidth::FixedPx(144)));
}

#[gpui::test]
fn table_structure_edits_emit_structured_semantic_commands(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.insert_default_table(2, 2, cx);
      editor.set_session_capture(true);
      editor.select_table_cell_for_test(0, 0, 0, cx);

      // Durable ids of the original row 0 / column 0, captured before edits so we
      // can assert the id-addressed commands anchor against them (§P2b).
      let row0_id = editor.table_row_id_for_test(0, 0);
      let column0_id = editor.table_column_id_for_test(0, 0);

      editor.insert_row_after_selected_table(cx);
      let edits = editor.take_pending_session_edits();
      let [SemanticEditCommand::InsertTableRow {
        new_row_id,
        after_row,
        row,
        ..
      }] = edits[0].semantic_commands.as_slice() else {
        panic!("expected one semantic InsertTableRow command, got {:?}", edits[0].semantic_commands);
      };
      assert_eq!(*after_row, Some(row0_id));
      assert_eq!(row.id, *new_row_id);
      assert_eq!(row.cells.len(), 2);
      assert!(row.cells.iter().all(|cell| cell.row_id == *new_row_id));

      editor.insert_column_after_selected_table(cx);
      let edits = editor.take_pending_session_edits();
      let [SemanticEditCommand::InsertTableColumn {
        new_column_id,
        after_column,
        width,
        cells,
        ..
      }] = edits[0].semantic_commands.as_slice() else {
        panic!(
          "expected one semantic InsertTableColumn command, got {:?}",
          edits[0].semantic_commands
        );
      };
      assert_eq!(*after_column, Some(column0_id));
      assert!(matches!(width, InputTableColumnWidth::Fraction(1)));
      assert_eq!(cells.len(), 3);
      assert!(cells.iter().all(|cell| cell.column_id == *new_column_id));

      editor.delete_last_row_from_selected_table(cx);
      let edits = editor.take_pending_session_edits();
      let [SemanticEditCommand::DeleteTableRow { row_id, .. }] = edits[0].semantic_commands.as_slice() else {
        panic!("expected one semantic DeleteTableRow command, got {:?}", edits[0].semantic_commands);
      };
      assert_eq!(*row_id, row0_id);

      editor.delete_last_column_from_selected_table(cx);
      let edits = editor.take_pending_session_edits();
      let [SemanticEditCommand::DeleteTableColumn { column_id, .. }] = edits[0].semantic_commands.as_slice() else {
        panic!(
          "expected one semantic DeleteTableColumn command, got {:?}",
          edits[0].semantic_commands
        );
      };
      assert_eq!(*column_id, column0_id);
    });
  });
}

#[gpui::test]
fn table_cell_text_edit_emits_cell_scoped_semantic_command(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let (edits, expected_ids) = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.insert_default_table(2, 2, cx);
      editor.set_session_capture(true);
      editor.select_table_cell_for_test(0, 0, 0, cx);
      let expected_ids = editor.table_cell_ids_for_test(0, 0, 0);
      editor.insert_plain_text_from_toolkit("cell", cx);
      (editor.take_pending_session_edits(), expected_ids)
    })
  });

  assert_eq!(edits.len(), 1);
  let [SemanticEditCommand::ReplaceTableCell {
    row_id,
    column_id,
    cell,
    ..
  }] = edits[0].semantic_commands.as_slice() else {
    panic!("expected one semantic ReplaceTableCell command, got {:?}", edits[0].semantic_commands);
  };
  assert_eq!((*row_id, *column_id), expected_ids);
  let InputTableCellBlock::Paragraph(paragraph) = &cell.blocks[0] else {
    panic!("expected paragraph cell payload");
  };
  assert_eq!(input_paragraph_text(paragraph), "cell");
}

#[gpui::test]
fn own_collaboration_caret_color_can_be_toggled_off(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_own_collaboration_caret_color(Some(0x3b82f6), cx);
      assert_eq!(editor.local_caret_color_rgb(), Some(0x3b82f6));
      editor.set_show_own_collaboration_caret_color(false, cx);
      assert_eq!(editor.local_caret_color_rgb(), None);
      editor.set_show_own_collaboration_caret_color(true, cx);
      assert_eq!(editor.local_caret_color_rgb(), Some(0x3b82f6));
    });
  });
}

#[gpui::test]
fn text_entry_in_selected_equation_updates_equation_only(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Equation(EquationBlock {
      source: "x".into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    }),
  ]);
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  let (document, edits) = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.select_equation_block_for_test(1, cx);
      editor.insert_plain_text_from_toolkit("+1", cx);
      editor.replace_selected_text_from_platform_for_test("+2", cx);
      (editor.document().clone(), editor.take_pending_session_edits())
    })
  });

  assert_eq!(paragraph_text(&document, 0), "body");
  let Block::Equation(equation) = &document.blocks[1] else {
    panic!("expected equation block after toolkit text insert");
  };
  assert_eq!(equation.source.as_ref(), "x+1+2");
  assert_eq!(edits.len(), 2);
  let [SemanticEditCommand::ReplaceEquationSourceRange { range, text, .. }] = edits[0].semantic_commands.as_slice() else {
    panic!("expected first edit to replace an equation source range, got {:?}", edits[0].semantic_commands);
  };
  assert_eq!(range.clone(), 1..1);
  assert_eq!(text, "+1");
  let [SemanticEditCommand::ReplaceEquationSourceRange { range, text, .. }] = edits[1].semantic_commands.as_slice() else {
    panic!("expected second edit to replace an equation source range, got {:?}", edits[1].semantic_commands);
  };
  assert_eq!(range.clone(), 3..3);
  assert_eq!(text, "+2");
}

#[gpui::test]
fn image_alt_text_edit_emits_alt_text_semantic_command(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Image(ImageBlock {
      asset_id: AssetId(7),
      alt_text: "old".into(),
      caption: None,
      sizing: ImageSizing::Intrinsic,
      alignment: BlockAlignment::Left,
      version: 0,
    }),
  ]);
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  let (document, edits) = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.select_image_block_for_test(1, cx);
      editor.set_selected_image_alt_text("new alt", cx);
      (editor.document().clone(), editor.take_pending_session_edits())
    })
  });

  let Block::Image(image) = &document.blocks[1] else {
    panic!("expected image block after alt edit");
  };
  assert_eq!(image.alt_text.as_ref(), "new alt");
  assert_eq!(edits.len(), 1);
  let [SemanticEditCommand::ReplaceImageAltText { text, .. }] = edits[0].semantic_commands.as_slice() else {
    panic!("expected image alt semantic command, got {:?}", edits[0].semantic_commands);
  };
  assert_eq!(text, "new alt");
}

#[gpui::test]
fn image_layout_edit_emits_layout_semantic_command(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Image(ImageBlock {
      asset_id: AssetId(7),
      alt_text: "alt".into(),
      caption: None,
      sizing: ImageSizing::Intrinsic,
      alignment: BlockAlignment::Left,
      version: 0,
    }),
  ]);
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.select_image_block_for_test(1, cx);
      editor.set_selected_image_fit_width(cx);
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let [SemanticEditCommand::SetImageLayout { sizing, alignment, .. }] = edits[0].semantic_commands.as_slice() else {
    panic!("expected image layout semantic command, got {:?}", edits[0].semantic_commands);
  };
  assert!(matches!(sizing, InputImageSizing::FitWidth));
  assert!(matches!(alignment, InputBlockAlignment::Left));
}

#[gpui::test]
fn deleting_selection_across_object_emits_structured_semantic_commands(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("alpha")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("omega")],
      },
    ],
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Image(ImageBlock {
      asset_id: AssetId(7),
      alt_text: "alt".into(),
      caption: None,
      sizing: ImageSizing::Intrinsic,
      alignment: BlockAlignment::Left,
      version: 0,
    }),
    Block::Paragraph(document.paragraphs[1].clone()),
  ]);
  let first_block_id = document.ids.block_ids.first().copied().unwrap_or(BlockId(1));
  let second_block_id = document.ids.block_ids.get(1).copied().unwrap_or(BlockId(2));
  document.ids.block_ids = vec![first_block_id, BlockId(7), second_block_id];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 0, byte: 2 },
        DocumentOffset { paragraph: 1, byte: 2 },
        cx,
      );
      editor.cut(cx);
      editor.take_pending_session_edits()
    })
  });

  assert_eq!(edits.len(), 1);
  let commands = edits[0].semantic_commands.as_slice();
  assert_eq!(commands.len(), 2);
  assert!(matches!(commands[0], SemanticEditCommand::DeleteBlock { .. }));
  assert!(matches!(commands[1], SemanticEditCommand::ReplaceParagraphSpan { .. }));
}

#[gpui::test]
fn stale_selection_style_commands_do_not_panic(cx: &mut gpui::TestAppContext) {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha")],
    }],
  );
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 1, byte: 0 },
        DocumentOffset { paragraph: 1, byte: 0 },
        cx,
      );
      editor.toggle_semantic_style_for_selection(RunSemanticStyle::Custom(2), cx);
      editor.insert_plain_text_from_toolkit("z", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "alpha");
    });
  });
}

#[gpui::test]
fn select_all_emits_selection_changed_once(cx: &mut gpui::TestAppContext) {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta")],
    }],
  );
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));
  let selections = Rc::new(RefCell::new(Vec::new()));
  let recorder_selections = selections.clone();
  let _recorder = cx.update(|cx| {
    let editor = editor.clone();
    cx.new(|cx| SelectionEventRecorder {
      selections: recorder_selections,
      _subscription: cx.subscribe(&editor, |recorder: &mut SelectionEventRecorder, _, event: &EditorEvent, _| {
        if let EditorEvent::SelectionChanged { selection } = event {
          recorder.selections.borrow_mut().push(selection.clone());
        }
      }),
    })
  });

  cx.update(|cx| editor.update(cx, |editor, cx| editor.select_all(cx)));
  let first_events = selections.borrow();
  assert_eq!(first_events.len(), 1);
  assert_eq!(
    first_events[0].normalized(),
    DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
      paragraph: 0,
      byte: "alpha beta".len(),
    }
  );
  drop(first_events);

  cx.update(|cx| editor.update(cx, |editor, cx| editor.select_all(cx)));
  assert_eq!(selections.borrow().len(), 1);
}

#[gpui::test]
fn runtime_acknowledgement_preserves_newer_optimistic_input_and_rebases_it(cx: &mut gpui::TestAppContext) {
  let mut document = blank_document();
  document.frontier = vec![1, 2, 3];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      assert!(editor.insert_single_grapheme_fast_path("a", cx));
      let flushed = editor.take_pending_runtime_edits();
      assert_eq!(flushed.len(), 1);
      let transaction_id = flushed[0].transaction_id;
      let acknowledged_selection = flushed[0].selection_after.clone();
      let stable_acknowledged_selection = flushed[0].stable_selection_after.clone();
      editor.begin_runtime_transaction(transaction_id);

      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      assert_eq!(paragraph_text(editor.document(), 0), "ab");
      assert_eq!(editor.selection.head.byte, 2);

      let mut canonical = document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("a")],
        }],
      );
      canonical.frontier = vec![4, 5, 6];
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), acknowledged_selection.clone(), cx);
      editor
        .complete_runtime_transaction(transaction_id, vec![4, 5, 6], stable_acknowledged_selection, cx)
        .expect("canonical runtime projection must be materialized before completion");

      assert_eq!(paragraph_text(editor.document(), 0), "ab");
      assert_eq!(editor.selection.head.byte, 2);
      assert!(!editor.runtime_transaction_in_flight());
      let queued = editor.take_pending_runtime_edits();
      assert_eq!(queued.len(), 1);
      assert_eq!(queued[0].base_frontier, vec![4, 5, 6]);
      let [SemanticEditCommand::InsertText { at, text, .. }] = queued[0].semantic_commands.as_slice() else {
        panic!("expected one queued insert command");
      };
      assert_eq!(*at, DocumentOffset { paragraph: 0, byte: 1 });
      assert_eq!(text, "b");
    });
  });
}

#[gpui::test]
fn local_reconciliation_keeps_the_caret_when_canonical_reassigns_a_paragraph_id(cx: &mut gpui::TestAppContext) {
  // Regression guard for "typed text outruns the cursor": type into a line, press
  // Enter to land the caret at the start of a fresh empty paragraph, then
  // acknowledge a canonical projection that reassigned that empty paragraph's
  // durable id (exactly what the repair pipeline does for freshly created
  // boundaries). The caret must stay on the empty paragraph, never snapping back
  // to the end of the previous line.
  let mut document = blank_document();
  document.frontier = vec![1];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      assert!(editor.insert_single_grapheme_fast_path("a", cx));
      editor.insert_paragraph_break_command(cx);
      assert_eq!(editor.document().paragraphs.len(), 2);
      assert_eq!(paragraph_text(editor.document(), 1), "");
      assert_eq!(editor.selection().head, DocumentOffset { paragraph: 1, byte: 0 });

      let flushed = editor.take_pending_runtime_edits();
      assert!(!flushed.is_empty());
      let transaction_id = flushed
        .iter()
        .find(|edit| edit.transaction_id != 0)
        .map(|edit| edit.transaction_id)
        .unwrap_or(1);
      editor.begin_runtime_transaction(transaction_id);

      // Canonical projection for the acknowledged batch: identical text and
      // structure, but the empty paragraph's durable id was reassigned by a
      // repair while the first paragraph keeps its id.
      let mut canonical = editor.document().clone();
      canonical.frontier = vec![2];
      canonical.ids.paragraph_ids[1] = ParagraphId(canonical.ids.paragraph_ids[1].0 ^ 0x5EED_5EED);

      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), None, cx);
      editor
        .complete_runtime_transaction(transaction_id, vec![2], None, cx)
        .expect("acknowledged canonical projection must complete");

      assert!(!editor.runtime_transaction_in_flight());
      assert_eq!(paragraph_text(editor.document(), 1), "");
      assert_eq!(
        editor.selection().head,
        DocumentOffset { paragraph: 1, byte: 0 },
        "caret must stay on the fresh empty paragraph, not snap back behind the boundary",
      );
    });
  });
}

#[gpui::test]
fn explicit_selection_movement_during_runtime_commit_is_preserved(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("a")],
    }],
  );
  document.frontier = vec![1, 2, 3];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 0, byte: 1 },
        DocumentOffset { paragraph: 0, byte: 1 },
        cx,
      );
      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      let flushed = editor.take_pending_runtime_edits();
      let transaction_id = flushed[0].transaction_id;
      let acknowledged_selection = flushed[0].selection_after.clone();
      let stable_acknowledged_selection = flushed[0].stable_selection_after.clone();
      editor.begin_runtime_transaction(transaction_id);

      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 0, byte: 0 },
        DocumentOffset { paragraph: 0, byte: 0 },
        cx,
      );

      let mut canonical = document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("ab")],
        }],
      );
      canonical.frontier = vec![4, 5, 6];
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), acknowledged_selection.clone(), cx);
      editor
        .complete_runtime_transaction(transaction_id, vec![4, 5, 6], stable_acknowledged_selection, cx)
        .expect("canonical runtime projection must be materialized before completion");

      assert_eq!(paragraph_text(editor.document(), 0), "ab");
      assert_eq!(editor.selection.head, DocumentOffset { paragraph: 0, byte: 0 });
      assert!(!editor.runtime_transaction_in_flight());
    });
  });
}

#[gpui::test]
fn structural_runtime_acknowledgement_replays_newer_optimistic_input_and_rebases_it(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("a")],
    }],
  );
  document.frontier = vec![1, 2, 3];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 0, byte: 1 },
        DocumentOffset { paragraph: 0, byte: 1 },
        cx,
      );
      editor.insert_paragraph_break_command(cx);
      let flushed = editor.take_pending_runtime_edits();
      assert_eq!(flushed.len(), 1);
      assert!(matches!(
        flushed[0].semantic_commands.as_slice(),
        [SemanticEditCommand::SplitParagraph { .. }]
      ));
      let transaction_id = flushed[0].transaction_id;
      let acknowledged_selection = flushed[0].selection_after.clone();
      let stable_acknowledged_selection = flushed[0].stable_selection_after.clone();
      editor.begin_runtime_transaction(transaction_id);

      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      assert!(editor.insert_single_grapheme_fast_path("c", cx));
      assert!(editor.insert_single_grapheme_fast_path("d", cx));
      assert_eq!(paragraph_text(editor.document(), 0), "a");
      assert_eq!(paragraph_text(editor.document(), 1), "bcd");
      assert_eq!(editor.selection.head, DocumentOffset { paragraph: 1, byte: 3 });

      let mut canonical = document_from_input(
        DocumentTheme::default(),
        vec![
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![plain("a")],
          },
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: Vec::new(),
          },
        ],
      );
      canonical.frontier = vec![4, 5, 6];
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), acknowledged_selection.clone(), cx);
      editor
        .complete_runtime_transaction(transaction_id, vec![4, 5, 6], stable_acknowledged_selection, cx)
        .expect("canonical structural projection must be materialized before completion");

      assert_eq!(paragraph_text(editor.document(), 0), "a");
      assert_eq!(paragraph_text(editor.document(), 1), "bcd");
      assert_eq!(editor.selection.head, DocumentOffset { paragraph: 1, byte: 3 });
      assert!(!editor.runtime_transaction_in_flight());
      let queued = editor.take_pending_runtime_edits();
      assert_eq!(queued.len(), 3);
      for edit in &queued {
        assert_eq!(edit.base_frontier, vec![4, 5, 6]);
      }
      for (edit, (expected_text, expected_byte)) in queued.iter().zip([("b", 0), ("c", 1), ("d", 2)]) {
        let [SemanticEditCommand::InsertText { at, text, .. }] = edit.semantic_commands.as_slice() else {
          panic!("expected one queued insert command");
        };
        assert_eq!(*at, DocumentOffset { paragraph: 1, byte: expected_byte });
        assert_eq!(text, expected_text);
      }
    });
  });
}

#[gpui::test]
fn structural_acknowledgement_replays_newer_enter_and_text_at_the_latest_endpoint(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("a")],
    }],
  );
  document.frontier = vec![1, 2, 3];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      editor.set_text_selection_for_test(
        DocumentOffset { paragraph: 0, byte: 1 },
        DocumentOffset { paragraph: 0, byte: 1 },
        cx,
      );
      editor.insert_paragraph_break_command(cx);
      let flushed = editor.take_pending_runtime_edits();
      let [SemanticEditCommand::SplitParagraph {
        new_paragraph,
        new_block,
        ..
      }] = flushed[0].semantic_commands.as_slice()
      else {
        panic!("expected the first batch to contain one paragraph split");
      };
      let first_new_paragraph = *new_paragraph;
      let first_new_block = *new_block;
      let transaction_id = flushed[0].transaction_id;
      let acknowledged_selection = flushed[0].selection_after.clone();
      let stable_acknowledged_selection = flushed[0].stable_selection_after.clone();
      editor.begin_runtime_transaction(transaction_id);

      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      editor.insert_paragraph_break_command(cx);
      assert!(editor.insert_single_grapheme_fast_path("d", cx));
      assert_eq!(paragraph_text(editor.document(), 0), "a");
      assert_eq!(paragraph_text(editor.document(), 1), "b");
      assert_eq!(paragraph_text(editor.document(), 2), "d");
      assert_eq!(editor.selection.head, DocumentOffset { paragraph: 2, byte: 1 });

      let mut canonical = document_from_input(
        DocumentTheme::default(),
        vec![
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![plain("a")],
          },
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: Vec::new(),
          },
        ],
      );
      canonical.ids.paragraph_ids[1] = first_new_paragraph;
      canonical.ids.block_ids[1] = first_new_block;
      canonical.frontier = vec![4, 5, 6];
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), acknowledged_selection.clone(), cx);
      editor
        .complete_runtime_transaction(transaction_id, vec![4, 5, 6], stable_acknowledged_selection, cx)
        .expect("canonical structural projection must be materialized before completion");

      assert_eq!(paragraph_text(editor.document(), 0), "a");
      assert_eq!(paragraph_text(editor.document(), 1), "b");
      assert_eq!(paragraph_text(editor.document(), 2), "d");
      assert_eq!(editor.selection.head, DocumentOffset { paragraph: 2, byte: 1 });
      assert!(!editor.runtime_transaction_in_flight());

      let queued = editor.take_pending_runtime_edits();
      assert_eq!(queued.len(), 3);
      assert!(queued.iter().all(|edit| edit.base_frontier == vec![4, 5, 6]));
      assert!(matches!(queued[0].semantic_commands.as_slice(), [SemanticEditCommand::InsertText { text, .. }] if text == "b"));
      assert!(matches!(queued[1].semantic_commands.as_slice(), [SemanticEditCommand::SplitParagraph { .. }]));
      assert!(matches!(queued[2].semantic_commands.as_slice(), [SemanticEditCommand::InsertText { text, .. }] if text == "d"));
    });
  });
}

#[gpui::test]
fn runtime_acknowledgement_rejects_unexpected_transaction(cx: &mut gpui::TestAppContext) {
  let mut document = blank_document();
  document.frontier = vec![1, 2, 3];
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document.clone(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_runtime_capture(true);
      assert!(editor.insert_single_grapheme_fast_path("a", cx));
      let flushed = editor.take_pending_runtime_edits();
      let transaction_id = flushed[0].transaction_id;
      let stable_acknowledged_selection = flushed[0].stable_selection_after.clone();
      editor.begin_runtime_transaction(transaction_id);

      let mut canonical = document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("a")],
        }],
      );
      canonical.frontier = vec![4, 5, 6];
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), None, cx);

      let error = editor
        .complete_runtime_transaction(transaction_id + 1, vec![4, 5, 6], stable_acknowledged_selection, cx)
        .expect_err("acknowledgement for a different transaction must be rejected");
      assert!(matches!(error, ProjectionApplyError::UnexpectedTransaction { expected, actual } if expected == Some(transaction_id) && actual == transaction_id + 1));
      assert!(editor.runtime_transaction_in_flight());
    });
  });
}

#[gpui::test]
fn text_input_floors_stale_interior_utf8_caret_to_a_character_boundary(cx: &mut gpui::TestAppContext) {
  let document = document_from_input_blocks(
    DocumentTheme::default(),
    vec![InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("a’b")],
    })],
  );
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      // The curly apostrophe occupies bytes 1..4. Byte 2 is deliberately an
      // invalid stale caret inside that scalar value.
      editor.selection = EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: 2 });
      editor.insert_text_command("x", cx);

      assert_eq!(paragraph_text(editor.document(), 0), "ax’b");
      assert_eq!(editor.selection.head, DocumentOffset { paragraph: 0, byte: 2 });
      assert!(editor.document().text.is_char_boundary(global_byte(editor.document(), editor.selection.head)));
    });
  });
}
