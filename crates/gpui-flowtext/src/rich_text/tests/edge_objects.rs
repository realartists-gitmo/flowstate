// B-S1: select-all + copy must keep DOCUMENT-EDGE objects. Text selections
// live in paragraph space, so a leading/trailing image or equation could never
// satisfy the old strictly-between fragment test — copy/cut silently dropped
// them.

#[gpui::test]
fn select_all_copy_keeps_edge_objects(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "middle paragraph".into(),
        styles: RunStyles::default(),
      }],
    }],
  );
  fn equation(source: &str) -> Block {
    Block::Equation(EquationBlock {
      source: source.to_string().into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    })
  }
  let paragraph_block = document.blocks.get(0).expect("paragraph block").clone();
  document.blocks = BlockSeq::from_vec(vec![equation("lead"), paragraph_block, equation("tail")]);
  document.ids = document_ids_for_shape(1, 3);
  rebuild_document_sections(&mut document);

  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(document, None, cx));
  let fragment = handle
    .update(cx, |editor, _window, cx| {
      editor.select_all(cx);
      editor.copy(cx);
      cx.read_from_clipboard()
        .and_then(|item| item.metadata().cloned())
        .and_then(|metadata| serde_json::from_str::<RichClipboardFragment>(&metadata).ok())
    })
    .expect("window update")
    .expect("copy must write a rich fragment when objects are in range");

  let equations = fragment
    .blocks
    .iter()
    .filter(|block| matches!(block, InputBlock::Equation(_)))
    .count();
  assert_eq!(equations, 2, "both document-edge equations must survive select-all copy");
  assert!(
    fragment
      .blocks
      .iter()
      .any(|block| matches!(block, InputBlock::Paragraph(_))),
    "the paragraph travels too"
  );
}

// B-S6: structure DESCENDS — a heading style inside a table cell roots a
// real outline node, addressed by its cell, interleaved at the table's
// document position, and never parenting body content.
#[test]
fn outline_descends_into_table_cells() {
  let mut theme = DocumentTheme::default();
  theme.set_custom_paragraph_style(
    0,
    CustomParagraphStyle {
      font_size: px(20.0),
      font_family: None,
      color: gpui::rgb(0x0000_0000).into(),
      bold: true,
      italic: false,
      underline: ThemeUnderline::None,
      align: CustomParagraphAlign::Left,
      spacing_before: px(0.0),
      spacing_after: px(0.0),
      border: None,
      section_kind: Some(0),
      section_level: Some(1),
    },
  );

  let cell_paragraph = TableCellParagraph {
    paragraph: Paragraph {
      style: ParagraphStyle::Custom(0),
      runs: vec![TextRun {
        len: "CELL POCKET".len(),
        styles: RunStyles::default(),
      }],
      version: 0,
    },
    text: "CELL POCKET".to_string(),
  };
  let table = Block::Table(TableBlock {
    rows: vec![TableRow {
      id: RowId(1),
      cells: vec![TableCell {
        id: CellId::from_coordinate(RowId(1), ColumnId(2)),
        row_id: RowId(1),
        column_id: ColumnId(2),
        blocks: vec![TableCellBlock::Paragraph(cell_paragraph)],
        row_span: 1,
        col_span: 1,
      }],
    }],
    columns: vec![TableColumn {
      id: ColumnId(2),
      width: TableColumnWidth::Auto,
    }],
    style: TableStyle { header_row: false },
    version: 0,
  });

  let mut document = document_from_input(
    theme,
    vec![
      InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![plain("BODY POCKET")],
      },
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("body text")],
      },
    ],
  );
  document.blocks.push(table);
  let mut block_ids = document.ids.block_ids.as_ref().clone();
  block_ids.push(BlockId(999));
  document.ids.block_ids = std::sync::Arc::new(block_ids);
  rebuild_document_outline_now(&mut document);

  let outline = document.outline.as_ref();
  assert_eq!(outline.len(), 2, "one body heading + one cell heading, got {outline:?}");
  let body = &outline[0];
  assert!(body.cell_address.is_none());
  let cell = &outline[1];
  let address = cell.cell_address.expect("cell node carries its address");
  assert_eq!(address.table_block, 999);
  assert_eq!((address.row_ix, address.cell_ix, address.cell_paragraph_ix), (0, 0, 0));
  assert_eq!(
    cell.parent_id,
    Some(body.id),
    "the cell heading nests as a LEAF under the enclosing body section"
  );
}
