
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
  document.blocks = crate::BlockSeq::from_vec(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Image(ImageBlock {
      asset_id: AssetId(42),
      alt_text: "missing".into(),
      caption: None,
      sizing: ImageSizing::Intrinsic,
      alignment: BlockAlignment::Center,
      external_url: None,
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
