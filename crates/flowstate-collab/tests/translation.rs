#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::{SessionId, binding::DocBinding, local_apply::LocalApplier, projection, schema};
  use gpui_flowtext::{
    Block, CanonicalOperation, Document, DocumentOffset, DocumentTheme, InputBlock, InputEquationBlock, InputEquationDisplay,
    InputEquationSyntax, InputParagraph, ParagraphStyle, RunStyle, RunStyles, block_from_input_block, capture_document_span,
    delete_cross_paragraph_range, delete_range_in_paragraph, document_from_input_blocks, insert_block_id, insert_text_at, mutate_runs_in_range,
    paragraph_text, paragraphs_mut, plain, remove_block_ids, run, split_paragraph_at, update_paragraph_block,
  };
  use loro::LoroDoc;

  const MULTIBYTE: &str = "aé🌍\u{2028}x";

  #[test]
  fn delete_range_uses_utf8_byte_offsets() {
    let (mut document, loro, mut binding) = collab_fixture(vec![paragraph_block(vec![plain(MULTIBYTE)])]);
    let paragraph = document.ids.paragraph_ids[0];
    let start = "a".len();
    let end = "aé🌍".len();

    delete_range_in_paragraph(&mut document, 0, start..end);
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::DeleteRange {
        start_paragraph: paragraph,
        start_byte: start,
        end_paragraph: paragraph,
        end_byte: end,
      }],
    );
  }

  #[test]
  fn split_and_join_paragraphs_preserve_multibyte_text_and_styles() {
    let semantic = RunStyles::default()
      .with(RunStyle::Semantic(2))
      .with_direct_underline();
    let (mut document, loro, mut binding) = collab_fixture(vec![paragraph_block(vec![plain("aé"), run("🌍\u{2028}x", semantic)])]);
    let original = document.ids.paragraph_ids[0];
    let split_byte = "aé".len();

    split_paragraph_at(&mut document, 0, split_byte);
    let split = document.ids.paragraph_ids[1];
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::SplitParagraph {
        paragraph: original,
        byte: split_byte,
        new_paragraph: split,
      }],
    );

    let first_len = paragraph_text(&document, 0).len();
    delete_cross_paragraph_range(
      &mut document,
      DocumentOffset {
        paragraph: 0,
        byte: first_len,
      }..DocumentOffset { paragraph: 1, byte: 0 },
    );
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::JoinParagraphs {
        first: original,
        second: split,
      }],
    );
  }

  #[test]
  fn paragraph_and_run_style_updates_match_flowtext_runs() {
    let base_style = RunStyles::default()
      .with(RunStyle::Semantic(1))
      .with_strikethrough();
    let (mut document, loro, mut binding) = collab_fixture(vec![paragraph_block(vec![run(MULTIBYTE, base_style)])]);
    let paragraph = document.ids.paragraph_ids[0];

    paragraphs_mut(&mut document)[0].style = ParagraphStyle::Custom(6);
    update_paragraph_block(&mut document, 0);
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::SetParagraphStyle {
        paragraph,
        style: ParagraphStyle::Custom(6),
      }],
    );

    let unstyled = "a".len().."aé".len();
    mutate_runs_in_range(
      &mut document,
      DocumentOffset {
        paragraph: 0,
        byte: unstyled.start,
      }..DocumentOffset {
        paragraph: 0,
        byte: unstyled.end,
      },
      |styles| *styles = RunStyles::default(),
    );
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::SetRunStyles {
        paragraph,
        range: unstyled,
        styles: RunStyles::default(),
      }],
    );

    let highlighted = "aé".len().."aé🌍".len();
    let highlighted_styles = RunStyles::default()
      .with(RunStyle::Semantic(4))
      .with(RunStyle::Highlight(3))
      .with_direct_underline()
      .with_strikethrough();
    mutate_runs_in_range(
      &mut document,
      DocumentOffset {
        paragraph: 0,
        byte: highlighted.start,
      }..DocumentOffset {
        paragraph: 0,
        byte: highlighted.end,
      },
      |styles| *styles = highlighted_styles,
    );
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::SetRunStyles {
        paragraph,
        range: highlighted,
        styles: highlighted_styles,
      }],
    );
  }

  #[test]
  fn replace_paragraph_span_refreshes_text_runs_and_styles() {
    let (mut document, loro, mut binding) =
      collab_fixture(vec![paragraph_block(vec![plain(MULTIBYTE)]), paragraph_block(vec![plain("second aé")])]);
    let start_paragraph = document.ids.paragraph_ids[0];
    let before = capture_document_span(&document, 0..2);
    let inserted_styles = RunStyles::default()
      .with(RunStyle::Semantic(5))
      .with_direct_underline();

    insert_text_at(&mut document, 0, "a".len(), "Zé", inserted_styles);
    paragraphs_mut(&mut document)[1].style = ParagraphStyle::Custom(2);
    update_paragraph_block(&mut document, 1);
    let after = capture_document_span(&document, 0..2);

    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph: Some(start_paragraph),
        before,
        after,
      }],
    );
  }

  #[test]
  fn insert_move_replace_and_delete_object_blocks_match_flowtext_document() {
    let (mut document, loro, mut binding) = collab_fixture(vec![paragraph_block(vec![plain("one aé")]), paragraph_block(vec![plain("two 🌍")])]);
    let equation = equation_block("x = aé + 🌍");

    let equation_id = insert_object_block(&mut document, 1, equation);
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::InsertBlock {
        block: equation_id,
        block_ix: 1,
      }],
    );

    move_block(&mut document, 1, 2);
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::MoveBlock {
        block: equation_id,
        new_block_ix: 2,
      }],
    );

    replace_object_block(&mut document, 2, equation_block("y = 🌍^2"));
    apply_and_assert_projection(
      &document,
      &loro,
      &mut binding,
      &[CanonicalOperation::ReplaceBlock { block: Some(equation_id) }],
    );

    delete_object_block(&mut document, 2);
    apply_and_assert_projection(&document, &loro, &mut binding, &[CanonicalOperation::DeleteBlock { block: equation_id }]);
  }

  #[test]
  fn replace_document_rebuilds_the_loro_projection() {
    let (_, loro, mut binding) = collab_fixture(vec![paragraph_block(vec![plain("old")]), equation_block("old = 1")]);
    let document = document_from_input_blocks(
      DocumentTheme::default(),
      vec![
        paragraph_block(vec![plain("new aé"), run("🌍", RunStyles::default().with(RunStyle::Highlight(1)))]),
        equation_block("new = aé + 🌍"),
        paragraph_block(vec![plain("tail")]),
      ],
    );

    apply_and_assert_projection(&document, &loro, &mut binding, &[CanonicalOperation::ReplaceDocument]);
  }

  fn collab_fixture(blocks: Vec<InputBlock>) -> (Document, LoroDoc, DocBinding) {
    let document = document_from_input_blocks(DocumentTheme::default(), blocks);
    let loro = schema::new_configured_doc();
    projection::populate_from_document(&loro, SessionId::from_bytes([42; 32]), "translation", &document).expect("populate should succeed");
    let binding = DocBinding::build(&loro, &document).expect("binding should build");
    (document, loro, binding)
  }

  fn apply_and_assert_projection(document: &Document, loro: &LoroDoc, binding: &mut DocBinding, ops: &[CanonicalOperation]) {
    LocalApplier { doc: loro, binding }
      .apply(document, ops)
      .expect("local apply should succeed");
    let projected = projection::document_from_loro(loro, DocumentTheme::default()).expect("projection should succeed");
    assert_document_projection_eq(document, &projected);
  }

  fn paragraph_block(runs: Vec<gpui_flowtext::InputRun>) -> InputBlock {
    InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs,
    })
  }

  fn equation_block(source: &str) -> InputBlock {
    InputBlock::Equation(InputEquationBlock {
      source: source.to_string(),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })
  }

  fn insert_object_block(document: &mut Document, block_ix: usize, input: InputBlock) -> gpui_flowtext::BlockId {
    Arc::make_mut(&mut document.blocks).insert(block_ix, block_from_input_block(&input));
    insert_block_id(document, block_ix)
  }

  fn move_block(document: &mut Document, from: usize, to: usize) {
    let blocks = Arc::make_mut(&mut document.blocks);
    let block = blocks.remove(from);
    blocks.insert(to.min(blocks.len()), block);
    let block_id = document.ids.block_ids.remove(from);
    document
      .ids
      .block_ids
      .insert(to.min(document.ids.block_ids.len()), block_id);
  }

  fn replace_object_block(document: &mut Document, block_ix: usize, input: InputBlock) {
    Arc::make_mut(&mut document.blocks)[block_ix] = block_from_input_block(&input);
  }

  fn delete_object_block(document: &mut Document, block_ix: usize) {
    Arc::make_mut(&mut document.blocks).remove(block_ix);
    remove_block_ids(document, block_ix..block_ix + 1);
  }

  fn assert_document_projection_eq(expected: &Document, actual: &Document) {
    assert_eq!(expected.blocks.len(), actual.blocks.len());
    assert_eq!(expected.paragraphs.len(), actual.paragraphs.len());
    let mut paragraph_ix = 0;
    for (expected_block, actual_block) in expected.blocks.iter().zip(actual.blocks.iter()) {
      match (expected_block, actual_block) {
        (Block::Paragraph(expected_paragraph), Block::Paragraph(actual_paragraph)) => {
          assert_eq!(paragraph_text(expected, paragraph_ix), paragraph_text(actual, paragraph_ix));
          assert_eq!(expected_paragraph.style, actual_paragraph.style);
          assert_eq!(expected_paragraph.runs, actual_paragraph.runs);
        },
        (Block::Equation(expected), Block::Equation(actual)) => {
          assert_eq!(expected.source, actual.source);
          assert_eq!(expected.syntax, actual.syntax);
          assert_eq!(expected.display, actual.display);
        },
        (Block::Image(expected), Block::Image(actual)) => {
          assert_eq!(expected.asset_id, actual.asset_id);
          assert_eq!(expected.alt_text, actual.alt_text);
          assert_eq!(expected.caption, actual.caption);
          assert_eq!(expected.sizing, actual.sizing);
          assert_eq!(expected.alignment, actual.alignment);
        },
        (Block::Table(expected), Block::Table(actual)) => {
          assert_eq!(expected.rows, actual.rows);
          assert_eq!(expected.column_widths, actual.column_widths);
          assert_eq!(expected.style, actual.style);
        },
        _ => panic!("projected block kind changed"),
      }
      if matches!(expected_block, Block::Paragraph(_)) {
        paragraph_ix += 1;
      }
    }
  }
}
