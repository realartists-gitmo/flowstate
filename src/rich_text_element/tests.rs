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
  assert_eq!(document.paragraphs[0].runs, loaded.paragraphs[0].runs);
}
