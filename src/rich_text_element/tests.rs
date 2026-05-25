use super::*;
use gpui::px;
use std::{
  collections::hash_map::DefaultHasher,
  hash::{Hash, Hasher},
};

#[test]
fn paragraph_edit_helpers_preserve_text_and_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![run("hello", RunStyles::default())],
    }],
  );

  insert_text_at(&mut document, 0, "he".len(), "y", RunStyles::default());
  assert_eq!(paragraph_text(&document, 0), "heyllo");
  assert_eq!(document.paragraphs[0].runs.len(), 1);

  apply_style_to_paragraph_range(&mut document, 0, "hey".len().."heyll".len(), RunStyle::Emphasis);
  assert_eq!(paragraph_text(&document, 0), "heyllo");
  assert_eq!(document.paragraphs[0].runs.len(), 3);
  assert_eq!(document.paragraphs[0].runs[1].styles, emphasized);

  delete_range_in_paragraph(&mut document, 0, "he".len().."heyll".len());
  assert_eq!(paragraph_text(&document, 0), "heo");
  assert_eq!(document.paragraphs[0].runs.len(), 1);
  assert_eq!(document.paragraphs[0].runs[0].styles, RunStyles::default());
}

#[test]
fn document_rope_edits_keep_utf8_byte_offsets() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![run("abé🚀cd", RunStyles::default())],
    }],
  );
  insert_text_at(&mut document, 0, "abé".len(), "Z", RunStyles::default());
  assert_eq!(paragraph_text(&document, 0), "abéZ🚀cd");

  let delete_start = "abé".len();
  let delete_end = "abéZ🚀".len();
  delete_range_in_paragraph(&mut document, 0, delete_start..delete_end);
  assert_eq!(paragraph_text(&document, 0), "abécd");
}

#[test]
fn smart_word_selection_is_enabled_by_default() {
  assert!(RichTextEditorConfig::default().smart_word_selection);
}

#[test]
fn smart_mouse_selection_snaps_across_words_but_not_inside_one_word() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta gamma")],
    }],
  );

  let smart = MouseSelectionOptions {
    smart_word_selection: true,
    exact: false,
  };
  let exact_fragment = expand_mouse_selection(
    &document,
    DocumentOffset { paragraph: 0, byte: 1 },
    DocumentOffset { paragraph: 0, byte: 4 },
    SelectionGranularity::Character,
    smart,
  )
  .normalized();
  assert_eq!(exact_fragment.start.byte, 1);
  assert_eq!(exact_fragment.end.byte, 4);

  let snapped = expand_mouse_selection(
    &document,
    DocumentOffset { paragraph: 0, byte: 2 },
    DocumentOffset {
      paragraph: 0,
      byte: "alpha be".len(),
    },
    SelectionGranularity::Character,
    smart,
  )
  .normalized();
  assert_eq!(snapped.start.byte, 0);
  assert_eq!(snapped.end.byte, "alpha beta".len());

  let after_first_word = expand_mouse_selection(
    &document,
    DocumentOffset { paragraph: 0, byte: 2 },
    DocumentOffset {
      paragraph: 0,
      byte: "alpha".len(),
    },
    SelectionGranularity::Character,
    smart,
  )
  .normalized();
  assert_eq!(after_first_word.start.byte, 0);
  assert_eq!(after_first_word.end.byte, "alpha".len());
}

#[test]
fn exact_mouse_selection_override_avoids_word_snapping() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta")],
    }],
  );

  let selection = expand_mouse_selection(
    &document,
    DocumentOffset { paragraph: 0, byte: 2 },
    DocumentOffset {
      paragraph: 0,
      byte: "alpha be".len(),
    },
    SelectionGranularity::Character,
    MouseSelectionOptions {
      smart_word_selection: true,
      exact: true,
    },
  )
  .normalized();

  assert_eq!(selection.start.byte, 2);
  assert_eq!(selection.end.byte, "alpha be".len());
}

#[test]
fn mouse_selection_can_disable_smart_word_snapping() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta")],
    }],
  );

  let selection = expand_mouse_selection(
    &document,
    DocumentOffset { paragraph: 0, byte: 2 },
    DocumentOffset {
      paragraph: 0,
      byte: "alpha be".len(),
    },
    SelectionGranularity::Character,
    MouseSelectionOptions {
      smart_word_selection: false,
      exact: false,
    },
  )
  .normalized();

  assert_eq!(selection.start.byte, 2);
  assert_eq!(selection.end.byte, "alpha be".len());
}

