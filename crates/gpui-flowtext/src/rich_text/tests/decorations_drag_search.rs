
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
  let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
  let highlighted_emphasis = emphasized.with(RunStyle::Highlight(1));
  let mut theme = DocumentTheme::default();
  theme.set_custom_semantic_style(
    2,
    CustomSemanticStyle {
      border_width: Some(px(1.0)),
      ..CustomSemanticStyle::default()
    },
  );
  theme.box_padding_left = px(1.28);
  theme.box_padding_right = px(1.3466667);
  let document = document_from_input(
    theme,
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
fn custom_style_slots_resolve_from_document_theme() {
  let mut theme = DocumentTheme::default();
  theme.set_custom_paragraph_style(
    2,
    CustomParagraphStyle {
      font_size: px(20.0),
      font_family: None,
      color: gpui::rgb(0x0012_3456).into(),
      bold: true,
      italic: true,
      underline: ThemeUnderline::Single,
      align: CustomParagraphAlign::Center,
      spacing_before: px(3.0),
      spacing_after: px(4.0),
      border: Some(CustomParagraphBorder {
        width: px(1.0),
        space_x: px(2.0),
        space_y: px(3.0),
      }),
      section_kind: None,
      section_level: None,
    },
  );
  theme.set_custom_semantic_style(
    4,
    CustomSemanticStyle {
      font_size: Some(px(15.0)),
      font_family: None,
      color: Some(gpui::rgb(0x0065_4321).into()),
      bold: Some(true),
      italic: Some(false),
      underline: Some(ThemeUnderline::Double),
      border_width: Some(px(2.0)),
    },
  );
  theme.set_custom_highlight_style(
    7,
    CustomHighlightStyle {
      color: gpui::rgb(0x00ab_cdef).into(),
    },
  );
  let document = document_from_input(
    theme,
    vec![InputParagraph {
      style: ParagraphStyle::Custom(2),
      runs: vec![run(
        "custom",
        RunStyles {
          semantic: RunSemanticStyle::Custom(4),
          highlight: Some(HighlightStyle::Custom(7)),
          ..RunStyles::default()
        },
      )],
    }],
  );

  let paragraph = &document.paragraphs[0];
  let text = paragraph_text(&document, 0);
  let p_format = paragraph_format(&document, paragraph.style);
  let fragments = formatted_fragments_for_range(&document, &p_format, paragraph, &(0..text.len()), &text);

  assert_eq!(p_format.font_size, px(20.0));
  assert!(matches!(p_format.align, ParagraphAlign::Center));
  assert!(p_format.border.is_some());
  assert_eq!(fragments[0].format.font_size, px(15.0));
  assert_eq!(fragments[0].format.color, gpui::rgb(0x0065_4321).into());
  assert_eq!(fragments[0].format.highlight, Some(gpui::rgb(0x00ab_cdef).into()));
  assert_eq!(fragments[0].format.border_width, px(2.0));
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


// C-S4 headless coverage for the review-mark overlay model: the per-paragraph
// filter, the hover flag, and hover reset on set replacement. The caret bug
// taught us the decoration layer ships blind without model-level nets.
#[gpui::test]
fn annotation_marks_filter_per_paragraph_and_track_hover(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let document = document_from_input(
    DocumentTheme::default(),
    vec![
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("first paragraph carrying a comment span")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("second paragraph outside every span")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("third paragraph carrying another span")],
      },
    ],
  );
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(document, None, cx));
  handle
    .update(cx, |editor, _window, cx| {
      let spans = vec![
        ExternalSelection {
          selection: EditorSelection::range(
            DocumentOffset { paragraph: 0, byte: 6 },
            DocumentOffset { paragraph: 0, byte: 15 },
          ),
          color_rgb: 0x00d9_9a20,
        },
        ExternalSelection {
          selection: EditorSelection::range(
            DocumentOffset { paragraph: 2, byte: 0 },
            DocumentOffset { paragraph: 2, byte: 5 },
          ),
          color_rgb: 0x00d9_9a20,
        },
      ];
      editor.set_annotation_selections(spans.clone(), cx);
      assert_eq!(editor.annotation_selections(), spans.as_slice());

      let first = editor.annotation_selections_for_paragraph(0);
      assert_eq!(first.len(), 1, "paragraph 0 intersects exactly one span");
      assert!(!first[0].1, "nothing is hovered yet");
      assert!(
        editor.annotation_selections_for_paragraph(1).is_empty(),
        "paragraph 1 has no marks"
      );

      editor.update_annotation_hover(DocumentOffset { paragraph: 2, byte: 3 }, cx);
      let third = editor.annotation_selections_for_paragraph(2);
      assert_eq!(third.len(), 1);
      assert!(third[0].1, "the span under the pointer must carry the hover flag");
      assert!(
        !editor.annotation_selections_for_paragraph(0)[0].1,
        "hover is exclusive to the hit span"
      );

      editor.update_annotation_hover(DocumentOffset { paragraph: 1, byte: 0 }, cx);
      assert!(
        !editor.annotation_selections_for_paragraph(2)[0].1,
        "moving off the span drops the hover"
      );

      editor.update_annotation_hover(DocumentOffset { paragraph: 2, byte: 3 }, cx);
      editor.set_annotation_selections(vec![spans[1].clone()], cx);
      assert!(
        !editor.annotation_selections_for_paragraph(2)[0].1,
        "replacing the annotation set must reset the hover index"
      );

      editor.set_annotation_selections(Vec::new(), cx);
      assert!(editor.annotation_selections().is_empty(), "clearing removes the overlay");
    })
    .unwrap();
}
