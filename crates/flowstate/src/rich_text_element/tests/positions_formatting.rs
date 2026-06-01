
#[test]
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
fn semantic_run_styles_are_mutually_exclusive() {
  let mut styles = RunStyles::default().with(RunStyle::Emphasis);
  styles.apply(RunStyle::Condensed);
  assert_eq!(styles.semantic, RunSemanticStyle::Condensed);
  styles.apply(RunStyle::Ultracondensed);
  assert_eq!(styles.semantic, RunSemanticStyle::Ultracondensed);
}

#[test]
#[hotpath::measure]
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
#[hotpath::measure]
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