#[test]
fn single_paragraph_edits_refresh_following_cached_byte_ranges() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("first")],
      },
      InputParagraph {
        style: ParagraphStyle::Pocket,
        runs: vec![plain("second")],
      },
    ],
  );

  insert_text_at(&mut document, 0, "first".len(), " extended", RunStyles::default());

  assert_eq!(document_text_slice(&document, document.paragraphs[1].byte_range.clone()), "second");
  assert!(document.paragraphs[1].byte_range.end <= document.text.byte_len());
}

#[test]
fn db8_round_trip_preserves_text_structure_and_styles() {
  let document = demo_document();
  let dir = std::env::temp_dir();
  let path = dir.join(format!("flowstate-test-{}.db8", std::process::id()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(
    document_text_slice(&document, 0..document.text.byte_len()),
    document_text_slice(&loaded, 0..loaded.text.byte_len())
  );
  assert_eq!(document.paragraphs.len(), loaded.paragraphs.len());
  // Verify styles and run structure for every paragraph, not just the first.
  for (ix, (orig, loaded_para)) in document
    .paragraphs
    .iter()
    .zip(loaded.paragraphs.iter())
    .enumerate()
  {
    assert_eq!(orig.style, loaded_para.style, "paragraph {ix} style mismatch");
    assert_eq!(orig.runs, loaded_para.runs, "paragraph {ix} runs mismatch");
  }
}

#[test]
fn split_and_merge_preserve_empty_styled_paragraphs() {
  let spoken = RunStyles::default().with(RunStyle::HighlightSpoken);
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Pocket,
      runs: vec![run("Pocket", spoken)],
    }],
  );

  let first_len = paragraph_text_len(&document.paragraphs[0]);
  split_paragraph_at(&mut document, 0, first_len);
  assert_eq!(document.paragraphs.len(), 2);
  assert_eq!(document.paragraphs[1].style, ParagraphStyle::Pocket);
  assert_eq!(paragraph_text_len(&document.paragraphs[1]), 0);
  assert!(document.paragraphs[1].runs.is_empty());

  let join_byte = paragraph_text_len(&document.paragraphs[0]);
  delete_cross_paragraph_range(
    &mut document,
    DocumentOffset {
      paragraph: 0,
      byte: join_byte,
    }..DocumentOffset { paragraph: 1, byte: 0 },
  );
  assert_eq!(document.paragraphs.len(), 1);
  assert_eq!(paragraph_text(&document, 0), "Pocket");
  assert_eq!(
    document.paragraphs[0].runs,
    vec![TextRun {
      len: "Pocket".len(),
      styles: spoken
    }]
  );
}

#[test]
fn db8_round_trip_preserves_empty_styled_paragraphs() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Pocket,
        runs: Vec::new(),
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("body")],
      },
    ],
  );
  let path = std::env::temp_dir().join(format!("flowstate-empty-{}.db8", std::process::id()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(loaded.paragraphs[0].style, ParagraphStyle::Pocket);
  assert_eq!(paragraph_text_len(&loaded.paragraphs[0]), 0);
  assert!(loaded.paragraphs[0].runs.is_empty());
  assert_eq!(paragraph_text(&loaded, 1), "body");
}

#[test]
fn db8_v4_round_trip_preserves_mixed_block_order_and_assets() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Pocket,
        runs: vec![plain("Heading")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("After image")],
      },
    ],
  );
  let asset_id = AssetId(42);
  let asset_bytes = vec![1, 2, 3, 4];
  let mut hasher = DefaultHasher::new();
  asset_bytes.hash(&mut hasher);
  document.assets.assets.insert(
    asset_id,
    AssetRecord {
      id: asset_id,
      mime_type: "image/png".into(),
      original_name: Some("figure.png".into()),
      content_hash: hasher.finish(),
      bytes: std::sync::Arc::new(asset_bytes),
    },
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Image(ImageBlock {
      asset_id,
      alt_text: "figure".into(),
      caption: None,
      sizing: ImageSizing::FitWidth,
      alignment: BlockAlignment::Center,
      version: 0,
    }),
    Block::Paragraph(document.paragraphs[1].clone()),
    Block::Equation(EquationBlock {
      source: "x^2 + y^2 = z^2".into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    }),
  ]);

  let path = std::env::temp_dir().join(format!("flowstate-blocks-{}.db8", uuid::Uuid::new_v4()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(loaded.blocks.len(), 4);
  assert!(matches!(loaded.blocks[0], Block::Paragraph(_)));
  assert!(matches!(loaded.blocks[1], Block::Image(_)));
  assert!(matches!(loaded.blocks[2], Block::Paragraph(_)));
  assert!(matches!(loaded.blocks[3], Block::Equation(_)));
  assert_eq!(loaded.assets.assets[&asset_id].bytes.as_slice(), &[1, 2, 3, 4]);
}

#[test]
fn image_fit_width_layout_uses_asset_aspect_ratio() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let asset_id = AssetId(7);
  document.assets.assets.insert(
    asset_id,
    AssetRecord {
      id: asset_id,
      mime_type: "image/png".into(),
      original_name: None,
      content_hash: 0,
      bytes: std::sync::Arc::new(test_png_2x1()),
    },
  );
  let image = ImageBlock {
    asset_id,
    alt_text: "".into(),
    caption: None,
    sizing: ImageSizing::FitWidth,
    alignment: BlockAlignment::Left,
    version: 0,
  };

  let width = document.theme.pageless_inset_x * 2.0 + px(200.0);
  assert_eq!(image_layout_height_for_test(&document, &image, width), px(100.0));
}

