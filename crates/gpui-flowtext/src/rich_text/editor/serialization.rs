#[hotpath::measure]
pub(super) fn input_paragraph_text(paragraph: &InputParagraph) -> String {
  paragraph.runs.iter().map(|run| run.text.as_str()).collect()
}

#[hotpath::measure]
fn collect_block_assets(block: &Block, assets: &AssetStore, output: &mut Vec<InputAsset>) {
  match block {
    Block::Image(image) => {
      if let Some(asset) = assets.assets.get(&image.asset_id) {
        output.push(InputAsset {
          id: asset.id,
          mime_type: asset.mime_type.to_string(),
          original_name: asset.original_name.as_ref().map(ToString::to_string),
          content_hash: asset.content_hash,
          bytes: asset.bytes.as_ref().clone(),
        });
      }
    },
    Block::Table(table) => {
      for row in &table.rows {
        for cell in &row.cells {
          for block in &cell.blocks {
            if let TableCellBlock::Table(table) = block {
              collect_block_assets(&Block::Table(table.clone()), assets, output);
            }
          }
        }
      }
    },
    Block::Paragraph(_) | Block::Equation(_) => {},
  }
}

#[hotpath::measure]
pub fn input_block_from_block(block: &Block) -> InputBlock {
  match block {
    Block::Paragraph(paragraph) => InputBlock::Paragraph(input_paragraph_from_paragraph(paragraph)),
    Block::Image(image) => InputBlock::Image(InputImageBlock {
      asset_id: image.asset_id,
      alt_text: image.alt_text.to_string(),
      caption: image.caption.as_ref().map(input_paragraph_from_paragraph),
      sizing: match image.sizing {
        ImageSizing::Intrinsic => InputImageSizing::Intrinsic,
        ImageSizing::FitWidth => InputImageSizing::FitWidth,
        ImageSizing::Fixed { width_px, height_px } => InputImageSizing::Fixed { width_px, height_px },
      },
      alignment: input_alignment_from_alignment(image.alignment),
    }),
    Block::Equation(equation) => InputBlock::Equation(InputEquationBlock {
      source: equation.source.to_string(),
      syntax: InputEquationSyntax::Latex,
      display: match equation.display {
        EquationDisplay::Display => InputEquationDisplay::Display,
        EquationDisplay::InlineLikeParagraph => InputEquationDisplay::InlineLikeParagraph,
      },
    }),
    Block::Table(table) => InputBlock::Table(input_table_from_table(table)),
  }
}

#[hotpath::measure]
pub fn block_from_input_block(block: &InputBlock) -> Block {
  match block {
    InputBlock::Paragraph(paragraph) => Block::Paragraph(paragraph_from_input_paragraph(paragraph)),
    InputBlock::Image(image) => Block::Image(ImageBlock {
      asset_id: image.asset_id,
      alt_text: image.alt_text.clone().into(),
      caption: image.caption.as_ref().map(paragraph_from_input_paragraph),
      sizing: match image.sizing {
        InputImageSizing::Intrinsic => ImageSizing::Intrinsic,
        InputImageSizing::FitWidth => ImageSizing::FitWidth,
        InputImageSizing::Fixed { width_px, height_px } => ImageSizing::Fixed { width_px, height_px },
      },
      alignment: alignment_from_input_alignment(image.alignment),
      version: 0,
    }),
    InputBlock::Equation(equation) => Block::Equation(EquationBlock {
      source: equation.source.clone().into(),
      syntax: EquationSyntax::Latex,
      display: match equation.display {
        InputEquationDisplay::Display => EquationDisplay::Display,
        InputEquationDisplay::InlineLikeParagraph => EquationDisplay::InlineLikeParagraph,
      },
      version: 0,
    }),
    InputBlock::Table(table) => Block::Table(table_from_input_table(table)),
  }
}

