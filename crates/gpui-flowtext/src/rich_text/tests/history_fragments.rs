<<<<<<< HEAD
=======

>>>>>>> main
#[test]
#[hotpath::measure]
fn history_operation_round_trip_for_text_and_paragraph_split() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta")],
    }],
  );
  let before = capture_document_span(&document, 0..1);
<<<<<<< HEAD
  assert!(split_paragraph_at(&mut document, 0, "alpha".len()));
  assert!(insert_text_at(
    &mut document,
    1,
    0,
    "NEW ",
    RunStyles::default().with(RunStyle::Semantic(2)),
  ));
=======
  split_paragraph_at(&mut document, 0, "alpha".len());
  insert_text_at(&mut document, 1, 0, "NEW ", RunStyles::default().with(RunStyle::Semantic(2)));
>>>>>>> main
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
  assert_eq!(document.paragraphs[1].runs[0].styles.semantic, RunSemanticStyle::Custom(2));
}

#[test]
#[hotpath::measure]
fn rich_fragment_insert_bulk_preserves_multiline_paste_shape() {
  let cite = RunStyles::default().with(RunStyle::Semantic(1));
  let emphasis = RunStyles::default().with(RunStyle::Semantic(2));
  let underline = RunStyles::default().with(RunStyle::Semantic(3));
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("hello "), run("world", cite)],
    }],
  );
  let fragment = RichClipboardFragment {
    format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
    paragraphs: vec![
      InputParagraph {
        style: ParagraphStyle::Custom(3),
        runs: vec![run("A", emphasis), run("B", emphasis)],
      },
      InputParagraph {
        style: ParagraphStyle::Custom(4),
        runs: vec![run("C", underline)],
      },
    ],
    blocks: Vec::new(),
    assets: Vec::new(),
  };

  let caret = insert_rich_fragment_at(
    &mut document,
    DocumentOffset {
      paragraph: 0,
      byte: "hello ".len(),
    },
    &fragment,
  );

  assert_eq!(caret, DocumentOffset { paragraph: 1, byte: 1 });
  assert_eq!(document.paragraphs.len(), 2);
  assert_eq!(paragraph_text(&document, 0), "hello AB");
  assert_eq!(paragraph_text(&document, 1), "Cworld");
  assert_eq!(document.paragraphs[0].style, ParagraphStyle::Normal);
  assert_eq!(document.paragraphs[1].style, ParagraphStyle::Custom(4));
  assert_eq!(document.paragraphs[0].runs.last().unwrap().styles, emphasis);
  assert_eq!(document.paragraphs[1].runs.first().unwrap().styles, underline);
  assert_eq!(document.paragraphs[1].runs.last().unwrap().styles, cite);
  assert!(matches!(&document.blocks[0], Block::Paragraph(paragraph) if paragraph.byte_range == document.paragraphs[0].byte_range));
  assert!(matches!(&document.blocks[1], Block::Paragraph(paragraph) if paragraph.byte_range == document.paragraphs[1].byte_range));
}

#[test]
#[hotpath::measure]
fn insert_rich_fragment_history_operation_round_trips_without_paragraph_snapshots() {
  let emphasis = RunStyles::default().with(RunStyle::Semantic(2));
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha omega")],
    }],
  );
  let fragment = RichClipboardFragment {
    format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
    paragraphs: vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![run(" BETA", emphasis)],
    }],
    blocks: Vec::new(),
    assets: Vec::new(),
  };
  let offset = DocumentOffset {
    paragraph: 0,
    byte: "alpha".len(),
  };
  let inserted_end = insert_rich_fragment_at(&mut document, offset, &fragment);
  let operation = EditOperation::InsertRichFragment {
    offset,
    inserted_end,
    fragment,
  };

  assert_eq!(paragraph_text(&document, 0), "alpha BETA omega");
  operation.undo(&mut document);
  assert_eq!(paragraph_text(&document, 0), "alpha omega");
  operation.redo(&mut document);
  assert_eq!(paragraph_text(&document, 0), "alpha BETA omega");
  assert!(
    document.paragraphs[0]
      .runs
      .iter()
      .any(|run| run.styles == emphasis)
  );
}
