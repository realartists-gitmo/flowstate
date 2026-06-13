#[cfg(test)]
mod tests {
  use flowstate_collab::{SessionId, binding::DocBinding, local_apply::LocalApplier, projection, schema};
  use gpui_flowtext::{
    CanonicalOperation, Document, DocumentTheme, InputBlock, InputParagraph, ParagraphStyle, RunStyle, RunStyles, insert_text_at,
    paragraph_text, plain,
  };

  #[test]
  fn local_apply_insert_text_matches_flowtext_edit_op() {
    let mut document = gpui_flowtext::document_from_input_blocks(
      DocumentTheme::default(),
      vec![InputBlock::Paragraph(InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("aé🌍\u{2028}x")],
      })],
    );
    let loro = schema::new_configured_doc();
    projection::populate_from_document(&loro, SessionId::from_bytes([9; 32]), "local-apply", &document).expect("populate should succeed");
    let mut binding = DocBinding::build(&loro, &document).expect("binding should build");
    let paragraph = document.ids.paragraph_ids[0];
    let styles = RunStyles::default()
      .with(RunStyle::Semantic(4))
      .with_strikethrough();

    insert_text_at(&mut document, 0, "a".len(), "Z", styles);
    LocalApplier {
      doc: &loro,
      binding: &mut binding,
    }
    .apply(
      &document,
      &[CanonicalOperation::InsertText {
        paragraph,
        byte: "a".len(),
        text: "Z".to_string(),
        styles,
      }],
    )
    .expect("local apply should succeed");

    let projected = projection::document_from_loro(&loro, DocumentTheme::default()).expect("projection should succeed");
    assert_document_text_and_runs_eq(&document, &projected);
  }

  fn assert_document_text_and_runs_eq(left: &Document, right: &Document) {
    assert_eq!(left.paragraphs.len(), right.paragraphs.len());
    for ix in 0..left.paragraphs.len() {
      assert_eq!(paragraph_text(left, ix), paragraph_text(right, ix));
      assert_eq!(left.paragraphs[ix].runs, right.paragraphs[ix].runs);
    }
  }
}
