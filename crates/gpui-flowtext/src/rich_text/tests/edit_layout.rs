use super::*;
use gpui::{Bounds, black, point, px, size};
use std::{
  collections::hash_map::DefaultHasher,
  hash::{Hash, Hasher},
};

#[test]
#[hotpath::measure]
fn paragraph_edit_helpers_preserve_text_and_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
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

  apply_style_to_paragraph_range(&mut document, 0, "hey".len().."heyll".len(), RunStyle::Semantic(2));
  assert_eq!(paragraph_text(&document, 0), "heyllo");
  assert_eq!(document.paragraphs[0].runs.len(), 3);
  assert_eq!(document.paragraphs[0].runs[1].styles, emphasized);

  delete_range_in_paragraph(&mut document, 0, "he".len().."heyll".len());
  assert_eq!(paragraph_text(&document, 0), "heo");
  assert_eq!(document.paragraphs[0].runs.len(), 1);
  assert_eq!(document.paragraphs[0].runs[0].styles, RunStyles::default());
}

#[test]
#[hotpath::measure]
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
#[hotpath::measure]
fn multi_paragraph_rich_paste_preserves_first_pasted_paragraph_style() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    }],
  );
  let fragment = RichClipboardFragment {
    format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
    paragraphs: vec![
      InputParagraph {
        style: ParagraphStyle::Custom(1),
        runs: vec![plain("Heading")],
      },
      InputParagraph {
        style: ParagraphStyle::Custom(3),
        runs: vec![plain("Tag")],
      },
    ],
    blocks: Vec::new(),
    assets: Vec::new(),
  };

  insert_rich_fragment_at(&mut document, DocumentOffset::default(), &fragment);

  assert_eq!(paragraph_text(&document, 0), "Heading");
  assert_eq!(document.paragraphs[0].style, ParagraphStyle::Custom(1));
  assert_eq!(paragraph_text(&document, 1), "Tag");
  assert_eq!(document.paragraphs[1].style, ParagraphStyle::Custom(3));
}

#[test]
#[hotpath::measure]
fn single_paragraph_rich_paste_preserves_pasted_paragraph_style() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    }],
  );
  let fragment = RichClipboardFragment {
    format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
    paragraphs: vec![InputParagraph {
      style: ParagraphStyle::Custom(2),
      runs: vec![plain("Block")],
    }],
    blocks: Vec::new(),
    assets: Vec::new(),
  };

  insert_rich_fragment_at(&mut document, DocumentOffset::default(), &fragment);

  assert_eq!(paragraph_text(&document, 0), "Block");
  assert_eq!(document.paragraphs[0].style, ParagraphStyle::Custom(2));
}

#[test]
#[hotpath::measure]
fn layout_fragments_preserve_text_when_run_boundary_splits_utf8_character() {
  let text = "state\u{2019}s overconfidence";
  let split_inside_apostrophe = "state".len() + 1;
  let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
  let paragraph = Paragraph {
    style: ParagraphStyle::Normal,
    byte_range: 0..text.len(),
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