#[test]
fn paragraph_sync_preserves_non_text_blocks_when_paragraphs_are_removed() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("before")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("after")],
      },
    ],
  );
  let image = Block::Image(ImageBlock {
    asset_id: AssetId(99),
    alt_text: "image".into(),
    caption: None,
    sizing: ImageSizing::FitWidth,
    alignment: BlockAlignment::Center,
    version: 0,
  });
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    image.clone(),
    Block::Paragraph(document.paragraphs[1].clone()),
  ]);

  let current = capture_document_span(&document, 0..2);
  apply_document_span_replacement(
    &mut document,
    &current,
    &DocumentSpan {
      start_paragraph: 0,
      text: "after".to_string(),
      paragraphs: vec![Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0.."after".len(),
        runs: vec![TextRun {
          len: "after".len(),
          styles: RunStyles::default(),
        }],
        version: 0,
      }],
    },
  );

  assert_eq!(document.paragraphs.len(), 1);
  assert!(
    document
      .blocks
      .iter()
      .any(|block| matches!(block, Block::Image(_)))
  );
}

#[test]
fn deleting_empty_paragraph_above_image_keeps_image_before_next_paragraph() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("before")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("after")],
      },
    ],
  );
  let image = Block::Image(ImageBlock {
    asset_id: AssetId(100),
    alt_text: "image".into(),
    caption: None,
    sizing: ImageSizing::FitWidth,
    alignment: BlockAlignment::Center,
    version: 0,
  });
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Paragraph(document.paragraphs[1].clone()),
    image,
    Block::Paragraph(document.paragraphs[2].clone()),
  ]);

  delete_cross_paragraph_range(
    &mut document,
    DocumentOffset {
      paragraph: 0,
      byte: "before".len(),
    }..DocumentOffset { paragraph: 1, byte: 0 },
  );

  assert_eq!(document.paragraphs.len(), 2);
  assert!(matches!(document.blocks[0], Block::Paragraph(_)));
  assert!(matches!(document.blocks[1], Block::Image(_)));
  assert!(matches!(document.blocks[2], Block::Paragraph(_)));
}

fn test_png_2x1() -> Vec<u8> {
  vec![
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0, 244, 34, 127, 138, 0, 0, 0, 12, 73, 68,
    65, 84, 8, 29, 99, 248, 15, 4, 0, 9, 251, 3, 253, 167, 170, 43, 113, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
  ]
}

#[test]
fn db8_v4_round_trip_preserves_table_cell_paragraph_and_run_styles() {
  let emphasized = RunStyles::default()
    .with(RunStyle::Emphasis)
    .with(RunStyle::HighlightSpoken);
  let cell_paragraph = Paragraph {
    style: ParagraphStyle::Tag,
    byte_range: 0.."cell".len(),
    runs: vec![TextRun {
      len: "cell".len(),
      styles: emphasized,
    }],
    version: 0,
  };
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("before")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Table(TableBlock {
      rows: vec![TableRow {
        cells: vec![TableCell {
          blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
            paragraph: cell_paragraph.clone(),
            text: "cell".to_string(),
          })],
          row_span: 1,
          col_span: 1,
        }],
      }],
      column_widths: vec![TableColumnWidth::Fraction(1)],
      style: TableStyle { header_row: true },
      version: 0,
    }),
  ]);

  let path = std::env::temp_dir().join(format!("flowstate-table-{}.db8", uuid::Uuid::new_v4()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  let Block::Table(table) = &loaded.blocks[1] else {
    panic!("expected table block");
  };
  assert!(table.style.header_row);
  let TableCellBlock::Paragraph(loaded_paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("expected table-cell paragraph");
  };
  assert_eq!(loaded_paragraph.paragraph.style, ParagraphStyle::Tag);
  assert_eq!(loaded_paragraph.paragraph.runs, cell_paragraph.runs);
  assert_eq!(loaded_paragraph.text, "cell");
}

#[test]
fn block_delete_operation_undo_redo_preserves_non_text_block() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let equation = Block::Equation(EquationBlock {
    source: "a^2+b^2=c^2".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  });
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), equation.clone()]);

  let op = EditOperation::DeleteBlock {
    block_ix: 1,
    block: equation,
  };
  op.redo(&mut document);
  assert_eq!(document.blocks.len(), 1);
  op.undo(&mut document);
  assert_eq!(document.blocks.len(), 2);
  assert!(matches!(document.blocks[1], Block::Equation(_)));
}

