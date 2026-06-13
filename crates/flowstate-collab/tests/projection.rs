#[cfg(test)]
mod tests {
  use flowstate_collab::binding::DocBinding;
  use flowstate_collab::{SessionId, projection, schema};
  use gpui_flowtext::{
    Document, DocumentTheme, InputBlock, InputEquationBlock, InputEquationDisplay, InputEquationSyntax, InputParagraph, ParagraphStyle, RunStyle,
    RunStyles, document_from_input_blocks, paragraph_text, plain, run,
  };

  #[test]
  fn projection_round_trips_text_marks_and_equation_blocks() {
    let input_blocks = vec![
      InputBlock::Paragraph(InputParagraph {
        style: ParagraphStyle::Custom(2),
        runs: vec![
          plain("aé"),
          run("🌍\u{2028}x", RunStyles::default().with(RunStyle::Semantic(3)).with_direct_underline()),
        ],
      }),
      InputBlock::Equation(InputEquationBlock {
        source: "E = mc^2".to_string(),
        syntax: InputEquationSyntax::Latex,
        display: InputEquationDisplay::Display,
      }),
    ];
    let original = document_from_input_blocks(DocumentTheme::default(), input_blocks);
    let loro = schema::new_configured_doc();
    let session = SessionId::from_bytes([7; 32]);

    projection::populate_from_document(&loro, session, "projection-test", &original).expect("populate should succeed");
    projection::verify_lineage(&loro, session).expect("lineage should match");
    let projected = projection::document_from_loro(&loro, DocumentTheme::default()).expect("projection should succeed");
    let binding = DocBinding::build(&loro, &projected).expect("binding should match projected document");

    assert_document_shape_eq(&original, &projected);
    assert_eq!(binding.rows.len(), projected.blocks.len());
  }

  fn assert_document_shape_eq(left: &Document, right: &Document) {
    assert_eq!(left.blocks.len(), right.blocks.len());
    assert_eq!(left.paragraphs.len(), right.paragraphs.len());
    for ix in 0..left.paragraphs.len() {
      assert_eq!(paragraph_text(left, ix), paragraph_text(right, ix));
      assert_eq!(left.paragraphs[ix].style, right.paragraphs[ix].style);
      assert_eq!(left.paragraphs[ix].runs, right.paragraphs[ix].runs);
    }
    for (left, right) in left.blocks.iter().zip(right.blocks.iter()) {
      assert_eq!(std::mem::discriminant(left), std::mem::discriminant(right));
    }
  }
}
