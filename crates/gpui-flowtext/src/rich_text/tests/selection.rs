
#[test]
#[hotpath::measure]
fn smart_word_selection_is_enabled_by_default() {
  assert!(RichTextEditorConfig::default().smart_word_selection);
}

#[test]
#[hotpath::measure]
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
#[hotpath::measure]
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
#[hotpath::measure]
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