#[test]
fn insert_blocks_operation_undo_redo_preserves_inserted_table_and_equation() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let blocks = vec![
    Block::Table(TableBlock {
      rows: vec![TableRow {
        cells: vec![TableCell {
          blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
            paragraph: Paragraph {
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
    }),
    Block::Equation(EquationBlock {
      source: "x=1".into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    }),
  ];

  let op = EditOperation::InsertBlocks {
    block_ix: 1,
    blocks: blocks.clone(),
  };
  op.redo(&mut document);
  assert_eq!(document.blocks.len(), 3);
  assert!(matches!(document.blocks[1], Block::Table(_)));
  assert!(matches!(document.blocks[2], Block::Equation(_)));
  op.undo(&mut document);
  assert_eq!(document.blocks.len(), 1);
  op.redo(&mut document);
  assert_eq!(document.blocks.len(), 3);
}

#[test]
fn replace_block_operation_undo_redo_preserves_table_shape_changes() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let before = Block::Table(TableBlock {
    rows: vec![TableRow {
      cells: vec![TableCell {
        blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
          paragraph: Paragraph {
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
  });
  let mut after = before.clone();
  let Block::Table(table) = &mut after else {
    unreachable!();
  };
  table.rows.push(table.rows[0].clone());
  table.version = 1;
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), before.clone()]);

  let op = EditOperation::ReplaceBlock { block_ix: 1, before, after };
  op.redo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  assert_eq!(table.rows.len(), 2);
  op.undo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  assert_eq!(table.rows.len(), 1);
}

#[test]
fn table_cell_text_edit_is_a_replace_block_history_operation() {
  let before = Block::Table(TableBlock {
    rows: vec![TableRow {
      cells: vec![TableCell {
        blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
          paragraph: Paragraph {
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
  });
  let mut after = before.clone();
  let Block::Table(table) = &mut after else {
    unreachable!();
  };
  let TableCellBlock::Paragraph(paragraph) = &mut table.rows[0].cells[0].blocks[0] else {
    unreachable!();
  };
  paragraph.text = "cell".to_string();
  paragraph.paragraph.byte_range = 0.."cell".len();
  paragraph.paragraph.runs = vec![TextRun {
    len: "cell".len(),
    styles: RunStyles::default(),
  }];
  table.version = 1;

  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), before.clone()]);
  let op = EditOperation::ReplaceBlock { block_ix: 1, before, after };
  op.redo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  let TableCellBlock::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("expected paragraph");
  };
  assert_eq!(paragraph.text, "cell");
  op.undo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  let TableCellBlock::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("expected paragraph");
  };
  assert!(paragraph.text.is_empty());
}

#[test]
fn replace_block_operation_undo_redo_preserves_equation_source_changes() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let before = Block::Equation(EquationBlock {
    source: "x".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  });
  let after = Block::Equation(EquationBlock {
    source: "x+1".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 1,
  });
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), before.clone()]);
  let op = EditOperation::ReplaceBlock { block_ix: 1, before, after };
  op.redo(&mut document);
  let Block::Equation(equation) = &document.blocks[1] else {
    panic!("expected equation");
  };
  assert_eq!(equation.source.as_ref(), "x+1");
  op.undo(&mut document);
  let Block::Equation(equation) = &document.blocks[1] else {
    panic!("expected equation");
  };
  assert_eq!(equation.source.as_ref(), "x");
}

#[test]
fn default_inserted_table_shape_round_trips_through_db8() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let table = Block::Table(TableBlock {
    rows: (0..2)
      .map(|_| TableRow {
        cells: (0..2)
          .map(|_| TableCell {
            blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
              paragraph: Paragraph {
                style: ParagraphStyle::Normal,
                byte_range: 0..0,
                runs: Vec::new(),
                version: 0,
              },
              text: String::new(),
            })],
            row_span: 1,
            col_span: 1,
          })
          .collect(),
      })
      .collect(),
    column_widths: vec![TableColumnWidth::Fraction(1), TableColumnWidth::Fraction(1)],
    style: TableStyle { header_row: false },
    version: 0,
  });
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), table]);

  let path = std::env::temp_dir().join(format!("flowstate-default-table-{}.db8", uuid::Uuid::new_v4()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  let Block::Table(table) = &loaded.blocks[1] else {
    panic!("expected table block");
  };
  assert_eq!(table.rows.len(), 2);
  assert!(table.rows.iter().all(|row| row.cells.len() == 2));
  assert_eq!(table.column_widths.len(), 2);
}

#[test]
fn table_cell_paragraph_clipboard_conversion_preserves_text_and_styles() {
  let styles = RunStyles::default().with(RunStyle::Emphasis);
  let paragraph = InputParagraph {
    style: ParagraphStyle::Tag,
    runs: vec![InputRun {
      text: "cell text".to_string(),
      styles,
    }],
  };
  let cell = table_cell_paragraph_from_input_paragraph(&paragraph);
  assert_eq!(cell.text, "cell text");
  assert_eq!(cell.paragraph.style, ParagraphStyle::Tag);
  assert_eq!(cell.paragraph.runs[0].styles, styles);

  let restored = input_paragraph_from_table_cell_paragraph(&cell);
  assert_eq!(input_paragraph_text(&restored), "cell text");
  assert_eq!(restored.style, ParagraphStyle::Tag);
  assert_eq!(restored.runs[0].styles, styles);
}

#[test]
fn splitting_table_cell_paragraph_preserves_text_and_run_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut cell = TableCell {
    blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
      paragraph: Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0.."alpha beta".len(),
        runs: vec![
          TextRun {
            len: "alpha ".len(),
            styles: RunStyles::default(),
          },
          TextRun {
            len: "beta".len(),
            styles: emphasized,
          },
        ],
        version: 0,
      },
      text: "alpha beta".to_string(),
    })],
    row_span: 1,
    col_span: 1,
  };

  let new_ix = split_table_cell_paragraph_at(&mut cell, 0, "alpha ".len()).unwrap();
  assert_eq!(new_ix, 1);

  let TableCellBlock::Paragraph(left) = &cell.blocks[0] else {
    panic!("expected left paragraph");
  };
  let TableCellBlock::Paragraph(right) = &cell.blocks[1] else {
    panic!("expected right paragraph");
  };

  assert_eq!(left.text, "alpha ");
  assert_eq!(right.text, "beta");
  assert_eq!(left.paragraph.runs[0].styles, RunStyles::default());
  assert_eq!(right.paragraph.runs[0].styles, emphasized);
  assert_eq!(left.paragraph.byte_range, 0.."alpha ".len());
  assert_eq!(right.paragraph.byte_range, 0.."beta".len());
}

