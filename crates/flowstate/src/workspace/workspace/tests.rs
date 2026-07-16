#[cfg(test)]
mod tests {
  use super::*;
  use crate::rich_text_element::{DocumentParagraphInput, DocumentRunInput, RunStyles, document_from_paragraphs};

  #[hotpath::measure]
  fn paragraph(style: ParagraphStyle, text: &str) -> DocumentParagraphInput {
    DocumentParagraphInput {
      style,
      runs: vec![DocumentRunInput {
        text: text.to_string(),
        styles: RunStyles::default(),
      }],
    }
  }

  #[test]
  #[hotpath::measure]
  fn ordered_document_tabs_moves_pins_left_in_pin_order() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let third = Uuid::new_v4();
    let tabs = vec![
      DocumentTab {
        id: first,
        label: "first".into(),
        active: false,
        pinned: false,
        pin_index: None,
        speech: false,
        dirty: false,
      },
      DocumentTab {
        id: second,
        label: "second".into(),
        active: false,
        pinned: false,
        pin_index: None,
        speech: false,
        dirty: false,
      },
      DocumentTab {
        id: third,
        label: "third".into(),
        active: false,
        pinned: false,
        pin_index: None,
        speech: false,
        dirty: false,
      },
    ];

    let ordered = ordered_document_tabs(tabs, &[third, first]);

    assert_eq!(ordered.iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![third, first, second]);
    assert_eq!(ordered[0].pin_index, Some(0));
    assert_eq!(ordered[1].pin_index, Some(1));
    assert_eq!(ordered[2].pin_index, None);
    assert_eq!(pin_shortcut_label(0), Some("1"));
    assert_eq!(pin_shortcut_label(9), Some("0"));
  }

  #[test]
  #[hotpath::measure]
  fn outline_label_normalizes_whitespace_without_full_join() {
    let document = document_from_paragraphs(
      DocumentTheme::default(),
      vec![paragraph(flowstate_document::PARAGRAPH_POCKET, "  alpha\t beta\n\n gamma  ")],
    );

    assert_eq!(outline_paragraph_label(&document, 0), "alpha beta gamma");
  }

  #[test]
  #[hotpath::measure]
  fn active_visible_outline_uses_latest_visible_heading_before_caret() {
    let document = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(flowstate_document::PARAGRAPH_HAT, "Child"),
        paragraph(ParagraphStyle::Normal, "Body"),
        paragraph(flowstate_document::PARAGRAPH_BLOCK, "Grandchild"),
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Next"),
      ],
    );
    let nodes = outline_nodes(&document);
    let mut collapsed = HashSet::new();
    collapsed.insert(0);
    let mut visible = Vec::new();
    collect_visible_outline_paragraphs(&nodes, &collapsed, &mut visible);

    assert_eq!(visible, vec![0, 4]);
    assert_eq!(active_visible_outline_paragraph_from_visible(&visible, 3), Some(0));
    assert_eq!(active_visible_outline_paragraph_from_visible(&visible, 4), Some(4));
  }

  #[test]
  #[hotpath::measure]
  fn outline_signature_ignores_non_outline_text_edits() {
    let before = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let after = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body with more plain text"),
      ],
    );

    assert!(outline_signature(&before) == outline_signature(&after));
  }

  #[test]
  #[hotpath::measure]
  fn outline_signature_tracks_outline_labels_and_paragraph_count() {
    let before = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let renamed = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Renamed"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let appended = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
        paragraph(ParagraphStyle::Normal, "More body"),
      ],
    );

    assert!(outline_signature(&before) != outline_signature(&renamed));
    assert!(outline_signature(&before) != outline_signature(&appended));
  }
}
