<<<<<<< HEAD
=======

>>>>>>> main
#[test]
#[hotpath::measure]
fn single_paragraph_edits_keep_following_derived_byte_ranges_current() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("first")],
      },
      InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![plain("second")],
      },
    ],
  );

  insert_text_at(&mut document, 0, "first".len(), " extended", RunStyles::default());

  let range = paragraph_byte_range(&document, 1);
  assert_eq!(document_text_slice(&document, range.clone()), "second");
  assert!(range.end <= document.text.byte_len());
}

#[test]
#[hotpath::measure]
fn document_round_trip_preserves_text_structure_and_styles() {
  let document = demo_document();
  let dir = std::env::temp_dir();
  let path = dir.join(format!("flowtext-test-{}.document", std::process::id()));
  write_document(&path, &document).unwrap();
  let loaded = read_document(&path).unwrap();
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
#[hotpath::measure]
fn split_and_merge_preserve_empty_styled_paragraphs() {
  let spoken = RunStyles::default().with(RunStyle::Highlight(1));
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Custom(0),
      runs: vec![run("Custom heading", spoken)],
    }],
  );

  let first_len = paragraph_text_len(&document.paragraphs[0]);
<<<<<<< HEAD
  assert!(split_paragraph_at(&mut document, 0, first_len));
=======
  split_paragraph_at(&mut document, 0, first_len);
>>>>>>> main
  assert_eq!(document.paragraphs.len(), 2);
  assert_eq!(document.paragraphs[1].style, ParagraphStyle::Custom(0));
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
  assert_eq!(paragraph_text(&document, 0), "Custom heading");
  assert_eq!(
    document.paragraphs[0].runs,
    vec![TextRun {
      len: "Custom heading".len(),
      styles: spoken
    }]
  );
}

#[test]
#[hotpath::measure]
fn document_round_trip_preserves_empty_styled_paragraphs() {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: Vec::new(),
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("body")],
      },
    ],
  );
  let path = std::env::temp_dir().join(format!("flowtext-empty-{}.document", std::process::id()));
  write_document(&path, &document).unwrap();
  let loaded = read_document(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(loaded.paragraphs[0].style, ParagraphStyle::Custom(0));
  assert_eq!(paragraph_text_len(&loaded.paragraphs[0]), 0);
  assert!(loaded.paragraphs[0].runs.is_empty());
  assert_eq!(paragraph_text(&loaded, 1), "body");
}

#[test]
#[hotpath::measure]
fn document_v4_round_trip_preserves_mixed_block_order_and_assets() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Custom(0),
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

  let path = std::env::temp_dir().join(format!("flowtext-blocks-{}.document", uuid::Uuid::new_v4()));
  write_document(&path, &document).unwrap();
  let loaded = read_document(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(loaded.blocks.len(), 4);
  assert!(matches!(loaded.blocks[0], Block::Paragraph(_)));
  assert!(matches!(loaded.blocks[1], Block::Image(_)));
  assert!(matches!(loaded.blocks[2], Block::Paragraph(_)));
  assert!(matches!(loaded.blocks[3], Block::Equation(_)));
  assert_eq!(loaded.assets.assets[&asset_id].bytes.as_slice(), &[1, 2, 3, 4]);
}

#[test]
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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

#[hotpath::measure]
fn test_png_2x1() -> Vec<u8> {
  vec![
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0, 244, 34, 127, 138, 0, 0, 0, 12, 73, 68,
    65, 84, 8, 29, 99, 248, 15, 4, 0, 9, 251, 3, 253, 167, 170, 43, 113, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
  ]
}

#[test]
#[hotpath::measure]
fn document_v4_round_trip_preserves_table_cell_paragraph_and_run_styles() {
  let emphasized = RunStyles::default()
    .with(RunStyle::Semantic(2))
    .with(RunStyle::Highlight(1));
  let cell_paragraph = Paragraph {
    style: ParagraphStyle::Custom(3),
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

  let path = std::env::temp_dir().join(format!("flowtext-table-{}.document", uuid::Uuid::new_v4()));
  write_document(&path, &document).unwrap();
  let loaded = read_document(&path).unwrap();
  let _ = std::fs::remove_file(path);

  let Block::Table(table) = &loaded.blocks[1] else {
    panic!("expected table block");
  };
  assert!(table.style.header_row);
  let TableCellBlock::Paragraph(loaded_paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("expected table-cell paragraph");
  };
  assert_eq!(loaded_paragraph.paragraph.style, ParagraphStyle::Custom(3));
  assert_eq!(loaded_paragraph.paragraph.runs, cell_paragraph.runs);
  assert_eq!(loaded_paragraph.text, "cell");
}
