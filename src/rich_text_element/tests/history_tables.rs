
#[test]
#[hotpath::measure]
fn block_delete_operation_undo_redo_preserves_non_text_block() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let equation = Block::Equation(EquationBlock {
    source: "a^2+b^2=c^2".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  });
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), equation.clone()]);

  let op = EditOperation::DeleteBlock {
    block_ix: 1,
    block: equation,
  };
  op.redo(&mut document);
  assert_eq!(document.blocks.len(), 1);
  op.undo(&mut document);
  assert_eq!(document.blocks.len(), 2);
  assert!(matches!(document.blocks[1], Block::Equation(_)));
}

#[test]
#[hotpath::measure]
fn insert_blocks_operation_undo_redo_preserves_inserted_table_and_equation() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let blocks = vec![
    Block::Table(TableBlock {
      rows: vec![TableRow {
        cells: vec![TableCell {
          blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
            paragraph: Paragraph {
              style: ParagraphStyle::Normal,
              byte_range: 0..0,
              runs: Vec::new(),
              version: 0,
            },
            text: String::new(),
          })],
          row_span: 1,
          col_span: 1,
        }],
      }],
      column_widths: vec![TableColumnWidth::Fraction(1)],
      style: TableStyle { header_row: false },
      version: 0,
    }),
    Block::Equation(EquationBlock {
      source: "x=1".into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    }),
  ];

  let op = EditOperation::InsertBlocks {
    block_ix: 1,
    blocks: blocks.clone(),
  };
  op.redo(&mut document);
  assert_eq!(document.blocks.len(), 3);
  assert!(matches!(document.blocks[1], Block::Table(_)));
  assert!(matches!(document.blocks[2], Block::Equation(_)));
  op.undo(&mut document);
  assert_eq!(document.blocks.len(), 1);
  op.redo(&mut document);
  assert_eq!(document.blocks.len(), 3);
}

#[test]
#[hotpath::measure]
fn replace_block_operation_undo_redo_preserves_table_shape_changes() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let before = Block::Table(TableBlock {
    rows: vec![TableRow {
      cells: vec![TableCell {
        blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
          paragraph: Paragraph {
            style: ParagraphStyle::Normal,
            byte_range: 0..0,
            runs: Vec::new(),
            version: 0,
          },
          text: String::new(),
        })],
        row_span: 1,
        col_span: 1,
      }],
    }],
    column_widths: vec![TableColumnWidth::Fraction(1)],
    style: TableStyle { header_row: false },
    version: 0,
  });
  let mut after = before.clone();
  let Block::Table(table) = &mut after else {
    unreachable!();
  };
  table.rows.push(table.rows[0].clone());
  table.version = 1;
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), before.clone()]);

  let op = EditOperation::ReplaceBlock { block_ix: 1, before, after };
  op.redo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  assert_eq!(table.rows.len(), 2);
  op.undo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  assert_eq!(table.rows.len(), 1);
}

#[test]
#[hotpath::measure]
fn table_cell_text_edit_is_a_replace_block_history_operation() {
  let before = Block::Table(TableBlock {
    rows: vec![TableRow {
      cells: vec![TableCell {
        blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
          paragraph: Paragraph {
            style: ParagraphStyle::Normal,
            byte_range: 0..0,
            runs: Vec::new(),
            version: 0,
          },
          text: String::new(),
        })],
        row_span: 1,
        col_span: 1,
      }],
    }],
    column_widths: vec![TableColumnWidth::Fraction(1)],
    style: TableStyle { header_row: false },
    version: 0,
  });
  let mut after = before.clone();
  let Block::Table(table) = &mut after else {
    unreachable!();
  };
  let TableCellBlock::Paragraph(paragraph) = &mut table.rows[0].cells[0].blocks[0] else {
    unreachable!();
  };
  paragraph.text = "cell".to_string();
  paragraph.paragraph.byte_range = 0.."cell".len();
  paragraph.paragraph.runs = vec![TextRun {
    len: "cell".len(),
    styles: RunStyles::default(),
  }];
  table.version = 1;

  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), before.clone()]);
  let op = EditOperation::ReplaceBlock { block_ix: 1, before, after };
  op.redo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  let TableCellBlock::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("expected paragraph");
  };
  assert_eq!(paragraph.text, "cell");
  op.undo(&mut document);
  let Block::Table(table) = &document.blocks[1] else {
    panic!("expected table");
  };
  let TableCellBlock::Paragraph(paragraph) = &table.rows[0].cells[0].blocks[0] else {
    panic!("expected paragraph");
  };
  assert!(paragraph.text.is_empty());
}

