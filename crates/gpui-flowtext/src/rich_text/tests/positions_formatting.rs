
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
    EditorSelection::range(
      DocumentOffset { paragraph: 0, byte: 0 },
      DocumentOffset {
        paragraph: 0,
        byte: "first paragraph".len(),
      },
    )
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
    EditorSelection::collapsed(DocumentOffset { paragraph: 1, byte: 0 })
  );
}

#[test]
#[hotpath::measure]
fn selection_across_empty_paragraphs_and_clear_formatting_policy() {
  let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Custom(3),
        runs: vec![run("tag", emphasized)],
      },
      InputParagraph {
        style: ParagraphStyle::Custom(0),
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
  let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
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
    |styles| styles.semantic == RunSemanticStyle::Custom(2),
  ));
  assert!(!selection_all_run_styles(
    &document,
    DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
      paragraph: 0,
      byte: "all plain".len(),
    },
    |styles| styles.semantic == RunSemanticStyle::Custom(2),
  ));
}

#[test]
#[hotpath::measure]
fn semantic_run_styles_are_mutually_exclusive() {
  let mut styles = RunStyles::default().with(RunStyle::Semantic(2));
  styles.apply(RunStyle::Semantic(4));
  assert_eq!(styles.semantic, RunSemanticStyle::Custom(4));
  styles.apply(RunStyle::Semantic(5));
  assert_eq!(styles.semantic, RunSemanticStyle::Custom(5));
}
