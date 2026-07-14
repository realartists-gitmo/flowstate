// NOTE: include!()-spliced into tests/mod.rs — imports and the shared
// authority helpers live there.

#[test]
#[hotpath::measure]
fn layout_fragments_preserve_text_when_run_boundary_splits_utf8_character() {
  let text = "state\u{2019}s overconfidence";
  let split_inside_apostrophe = "state".len() + 1;
  let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
  let paragraph = Paragraph {
    style: ParagraphStyle::Normal,
    runs: vec![
      TextRun {
        len: split_inside_apostrophe,
        styles: RunStyles::default(),
      },
      TextRun {
        len: text.len() - split_inside_apostrophe,
        styles: emphasized,
      },
    ],
    version: 0,
  };

  let fragments = fragments_for_range(&paragraph, &(0..text.len()), text);
  let mut rendered = String::new();
  for fragment in &fragments {
    assert!(text.is_char_boundary(fragment.line_range.start));
    assert!(text.is_char_boundary(fragment.line_range.end));
    rendered.push_str(&text[fragment.line_range.clone()]);
  }

  assert_eq!(rendered, text);
  assert_eq!(&text[fragments[0].line_range.clone()], "state\u{2019}");
  assert_eq!(&text[fragments[1].line_range.clone()], "s overconfidence");
}
