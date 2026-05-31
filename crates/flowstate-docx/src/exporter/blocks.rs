use docx_rs::{
  AlignmentType, BreakType, Docx, Paragraph as DocxParagraph, Run, Shading, Table as DocxTable, TableCell as DocxTableCell,
  TableRow as DocxTableRow,
};
use flowstate_document::{
  Block, Document, DocumentTheme, EquationBlock, HighlightStyle, ImageBlock, Paragraph, ParagraphStyle, RunSemanticStyle, RunStyles,
  SOFT_LINE_BREAK, TableBlock, TableCellBlock, TableCellParagraph, document_text_slice,
};

use super::{
  formatting::{apply_run_text_format, color_hex, docx_fonts},
  styles::{apply_paragraph_style, emphasis_text_border},
};

#[hotpath::measure]
pub(super) fn add_block(docx: Docx, document: &Document, block: &Block, theme: &DocumentTheme) -> Docx {
  match block {
    Block::Paragraph(paragraph) => docx.add_paragraph(export_document_paragraph(document, paragraph, theme)),
    Block::Table(table) => docx.add_table(export_table(table, theme)),
    Block::Image(image) => docx.add_paragraph(placeholder_paragraph_for_image(document, image, theme)),
    Block::Equation(equation) => docx.add_paragraph(placeholder_paragraph_for_equation(equation, theme)),
  }
}

#[hotpath::measure]
fn export_document_paragraph(document: &Document, paragraph: &Paragraph, theme: &DocumentTheme) -> DocxParagraph {
  let text = document_text_slice(document, paragraph.byte_range.clone());
  export_paragraph_with_text(paragraph, &text, theme)
}

#[hotpath::measure]
fn export_table_cell_paragraph(paragraph: &TableCellParagraph, theme: &DocumentTheme) -> DocxParagraph {
  export_paragraph_with_text(&paragraph.paragraph, &paragraph.text, theme)
}

#[hotpath::measure]
fn export_paragraph_with_text(paragraph: &Paragraph, text: &str, theme: &DocumentTheme) -> DocxParagraph {
  let mut out = apply_paragraph_style(DocxParagraph::new(), paragraph.style, theme);
  let mut byte = 0usize;
  for run in paragraph.runs.iter() {
    let start = byte.min(text.len());
    let end = (byte + run.len).min(text.len()).max(start);
    out = add_text_run(out, &text[start..end], run.styles, paragraph.style, theme);
    byte = end;
  }
  if paragraph.runs.is_empty() && text.is_empty() {
    out = out.add_run(Run::new());
  }
  if matches!(paragraph.style, ParagraphStyle::Pocket | ParagraphStyle::Hat) {
    out = out.add_run(Run::new().add_break(BreakType::Page));
  }
  out
}

#[hotpath::measure]
fn add_text_run(
  mut paragraph: DocxParagraph,
  text: &str,
  styles: RunStyles,
  paragraph_style: ParagraphStyle,
  theme: &DocumentTheme,
) -> DocxParagraph {
  let mut first = true;
  for segment in text.split(SOFT_LINE_BREAK) {
    if !first {
      paragraph = paragraph.add_run(apply_run_style(
        Run::new().add_break(BreakType::TextWrapping),
        styles,
        paragraph_style,
        theme,
      ));
    }
    first = false;
    if !segment.is_empty() {
      paragraph = paragraph.add_run(apply_run_style(Run::new().add_text(segment), styles, paragraph_style, theme));
    }
  }
  paragraph
}

#[hotpath::measure]
fn apply_run_style(run: Run, styles: RunStyles, paragraph_style: ParagraphStyle, theme: &DocumentTheme) -> Run {
  let mut run = run.fonts(docx_fonts(theme));
  run = match styles.semantic {
    RunSemanticStyle::Plain => run,
    RunSemanticStyle::Cite => run.style("Style13ptBold"),
    RunSemanticStyle::Emphasis => run
      .style("Emphasis")
      .text_border(emphasis_text_border(theme)),
    RunSemanticStyle::Underline => run.style("StyleUnderline"),
    RunSemanticStyle::Condensed => apply_run_text_format(
      run,
      theme.condensed_font_size,
      theme.condensed_color,
      theme.condensed_bold,
      theme.condensed_italic,
      theme.condensed_underline,
    ),
    RunSemanticStyle::Ultracondensed => apply_run_text_format(
      run,
      theme.ultracondensed_font_size,
      theme.ultracondensed_color,
      theme.ultracondensed_bold,
      theme.ultracondensed_italic,
      theme.ultracondensed_underline,
    ),
  };
  if styles.semantic == RunSemanticStyle::Plain && paragraph_style == ParagraphStyle::Normal {
    run = apply_run_text_format(
      run,
      theme.body_font_size,
      theme.default_text_color,
      theme.normal_bold,
      theme.normal_italic,
      theme.normal_underline,
    );
  }
  if styles.direct_underline {
    run = run.underline("single");
  }
  if styles.strikethrough {
    run = run.strike();
  }
  if let Some(highlight) = styles.highlight {
    run = run.shading(Shading::new().fill(color_hex(match highlight {
      HighlightStyle::Spoken => theme.highlight_spoken,
      HighlightStyle::Insert => theme.highlight_insert,
      HighlightStyle::Alternative => theme.highlight_alternative,
    })));
  }
  run
}

#[hotpath::measure]
fn export_table(table: &TableBlock, theme: &DocumentTheme) -> DocxTable {
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
                  TableCellBlock::Paragraph(paragraph) => out.add_paragraph(export_table_cell_paragraph(paragraph, theme)),
                  TableCellBlock::Table(table) => out.add_table(export_table(table, theme)),
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
fn placeholder_paragraph_for_image(document: &Document, image: &ImageBlock, theme: &DocumentTheme) -> DocxParagraph {
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
  DocxParagraph::new().add_run(
    Run::new()
      .fonts(docx_fonts(theme))
      .italic()
      .add_text(format!("[{text}]")),
  )
}

#[hotpath::measure]
fn placeholder_paragraph_for_equation(equation: &EquationBlock, theme: &DocumentTheme) -> DocxParagraph {
  DocxParagraph::new().align(AlignmentType::Center).add_run(
    Run::new()
      .fonts(docx_fonts(theme))
      .italic()
      .add_text(format!("[Equation: {}]", equation.source)),
  )
}
