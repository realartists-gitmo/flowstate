
#[test]
#[hotpath::measure]
fn inline_decorations_merge_across_segment_splits() {
  let color = black();
  let merged = merge_inline_decorations(vec![
    Decoration {
      bounds: Bounds::new(point(px(0.0), px(12.0)), size(px(10.0), px(1.0))),
      color,
    },
    Decoration {
      bounds: Bounds::new(point(px(10.25), px(12.0)), size(px(6.0), px(1.0))),
      color,
    },
    Decoration {
      bounds: Bounds::new(point(px(30.0), px(12.0)), size(px(4.0), px(1.0))),
      color,
    },
  ]);

  assert_eq!(merged.len(), 2);
  assert_eq!(merged[0].bounds.origin.x, px(0.0));
  assert_eq!(merged[0].bounds.size.width, px(16.25));
  assert_eq!(merged[1].bounds.origin.x, px(30.0));
}

#[test]
#[hotpath::measure]
fn boxed_fragment_padding_is_only_applied_to_outer_emphasis_edges() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let highlighted_emphasis = emphasized.with(RunStyle::HighlightSpoken);
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![run("left", emphasized), run("middle", highlighted_emphasis), run("right", emphasized)],
    }],
  );
  let paragraph = &document.paragraphs[0];
  let text = paragraph_text(&document, 0);
  let p_format = paragraph_format(&document, paragraph.style);
  let fragments = formatted_fragments_for_range(&document, &p_format, paragraph, &(0..text.len()), &text);
  let left_pad = document.theme.box_padding_left;
  let right_pad = document.theme.box_padding_right;

  assert_eq!(fragments.len(), 3);
  assert_eq!(boxed_fragment_padding(&fragments, 0, left_pad, right_pad), (left_pad, px(0.0)));
  assert_eq!(boxed_fragment_padding(&fragments, 1, left_pad, right_pad), (px(0.0), px(0.0)));
  assert_eq!(boxed_fragment_padding(&fragments, 2, left_pad, right_pad), (px(0.0), right_pad));

  let old_internal_gap = left_pad + right_pad;
  assert!(f32::from(old_internal_gap) > 0.0);
  assert_eq!(
    boxed_fragment_padding(&fragments, 0, left_pad, right_pad).1
      + boxed_fragment_padding(&fragments, 1, left_pad, right_pad).0,
    px(0.0)
  );
}

#[test]
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