#[hotpath::measure]
#[must_use]
pub fn document_from_input_blocks(theme: DocumentTheme, input_blocks: Vec<InputBlock>) -> Document {
  let mut text = String::new();
  let mut paragraphs = Vec::new();
  let mut blocks = Vec::with_capacity(input_blocks.len());

  for input_block in input_blocks {
    match input_block {
      InputBlock::Paragraph(paragraph) => {
        if !paragraphs.is_empty() {
          text.push('\n');
        }
        let start = text.len();
        text.push_str(&input_paragraph_text(&paragraph));
        let mut paragraph = paragraph_from_input_paragraph(&paragraph);
        paragraph.byte_range = start..text.len();
        paragraphs.push(paragraph.clone());
        blocks.push(Block::Paragraph(paragraph));
      },
      InputBlock::Image(image) => blocks.push(block_from_input_block(&InputBlock::Image(image))),
      InputBlock::Equation(equation) => blocks.push(block_from_input_block(&InputBlock::Equation(equation))),
      InputBlock::Table(table) => blocks.push(block_from_input_block(&InputBlock::Table(table))),
    }
  }

  if paragraphs.is_empty() {
    let paragraph = Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    };
    paragraphs.push(paragraph.clone());
    blocks.push(Block::Paragraph(paragraph));
  }

  let offset_index = ParagraphOffsetIndex::new(&paragraphs);
  let paragraph_count = paragraphs.len();
  let block_count = blocks.len();
  let mut document = Document {
    text: Rope::from(text),
    paragraphs: Arc::new(paragraphs),
    blocks: Arc::new(blocks),
    assets: AssetStore::default(),
    ids: document_ids_for_shape(paragraph_count, block_count),
    sections: Arc::new(Vec::new()),
    offset_index,
    theme,
  };
  reconcile_document_ids(&mut document);
  rebuild_document_sections(&mut document);
  document
}

#[hotpath::measure]
fn input_paragraph_from_paragraph(paragraph: &Paragraph) -> InputParagraph {
  InputParagraph {
    style: paragraph.style,
    runs: paragraph
      .runs
      .iter()
      .map(|run| InputRun {
        text: String::new(),
        styles: run.styles,
      })
      .collect(),
  }
}

#[hotpath::measure]
fn input_paragraph_from_document_range(document: &Document, paragraph_ix: usize, range: Range<usize>) -> InputParagraph {
  let paragraph = &document.paragraphs[paragraph_ix];
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  let start = range.start.min(paragraph_text_len(paragraph));
  let end = range.end.min(paragraph_text_len(paragraph)).max(start);
  let mut runs = Vec::new();
  let mut offset = 0;
  for run in &paragraph.runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    let clipped_start = run_start.max(start);
    let clipped_end = run_end.min(end);
    if clipped_start < clipped_end {
      runs.push(InputRun {
        text: document_text_slice(document, paragraph_range.start + clipped_start..paragraph_range.start + clipped_end),
        styles: run.styles,
      });
    }
  }
  InputParagraph {
    style: paragraph.style,
    runs,
  }
}

#[hotpath::measure]
pub(super) fn input_paragraph_from_table_cell_paragraph(paragraph: &TableCellParagraph) -> InputParagraph {
  let mut byte = 0;
  InputParagraph {
    style: paragraph.paragraph.style,
    runs: paragraph
      .paragraph
      .runs
      .iter()
      .map(|run| {
        let start = byte;
        let end = (start + run.len).min(paragraph.text.len());
        byte = end;
        InputRun {
          text: paragraph.text.get(start..end).unwrap_or("").to_string(),
          styles: run.styles,
        }
      })
      .collect(),
  }
}

#[hotpath::measure]
fn input_paragraph_from_table_cell_range(paragraph: &TableCellParagraph, range: Range<usize>) -> InputParagraph {
  let start = range.start.min(paragraph.text.len());
  let end = range.end.min(paragraph.text.len()).max(start);
  let mut runs = Vec::new();
  let mut byte = 0;
  for run in &paragraph.paragraph.runs {
    let run_start = byte;
    let run_end = run_start + run.len;
    byte = run_end;
    let clipped_start = run_start.max(start);
    let clipped_end = run_end.min(end);
    if clipped_start < clipped_end {
      runs.push(InputRun {
        text: paragraph
          .text
          .get(clipped_start..clipped_end)
          .unwrap_or("")
          .to_string(),
        styles: run.styles,
      });
    }
  }
  InputParagraph {
    style: paragraph.paragraph.style,
    runs,
  }
}

