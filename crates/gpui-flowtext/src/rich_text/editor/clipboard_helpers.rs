#[hotpath::measure]
fn block_fragment_plain_text(fragment: &RichClipboardFragment) -> String {
  let mut parts = fragment
    .paragraphs
    .iter()
    .map(input_paragraph_text)
    .collect::<Vec<_>>();
  parts.extend(fragment.blocks.iter().map(|block| match block {
    InputBlock::Paragraph(paragraph) => input_paragraph_text(paragraph),
    InputBlock::Image(image) => {
      if image.alt_text.is_empty() {
        "[Image]".to_string()
      } else {
        image.alt_text.clone()
      }
    },
    InputBlock::Equation(equation) => equation.source.clone(),
    InputBlock::Table(table) => table_plain_text(table),
  }));
  parts.join("\n")
}

#[hotpath::measure]
fn table_plain_text(table: &InputTableBlock) -> String {
  table
    .rows
    .iter()
    .map(|row| {
      row
        .cells
        .iter()
        .map(|cell| {
          cell
            .blocks
            .iter()
            .map(|block| match block {
              InputTableCellBlock::Paragraph(paragraph) => input_paragraph_text(paragraph),
              InputTableCellBlock::Table(table) => table_plain_text(table),
              // B-S5: cell objects mirror the body's plain-text stand-ins.
              InputTableCellBlock::Image(image) => image.alt_text.clone(),
              InputTableCellBlock::Equation(equation) => equation.source.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\t")
    })
    .collect::<Vec<_>>()
    .join("\n")
}