#[test]
#[hotpath::measure]
fn replace_block_operation_undo_redo_preserves_equation_source_changes() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let before = Block::Equation(EquationBlock {
    source: "x".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 0,
  });
  let after = Block::Equation(EquationBlock {
    source: "x+1".into(),
    syntax: EquationSyntax::Latex,
    display: EquationDisplay::Display,
    version: 1,
  });
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), before.clone()]);
  let op = EditOperation::ReplaceBlock { block_ix: 1, before, after };
  op.redo(&mut document);
  let Block::Equation(equation) = &document.blocks[1] else {
    panic!("expected equation");
  };
  assert_eq!(equation.source.as_ref(), "x+1");
  op.undo(&mut document);
  let Block::Equation(equation) = &document.blocks[1] else {
    panic!("expected equation");
  };
  assert_eq!(equation.source.as_ref(), "x");
}

#[test]
#[hotpath::measure]
fn default_inserted_table_shape_round_trips_through_db8() {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  let table = Block::Table(TableBlock {
    rows: (0..2)
      .map(|_| TableRow {
        cells: (0..2)
          .map(|_| TableCell {
            blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
              paragraph: Paragraph {
                style: ParagraphStyle::Normal,
                byte_range: 0..0,
                runs: Vec::new(),
                version: 0,
              },
              text: String::new(),
            })],
            row_span: 1,
            col_span: 1,
          })
          .collect(),
      })
      .collect(),
    column_widths: vec![TableColumnWidth::Fraction(1), TableColumnWidth::Fraction(1)],
    style: TableStyle { header_row: false },
    version: 0,
  });
  document.blocks = std::sync::Arc::new(vec![Block::Paragraph(document.paragraphs[0].clone()), table]);

  let path = std::env::temp_dir().join(format!("flowstate-default-table-{}.db8", uuid::Uuid::new_v4()));
  write_db8(&path, &document).unwrap();
  let loaded = read_db8(&path).unwrap();
  let _ = std::fs::remove_file(path);

  let Block::Table(table) = &loaded.blocks[1] else {
    panic!("expected table block");
  };
  assert_eq!(table.rows.len(), 2);
  assert!(table.rows.iter().all(|row| row.cells.len() == 2));
  assert_eq!(table.column_widths.len(), 2);
}

#[test]
#[hotpath::measure]
fn table_cell_paragraph_clipboard_conversion_preserves_text_and_styles() {
  let styles = RunStyles::default().with(RunStyle::Emphasis);
  let paragraph = InputParagraph {
    style: ParagraphStyle::Tag,
    runs: vec![InputRun {
      text: "cell text".to_string(),
      styles,
    }],
  };
  let cell = table_cell_paragraph_from_input_paragraph(&paragraph);
  assert_eq!(cell.text, "cell text");
  assert_eq!(cell.paragraph.style, ParagraphStyle::Tag);
  assert_eq!(cell.paragraph.runs[0].styles, styles);

  let restored = input_paragraph_from_table_cell_paragraph(&cell);
  assert_eq!(input_paragraph_text(&restored), "cell text");
  assert_eq!(restored.style, ParagraphStyle::Tag);
  assert_eq!(restored.runs[0].styles, styles);
}