#[test]
fn merging_table_cell_paragraphs_preserves_boundary_caret_and_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut cell = TableCell {
    blocks: vec![
      TableCellBlock::Paragraph(TableCellParagraph {
        paragraph: Paragraph {
          style: ParagraphStyle::Normal,
          byte_range: 0.."left".len(),
          runs: vec![TextRun {
            len: "left".len(),
            styles: RunStyles::default(),
          }],
          version: 0,
        },
        text: "left".to_string(),
      }),
      TableCellBlock::Paragraph(TableCellParagraph {
        paragraph: Paragraph {
          style: ParagraphStyle::Normal,
          byte_range: 0.."right".len(),
          runs: vec![TextRun {
            len: "right".len(),
            styles: emphasized,
          }],
          version: 0,
        },
        text: "right".to_string(),
      }),
    ],
    row_span: 1,
    col_span: 1,
  };

  let (paragraph_ix, caret) = merge_table_cell_paragraph_with_previous(&mut cell, 1).unwrap();
  assert_eq!((paragraph_ix, caret), (0, "left".len()));
  assert_eq!(cell.blocks.len(), 1);

  let TableCellBlock::Paragraph(merged) = &cell.blocks[0] else {
    panic!("expected merged paragraph");
  };
  assert_eq!(merged.text, "leftright");
  assert_eq!(merged.paragraph.runs.len(), 2);
  assert_eq!(merged.paragraph.runs[0].styles, RunStyles::default());
  assert_eq!(merged.paragraph.runs[1].styles, emphasized);
}