#[hotpath::measure]
fn paragraph_from_input_paragraph(paragraph: &InputParagraph) -> Paragraph {
  let len = paragraph.runs.iter().map(|run| run.text.len()).sum();
  Paragraph {
    style: paragraph.style,
    byte_range: 0..len,
    runs: merge_adjacent_runs(
      paragraph
        .runs
        .iter()
        .map(|run| TextRun {
          len: run.text.len(),
          styles: run.styles,
        })
        .collect(),
    ),
    version: 0,
  }
}

#[hotpath::measure]
pub(super) fn table_cell_paragraph_from_input_paragraph(paragraph: &InputParagraph) -> TableCellParagraph {
  let text = input_paragraph_text(paragraph);
  TableCellParagraph {
    paragraph: paragraph_from_input_paragraph(paragraph),
    text,
  }
}

#[hotpath::measure]
fn input_table_from_table(table: &TableBlock) -> InputTableBlock {
  InputTableBlock {
    rows: table
      .rows
      .iter()
      .map(|row| InputTableRow {
        cells: row
          .cells
          .iter()
          .map(|cell| InputTableCell {
            blocks: cell
              .blocks
              .iter()
              .map(|block| match block {
                TableCellBlock::Paragraph(paragraph) => InputTableCellBlock::Paragraph(input_paragraph_from_table_cell_paragraph(paragraph)),
                TableCellBlock::Table(table) => InputTableCellBlock::Table(input_table_from_table(table)),
              })
              .collect(),
            row_span: cell.row_span,
            col_span: cell.col_span,
          })
          .collect(),
      })
      .collect(),
    column_widths: table
      .column_widths
      .iter()
      .map(|width| match *width {
        TableColumnWidth::Auto => InputTableColumnWidth::Auto,
        TableColumnWidth::FixedPx(px) => InputTableColumnWidth::FixedPx(px),
        TableColumnWidth::Fraction(fraction) => InputTableColumnWidth::Fraction(fraction),
      })
      .collect(),
    style: InputTableStyle {
      header_row: table.style.header_row,
    },
  }
}

#[hotpath::measure]
fn table_from_input_table(table: &InputTableBlock) -> TableBlock {
  TableBlock {
    rows: table
      .rows
      .iter()
      .map(|row| TableRow {
        cells: row
          .cells
          .iter()
          .map(|cell| TableCell {
            blocks: cell
              .blocks
              .iter()
              .map(|block| match block {
                InputTableCellBlock::Paragraph(paragraph) => TableCellBlock::Paragraph(table_cell_paragraph_from_input_paragraph(paragraph)),
                InputTableCellBlock::Table(table) => TableCellBlock::Table(table_from_input_table(table)),
              })
              .collect(),
            row_span: cell.row_span,
            col_span: cell.col_span,
          })
          .collect(),
      })
      .collect(),
    column_widths: table
      .column_widths
      .iter()
      .map(|width| match *width {
        InputTableColumnWidth::Auto => TableColumnWidth::Auto,
        InputTableColumnWidth::FixedPx(px) => TableColumnWidth::FixedPx(px),
        InputTableColumnWidth::Fraction(fraction) => TableColumnWidth::Fraction(fraction),
      })
      .collect(),
    style: TableStyle {
      header_row: table.style.header_row,
    },
    version: 0,
  }
}

#[hotpath::measure]
fn input_alignment_from_alignment(alignment: BlockAlignment) -> InputBlockAlignment {
  match alignment {
    BlockAlignment::Left => InputBlockAlignment::Left,
    BlockAlignment::Center => InputBlockAlignment::Center,
    BlockAlignment::Right => InputBlockAlignment::Right,
  }
}

#[hotpath::measure]
fn alignment_from_input_alignment(alignment: InputBlockAlignment) -> BlockAlignment {
  match alignment {
    InputBlockAlignment::Left => BlockAlignment::Left,
    InputBlockAlignment::Center => BlockAlignment::Center,
    InputBlockAlignment::Right => BlockAlignment::Right,
  }
}