#[test]
#[hotpath::measure]
fn splitting_table_cell_paragraph_preserves_text_and_run_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut cell = TableCell {
    blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
      paragraph: Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0.."alpha beta".len(),
        runs: vec![
          TextRun {
            len: "alpha ".len(),
            styles: RunStyles::default(),
          },
          TextRun {
            len: "beta".len(),
            styles: emphasized,
          },
        ],
        version: 0,
      },
      text: "alpha beta".to_string(),
    })],
    row_span: 1,
    col_span: 1,
  };

  let new_ix = split_table_cell_paragraph_at(&mut cell, 0, "alpha ".len()).unwrap();
  assert_eq!(new_ix, 1);

  let TableCellBlock::Paragraph(left) = &cell.blocks[0] else {
    panic!("expected left paragraph");
  };
  let TableCellBlock::Paragraph(right) = &cell.blocks[1] else {
    panic!("expected right paragraph");
  };

  assert_eq!(left.text, "alpha ");
  assert_eq!(right.text, "beta");
  assert_eq!(left.paragraph.runs[0].styles, RunStyles::default());
  assert_eq!(right.paragraph.runs[0].styles, emphasized);
  assert_eq!(left.paragraph.byte_range, 0.."alpha ".len());
  assert_eq!(right.paragraph.byte_range, 0.."beta".len());
}

#[test]
#[hotpath::measure]
fn merging_table_cell_paragraphs_preserves_boundary_caret_and_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let mut cell = TableCell {
    blocks: vec![
      TableCellBlock::Paragraph(TableCellParagraph {
        paragraph: Paragraph {
          style: ParagraphStyle::Normal,
          byte_range: 0.."left".len(),
          runs: vec![TextRun {
            len: "left".len(),
            styles: RunStyles::default(),
          }],
          version: 0,
        },
        text: "left".to_string(),
      }),
      TableCellBlock::Paragraph(TableCellParagraph {
        paragraph: Paragraph {
          style: ParagraphStyle::Normal,
          byte_range: 0.."right".len(),
          runs: vec![TextRun {
            len: "right".len(),
            styles: emphasized,
          }],
          version: 0,
        },
        text: "right".to_string(),
      }),
    ],
    row_span: 1,
    col_span: 1,
  };

  let (paragraph_ix, caret) = merge_table_cell_paragraph_with_previous(&mut cell, 1).unwrap();
  assert_eq!((paragraph_ix, caret), (0, "left".len()));
  assert_eq!(cell.blocks.len(), 1);

  let TableCellBlock::Paragraph(merged) = &cell.blocks[0] else {
    panic!("expected merged paragraph");
  };
  assert_eq!(merged.text, "leftright");
  assert_eq!(merged.paragraph.runs.len(), 2);
  assert_eq!(merged.paragraph.runs[0].styles, RunStyles::default());
  assert_eq!(merged.paragraph.runs[1].styles, emphasized);
}

#[test]
#[hotpath::measure]
fn inserting_rich_paragraphs_into_table_cell_preserves_tail_and_styles() {
  let emphasized = RunStyles::default().with(RunStyle::Emphasis);
  let cite = RunStyles::default().with(RunStyle::Cite);
  let mut cell = TableCell {
    blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
      paragraph: Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0.."alpha omega".len(),
        runs: vec![
          TextRun {
            len: "alpha ".len(),
            styles: RunStyles::default(),
          },
          TextRun {
            len: "omega".len(),
            styles: emphasized,
          },
        ],
        version: 0,
      },
      text: "alpha omega".to_string(),
    })],
    row_span: 1,
    col_span: 1,
  };

  let inserted = vec![
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "B".to_string(),
        styles: cite,
      }],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "C".to_string(),
        styles: cite,
      }],
    },
  ];

  let caret = insert_table_cell_paragraphs_at(&mut cell, 0, "alpha ".len(), &inserted).unwrap();
  assert_eq!(caret, (1, "C".len()));
  assert_eq!(cell.blocks.len(), 2);

  let TableCellBlock::Paragraph(first) = &cell.blocks[0] else {
    panic!("expected first paragraph");
  };
  let TableCellBlock::Paragraph(second) = &cell.blocks[1] else {
    panic!("expected second paragraph");
  };
  assert_eq!(first.text, "alpha B");
  assert_eq!(second.text, "Comega");
  assert_eq!(first.paragraph.runs.last().unwrap().styles, cite);
  assert_eq!(second.paragraph.runs[0].styles, cite);
  assert_eq!(second.paragraph.runs[1].styles, emphasized);
}