#[test]
fn inserting_rich_paragraphs_into_table_cell_preserves_tail_and_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let cite = RunStyles::default().with(RunStyle::Cite);
  let mut cell = TableCell {
    blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
      paragraph: Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0.."alpha omega".len(),
        runs: vec![
          TextRun {
            len: "alpha ".len(),
            styles: RunStyles::default(),
          },
          TextRun {
            len: "omega".len(),
            styles: emphasized,
          },
        ],
        version: 0,
      },
      text: "alpha omega".to_string(),
    })],
    row_span: 1,
    col_span: 1,
  };

  let inserted = vec![
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "B".to_string(),
        styles: cite,
      }],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "C".to_string(),
        styles: cite,
      }],
    },
  ];

  let caret = insert_table_cell_paragraphs_at(&mut cell, 0, "alpha ".len(), &inserted).unwrap();
  assert_eq!(caret, (1, "C".len()));
  assert_eq!(cell.blocks.len(), 2);

  let TableCellBlock::Paragraph(first) = &cell.blocks[0] else {
    panic!("expected first paragraph");
  };
  let TableCellBlock::Paragraph(second) = &cell.blocks[1] else {
    panic!("expected second paragraph");
  };
  assert_eq!(first.text, "alpha B");
  assert_eq!(second.text, "Comega");
  assert_eq!(first.paragraph.runs.last().unwrap().styles, cite);
  assert_eq!(second.paragraph.runs[0].styles, cite);
  assert_eq!(second.paragraph.runs[1].styles, emphasized);
}

#[test]
fn document_position_round_trips_top_level_text_blocks() {
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
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Image(ImageBlock {
      asset_id: AssetId(42),
      alt_text: "missing".into(),
      caption: None,
      sizing: ImageSizing::Intrinsic,
      alignment: BlockAlignment::Center,
      version: 0,
    }),
    Block::Paragraph(document.paragraphs[1].clone()),
  ]);

  let offset = DocumentOffset { paragraph: 1, byte: 3 };
  let position = document_position_for_offset(&document, offset).unwrap();
  assert_eq!(position, DocumentPosition::Text { block_ix: 2, byte: 3 });
  assert_eq!(document_offset_for_position(&document, &position), Some(offset));
  assert_eq!(
    document_offset_for_position(
      &document,
      &DocumentPosition::Object {
        block_ix: 1,
        affinity: ObjectAffinity::Before,
      }
    ),
    None
  );
}

#[test]
fn db8_validation_rejects_zero_sized_fixed_images() {
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
      asset_id: AssetId(99),
      alt_text: "invalid".into(),
      caption: None,
      sizing: ImageSizing::Fixed {
        width_px: 0,
        height_px: None,
      },
      alignment: BlockAlignment::Left,
      version: 0,
    }),
  ]);

  let path = std::env::temp_dir().join(format!("flowstate-invalid-image-{}.db8", uuid::Uuid::new_v4()));
  let error = write_db8(&path, &document).unwrap_err();
  assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
  let _ = std::fs::remove_file(path);
}

#[test]
fn double_click_at_text_paragraph_end_selects_only_that_paragraph() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("first paragraph")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("next paragraph")],
      },
    ],
  );

  let selection = selection_for_word_at(
    &document,
    DocumentOffset {
      paragraph: 0,
      byte: "first paragraph".len(),
    },
  );

  assert_eq!(
    selection,
    EditorSelection {
      anchor: DocumentOffset { paragraph: 0, byte: 0 },
      head: DocumentOffset {
        paragraph: 0,
        byte: "first paragraph".len(),
      },
    }
  );
}

#[test]
fn double_click_empty_paragraph_selects_only_empty_paragraph() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("before")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("after")],
      },
    ],
  );

  let selection = selection_for_word_at(&document, DocumentOffset { paragraph: 1, byte: 0 });

  assert_eq!(
    selection,
    EditorSelection {
      anchor: DocumentOffset { paragraph: 1, byte: 0 },
      head: DocumentOffset { paragraph: 1, byte: 0 },
    }
  );
}

