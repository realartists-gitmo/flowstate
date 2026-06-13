#[cfg(test)]
mod tests {
  use flowstate_collab::binding::DocBinding;
  use flowstate_collab::{SessionId, projection, schema};
  use gpui_flowtext::{
    Block, Document, DocumentTheme, InputBlock, InputEquationBlock, InputEquationDisplay, InputEquationSyntax, InputParagraph, InputTableBlock,
    InputTableCell, InputTableCellBlock, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RunStyle, RunStyles,
    document_from_input_blocks, paragraph_text, plain, run,
  };

  const MULTIBYTE: &str = "aé🌍\u{2028}x";

  #[test]
  fn projection_round_trips_text_marks_and_equation_blocks() {
    let input_blocks = vec![
      InputBlock::Paragraph(InputParagraph {
        style: ParagraphStyle::Custom(2),
        runs: vec![
          plain("aé"),
          run(
            "🌍\u{2028}x",
            RunStyles::default()
              .with(RunStyle::Semantic(3))
              .with_direct_underline(),
          ),
        ],
      }),
      InputBlock::Equation(InputEquationBlock {
        source: "E = mc^2".to_string(),
        syntax: InputEquationSyntax::Latex,
        display: InputEquationDisplay::Display,
      }),
      InputBlock::Table(InputTableBlock {
        rows: vec![InputTableRow {
          cells: vec![InputTableCell {
            blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
              style: ParagraphStyle::Normal,
              runs: vec![plain(MULTIBYTE)],
            })],
            row_span: 1,
            col_span: 1,
          }],
        }],
        column_widths: vec![InputTableColumnWidth::Auto],
        style: InputTableStyle { header_row: true },
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

  #[test]
  fn offset_helpers_round_trip_all_multibyte_boundaries() {
    let original = document_from_input_blocks(
      DocumentTheme::default(),
      vec![InputBlock::Paragraph(InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain(MULTIBYTE)],
      })],
    );
    let loro = schema::new_configured_doc();
    projection::populate_from_document(&loro, SessionId::from_bytes([8; 32]), "offsets", &original).expect("populate should succeed");
    let binding = DocBinding::build(&loro, &original).expect("binding should build");
    let text = binding.rows[0]
      .text
      .as_ref()
      .expect("first row should be paragraph text");
    let byte_to_unicode = [
      (0, 0),
      ("a".len(), 1),
      ("aé".len(), 2),
      ("aé🌍".len(), 3),
      ("aé🌍\u{2028}".len(), 4),
      (MULTIBYTE.len(), 5),
    ];

    assert_eq!(text.len_utf8(), MULTIBYTE.len());
    for (byte, unicode) in byte_to_unicode {
      assert_eq!(schema::loro_pos(text, byte), unicode);
      assert_eq!(schema::utf8_byte(text, unicode), byte);
    }
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
      match (left, right) {
        (Block::Paragraph(_), Block::Paragraph(_)) => {},
        (Block::Equation(left), Block::Equation(right)) => {
          assert_eq!(left.source, right.source);
          assert_eq!(left.syntax, right.syntax);
          assert_eq!(left.display, right.display);
        },
        (Block::Table(left), Block::Table(right)) => {
          assert_eq!(left.rows, right.rows);
          assert_eq!(left.column_widths, right.column_widths);
          assert_eq!(left.style, right.style);
        },
        (Block::Image(left), Block::Image(right)) => {
          assert_eq!(left.asset_id, right.asset_id);
          assert_eq!(left.alt_text, right.alt_text);
          assert_eq!(left.caption, right.caption);
          assert_eq!(left.sizing, right.sizing);
          assert_eq!(left.alignment, right.alignment);
        },
        _ => panic!("projected block kind changed"),
      }
    }
  }
}
