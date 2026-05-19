use super::*;

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
    DocumentOffset { paragraph: 0, byte: "alpha be".len() },
    SelectionGranularity::Character,
    smart,
  )
  .normalized();
  assert_eq!(snapped.start.byte, 0);
  assert_eq!(snapped.end.byte, "alpha beta".len());

  let after_first_word = expand_mouse_selection(
    &document,
    DocumentOffset { paragraph: 0, byte: 2 },
    DocumentOffset { paragraph: 0, byte: "alpha".len() },
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
    DocumentOffset { paragraph: 0, byte: "alpha be".len() },
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
    DocumentOffset { paragraph: 0, byte: "alpha be".len() },
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
  let path = dir.join(format!("debateprocessor-test-{}.db8", std::process::id()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(
    document_text_slice(&document, 0..document.text.byte_len()),
    document_text_slice(&loaded, 0..loaded.text.byte_len())
  );
  assert_eq!(document.paragraphs.len(), loaded.paragraphs.len());
  // Verify styles and run structure for every paragraph, not just the first.
  for (ix, (orig, loaded_para)) in document.paragraphs.iter().zip(loaded.paragraphs.iter()).enumerate() {
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
  assert_eq!(document.paragraphs[0].runs, vec![TextRun { len: "Pocket".len(), styles: spoken }]);
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
  let path = std::env::temp_dir().join(format!("debateprocessor-empty-{}.db8", std::process::id()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  assert_eq!(loaded.paragraphs[0].style, ParagraphStyle::Pocket);
  assert_eq!(paragraph_text_len(&loaded.paragraphs[0]), 0);
  assert!(loaded.paragraphs[0].runs.is_empty());
  assert_eq!(paragraph_text(&loaded, 1), "body");
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
    assert!(paragraph.runs.iter().all(|run| run.styles == RunStyles::default()));
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
  let path = std::env::temp_dir().join(format!("debateprocessor-semantic-{}.db8", uuid::Uuid::new_v4()));
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run("condensed", RunStyles::default().with(RunStyle::Condensed)),
        run(" ultra", RunStyles::default().with(RunStyle::Ultracondensed).with(RunStyle::HighlightSpoken)),
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
  let path = std::env::temp_dir().join(format!("debateprocessor-replace-{}.db8", uuid::Uuid::new_v4()));
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
  let source = DocumentOffset { paragraph: 0, byte: "abc ".len() }..DocumentOffset { paragraph: 0, byte: "abc MOVE".len() };
  let fragment = selected_rich_fragment(&document, source.clone());
  let drop = DocumentOffset { paragraph: 1, byte: "tar".len() };
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
  assert!(document.paragraphs[1].runs.iter().any(|run| run.styles.semantic == RunSemanticStyle::Emphasis));

  operation.undo(&mut document);
  assert_eq!(paragraph_text(&document, 0), "abc MOVE def");
  assert_eq!(paragraph_text(&document, 1), "target");
  assert!(document.paragraphs[0].runs.iter().any(|run| run.styles.semantic == RunSemanticStyle::Emphasis));

  operation.redo(&mut document);
  assert_eq!(paragraph_text(&document, 0), "abc  def");
  assert_eq!(paragraph_text(&document, 1), "tarMOVEget");
  assert!(document.paragraphs[1].runs.iter().any(|run| run.styles.semantic == RunSemanticStyle::Emphasis));
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
  assert_eq!(matches[0].end, DocumentOffset { paragraph: 0, byte: "alpha".len() });
  assert_eq!(matches[1].start, DocumentOffset { paragraph: 1, byte: "beta ".len() });
  assert_eq!(matches[1].end, DocumentOffset { paragraph: 1, byte: "beta alpha".len() });
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