#[test]
fn selection_across_empty_paragraphs_and_clear_formatting_policy() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Tag,
        runs: vec![run("tag", emphasized)],
      },
      InputParagraph {
        style: ParagraphStyle::Pocket,
        runs: Vec::new(),
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![run("body", emphasized)],
      },
    ],
  );
  let selection = DocumentOffset { paragraph: 0, byte: 1 }..DocumentOffset { paragraph: 2, byte: 1 };
  assert!(selection_contains_whole_paragraph(&document, selection.clone()));

  for paragraph_ix in selection.start.paragraph..=selection.end.paragraph {
    clear_whole_paragraph_formatting(&mut document, paragraph_ix);
  }

  for paragraph in document.paragraphs.iter() {
    assert_eq!(paragraph.style, ParagraphStyle::Normal);
    assert!(
      paragraph
        .runs
        .iter()
        .all(|run| run.styles == RunStyles::default())
    );
  }
}

#[test]
fn run_style_full_selection_toggle_policy() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![run("all", emphasized), plain(" plain")],
    }],
  );

  assert!(selection_all_run_styles(
    &document,
    DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
      paragraph: 0,
      byte: "all".len(),
    },
    |styles| styles.semantic == RunSemanticStyle::Emphasis,
  ));
  assert!(!selection_all_run_styles(
    &document,
    DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
      paragraph: 0,
      byte: "all plain".len(),
    },
    |styles| styles.semantic == RunSemanticStyle::Emphasis,
  ));
}

#[test]
fn semantic_run_styles_are_mutually_exclusive() {
  let mut styles = RunStyles::default().with(RunStyle::Emphasis);
  styles.apply(RunStyle::Condensed);
  assert_eq!(styles.semantic, RunSemanticStyle::Condensed);
  styles.apply(RunStyle::Ultracondensed);
  assert_eq!(styles.semantic, RunSemanticStyle::Ultracondensed);
}

#[test]
fn db8_round_trip_preserves_condensed_semantic_styles() {
  let path = std::env::temp_dir().join(format!("flowstate-semantic-{}.db8", uuid::Uuid::new_v4()));
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run("condensed", RunStyles::default().with(RunStyle::Condensed)),
        run(
          " ultra",
          RunStyles::default()
            .with(RunStyle::Ultracondensed)
            .with(RunStyle::HighlightSpoken),
        ),
      ],
    }],
  );
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(loaded.paragraphs[0].runs[0].styles.semantic, RunSemanticStyle::Condensed);
  assert_eq!(loaded.paragraphs[0].runs[1].styles.semantic, RunSemanticStyle::Ultracondensed);
  assert_eq!(loaded.paragraphs[0].runs[1].styles.highlight, Some(HighlightStyle::Spoken));
}

#[test]
fn db8_save_can_replace_existing_file() {
  let path = std::env::temp_dir().join(format!("flowstate-replace-{}.db8", uuid::Uuid::new_v4()));
  let first = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("first")],
    }],
  );
  let second = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("second")],
    }],
  );

  write_db8(&path, &first).unwrap();
  write_db8(&path, &second).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(paragraph_text(&loaded, 0), "second");
}

#[test]
fn history_operation_round_trip_for_text_and_paragraph_split() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta")],
    }],
  );
  let before = capture_document_span(&document, 0..1);
  split_paragraph_at(&mut document, 0, "alpha".len());
  insert_text_at(&mut document, 1, 0, "NEW ", RunStyles::default().with(RunStyle::Emphasis));
  let after = capture_document_span(&document, 0..2);
  assert_eq!(document.paragraphs.len(), 2);

  let operation = EditOperation::ReplaceParagraphSpan { before, after };
  operation.undo(&mut document);
  assert_eq!(document.paragraphs.len(), 1);
  assert_eq!(paragraph_text(&document, 0), "alpha beta");

  operation.redo(&mut document);
  assert_eq!(document.paragraphs.len(), 2);
  assert_eq!(paragraph_text(&document, 0), "alpha");
  assert_eq!(paragraph_text(&document, 1), "NEW  beta");
  assert_eq!(document.paragraphs[1].runs[0].styles.semantic, RunSemanticStyle::Emphasis);
}

#[test]
fn dragged_text_drop_offset_adjusts_after_source_deletion() {
  let source = DocumentOffset { paragraph: 0, byte: 2 }..DocumentOffset { paragraph: 0, byte: 5 };
  assert_eq!(
    adjust_drop_after_source_delete(DocumentOffset { paragraph: 0, byte: 8 }, source.clone()),
    DocumentOffset { paragraph: 0, byte: 5 }
  );
  assert_eq!(
    adjust_drop_after_source_delete(DocumentOffset { paragraph: 0, byte: 1 }, source),
    DocumentOffset { paragraph: 0, byte: 1 }
  );

  let cross = DocumentOffset { paragraph: 1, byte: 2 }..DocumentOffset { paragraph: 3, byte: 4 };
  assert_eq!(
    adjust_drop_after_source_delete(DocumentOffset { paragraph: 5, byte: 7 }, cross),
    DocumentOffset { paragraph: 3, byte: 7 }
  );
}

