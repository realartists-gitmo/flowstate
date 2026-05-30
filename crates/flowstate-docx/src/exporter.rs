use std::{
  fs::File,
  io,
  path::Path,
};

use docx_rs::{
  AlignmentType, BreakType, Docx, Paragraph as DocxParagraph, Run, RunFonts, Table as DocxTable, TableCell as DocxTableCell,
  TableRow as DocxTableRow,
};
use flowstate_document::{
  Block, Document, EquationBlock, HighlightStyle, ImageBlock, Paragraph, ParagraphStyle, RunSemanticStyle, RunStyles, SOFT_LINE_BREAK,
  TableBlock, TableCellBlock, TableCellParagraph, document_text_slice,
};

#[hotpath::measure]
pub fn write_docx(path: impl AsRef<Path>, document: &Document) -> io::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    std::fs::create_dir_all(parent)?;
  }
  let mut docx = Docx::new();
  for block in document.blocks.iter() {
    docx = add_block(docx, document, block);
  }
  let file = File::create(path)?;
  docx
    .build()
    .pack(file)
    .map_err(|error| io::Error::other(format!("failed to write docx package: {error}")))
}

#[hotpath::measure]
pub fn convert_db8_to_docx(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let document = flowstate_document::read_db8(input)?;
  write_docx(output, &document)
}

#[hotpath::measure]
fn add_block(docx: Docx, document: &Document, block: &Block) -> Docx {
  match block {
    Block::Paragraph(paragraph) => docx.add_paragraph(export_document_paragraph(document, paragraph)),
    Block::Table(table) => docx.add_table(export_table(table)),
    Block::Image(image) => docx.add_paragraph(placeholder_paragraph_for_image(document, image)),
    Block::Equation(equation) => docx.add_paragraph(placeholder_paragraph_for_equation(equation)),
  }
}

#[hotpath::measure]
fn export_document_paragraph(document: &Document, paragraph: &Paragraph) -> DocxParagraph {
  let text = document_text_slice(document, paragraph.byte_range.clone());
  export_paragraph_with_text(paragraph, &text)
}

#[hotpath::measure]
fn export_table_cell_paragraph(paragraph: &TableCellParagraph) -> DocxParagraph {
  export_paragraph_with_text(&paragraph.paragraph, &paragraph.text)
}

#[hotpath::measure]
fn export_paragraph_with_text(paragraph: &Paragraph, text: &str) -> DocxParagraph {
  let mut out = apply_paragraph_style(DocxParagraph::new(), paragraph.style);
  let mut byte = 0usize;
  for run in paragraph.runs.iter() {
    let start = byte.min(text.len());
    let end = (byte + run.len).min(text.len()).max(start);
    out = add_text_run(out, &text[start..end], run.styles, paragraph.style);
    byte = end;
  }
  if paragraph.runs.is_empty() && text.is_empty() {
    out = out.add_run(Run::new());
  }
  out
}

#[hotpath::measure]
fn add_text_run(mut paragraph: DocxParagraph, text: &str, styles: RunStyles, paragraph_style: ParagraphStyle) -> DocxParagraph {
  let mut first = true;
  for segment in text.split(SOFT_LINE_BREAK) {
    if !first {
      paragraph = paragraph.add_run(apply_run_style(Run::new().add_break(BreakType::TextWrapping), styles, paragraph_style));
    }
    first = false;
    if !segment.is_empty() {
      paragraph = paragraph.add_run(apply_run_style(Run::new().add_text(segment), styles, paragraph_style));
    }
  }
  paragraph
}

#[hotpath::measure]
fn apply_paragraph_style(paragraph: DocxParagraph, style: ParagraphStyle) -> DocxParagraph {
  match style {
    ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block => paragraph.align(AlignmentType::Center).bold(),
    ParagraphStyle::Tag | ParagraphStyle::Analytic => paragraph.bold(),
    ParagraphStyle::Undertag => paragraph.italic(),
    ParagraphStyle::Normal => paragraph,
  }
}

#[hotpath::measure]
fn apply_run_style(run: Run, styles: RunStyles, paragraph_style: ParagraphStyle) -> Run {
  let mut run = run.fonts(RunFonts::new().ascii("Carlito"));
  run = apply_paragraph_run_defaults(run, paragraph_style);
  run = match styles.semantic {
    RunSemanticStyle::Plain => run,
    RunSemanticStyle::Cite => run.size(26).bold(),
    RunSemanticStyle::Emphasis => run.size(26).bold().italic().underline("single"),
    RunSemanticStyle::Underline => run.underline("single"),
    RunSemanticStyle::Condensed => run.size(16),
    RunSemanticStyle::Ultracondensed => run.size(6),
  };
  if styles.direct_underline {
    run = run.underline("single");
  }
  if styles.strikethrough {
    run = run.strike();
  }
  if let Some(highlight) = styles.highlight {
    run = run.highlight(match highlight {
      HighlightStyle::Spoken => "green",
      HighlightStyle::Insert => "lightGray",
      HighlightStyle::Alternative => "cyan",
    });
  }
  run
}

#[hotpath::measure]
fn apply_paragraph_run_defaults(run: Run, style: ParagraphStyle) -> Run {
  match style {
    ParagraphStyle::Pocket => run.size(52).bold(),
    ParagraphStyle::Hat => run.size(44).bold().underline("double"),
    ParagraphStyle::Block => run.size(32).bold().underline("single"),
    ParagraphStyle::Tag => run.size(26).bold(),
    ParagraphStyle::Analytic => run.size(26).bold().color("1F3864"),
    ParagraphStyle::Undertag => run.size(24).italic().color("385623"),
    ParagraphStyle::Normal => run.size(22),
  }
}

#[hotpath::measure]
fn export_table(table: &TableBlock) -> DocxTable {
  DocxTable::new(
    table
      .rows
      .iter()
      .map(|row| {
        DocxTableRow::new(
          row
            .cells
            .iter()
            .map(|cell| {
              let mut out = DocxTableCell::new();
              for block in &cell.blocks {
                out = match block {
                  TableCellBlock::Paragraph(paragraph) => out.add_paragraph(export_table_cell_paragraph(paragraph)),
                  TableCellBlock::Table(table) => out.add_table(export_table(table)),
                };
              }
              out
            })
            .collect(),
        )
      })
      .collect(),
  )
}

#[hotpath::measure]
fn placeholder_paragraph_for_image(document: &Document, image: &ImageBlock) -> DocxParagraph {
  let mut text = image.alt_text.to_string();
  if text.trim().is_empty()
    && let Some(asset) = document.assets.assets.get(&image.asset_id)
    && let Some(name) = &asset.original_name
  {
    text = name.to_string();
  }
  if text.trim().is_empty() {
    text = "Image".to_string();
  }
  DocxParagraph::new().add_run(Run::new().italic().add_text(format!("[{text}]")))
}

#[hotpath::measure]
fn placeholder_paragraph_for_equation(equation: &EquationBlock) -> DocxParagraph {
  DocxParagraph::new()
    .align(AlignmentType::Center)
    .add_run(Run::new().italic().add_text(format!("[Equation: {}]", equation.source)))
}