#[test]
fn move_rich_text_operation_undo_redo_restores_source_and_drop() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("abc "), run("MOVE", emphasized), plain(" def")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("target")],
      },
    ],
  );
  let source = DocumentOffset {
    paragraph: 0,
    byte: "abc ".len(),
  }..DocumentOffset {
    paragraph: 0,
    byte: "abc MOVE".len(),
  };
  let fragment = selected_rich_fragment(&document, source.clone());
  let drop = DocumentOffset {
    paragraph: 1,
    byte: "tar".len(),
  };
  let adjusted_drop = adjust_drop_after_source_delete(drop, source.clone());
  delete_cross_paragraph_range(&mut document, source.clone());
  let inserted_end = insert_rich_fragment_at(&mut document, adjusted_drop, &fragment);
  let operation = EditOperation::MoveRichText {
    source_range: source,
    adjusted_drop,
    inserted_range: adjusted_drop..inserted_end,
    fragment,
  };

  assert_eq!(paragraph_text(&document, 0), "abc  def");
  assert_eq!(paragraph_text(&document, 1), "tarMOVEget");
  assert!(
    document.paragraphs[1]
      .runs
      .iter()
      .any(|run| run.styles.semantic == RunSemanticStyle::Emphasis)
  );

  operation.undo(&mut document);
  assert_eq!(paragraph_text(&document, 0), "abc MOVE def");
  assert_eq!(paragraph_text(&document, 1), "target");
  assert!(
    document.paragraphs[0]
      .runs
      .iter()
      .any(|run| run.styles.semantic == RunSemanticStyle::Emphasis)
  );

  operation.redo(&mut document);
  assert_eq!(paragraph_text(&document, 0), "abc  def");
  assert_eq!(paragraph_text(&document, 1), "tarMOVEget");
  assert!(
    document.paragraphs[1]
      .runs
      .iter()
      .any(|run| run.styles.semantic == RunSemanticStyle::Emphasis)
  );
}

#[test]
fn soft_line_break_stays_inside_paragraph_and_copies_as_newline() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alphaomega")],
    }],
  );
  insert_text_at(&mut document, 0, "alpha".len(), SOFT_LINE_BREAK_STR, RunStyles::default());

  assert_eq!(document.paragraphs.len(), 1);
  assert_eq!(paragraph_text(&document, 0), format!("alpha{SOFT_LINE_BREAK_STR}omega"));
  assert_eq!(
    selected_plain_text(
      &document,
      DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
        paragraph: 0,
        byte: paragraph_text_len(&document.paragraphs[0]),
      },
    ),
    "alpha\nomega"
  );
}

#[test]
fn find_text_ranges_returns_document_offsets_across_paragraphs() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("alpha")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("beta alpha")],
      },
    ],
  );
  let matches = find_text_ranges(&document, "alpha");
  assert_eq!(matches.len(), 2);
  assert_eq!(matches[0].start, DocumentOffset { paragraph: 0, byte: 0 });
  assert_eq!(
    matches[0].end,
    DocumentOffset {
      paragraph: 0,
      byte: "alpha".len()
    }
  );
  assert_eq!(
    matches[1].start,
    DocumentOffset {
      paragraph: 1,
      byte: "beta ".len()
    }
  );
  assert_eq!(
    matches[1].end,
    DocumentOffset {
      paragraph: 1,
      byte: "beta alpha".len()
    }
  );
}

#[test]
fn cross_paragraph_style_mutation_keeps_runs_and_unselected_text_intact() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("abc")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("def")],
      },
    ],
  );
  mutate_runs_in_range(
    &mut document,
    DocumentOffset { paragraph: 0, byte: 1 }..DocumentOffset { paragraph: 1, byte: 2 },
    |styles| styles.semantic = RunSemanticStyle::Cite,
  );

  assert_eq!(paragraph_text(&document, 0), "abc");
  assert_eq!(paragraph_text(&document, 1), "def");
  assert_ne!(document.paragraphs[0].runs[0].styles.semantic, RunSemanticStyle::Cite);
  assert_eq!(document.paragraphs[0].runs[1].styles.semantic, RunSemanticStyle::Cite);
  assert_eq!(document.paragraphs[1].runs[0].styles.semantic, RunSemanticStyle::Cite);
  assert_ne!(document.paragraphs[1].runs[1].styles.semantic, RunSemanticStyle::Cite);
}
