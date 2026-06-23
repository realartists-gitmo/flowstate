use docx_rs::{
  AlignmentType, BreakType, Docx, Paragraph as DocxParagraph, Pic, Run, Shading, Table as DocxTable, TableCell as DocxTableCell,
  TableRow as DocxTableRow,
};
use flowstate_document::{
  Block, DocumentProjection, DocumentTheme, EquationBlock, HighlightStyle, ImageBlock, Paragraph, ParagraphStyle, RunSemanticStyle, RunStyles,
  SOFT_LINE_BREAK, TableBlock, TableCellBlock, TableCellParagraph, document_text_slice,
};

use super::{
  formatting::{apply_run_text_format, color_hex, docx_fonts},
  styles::{apply_paragraph_style, apply_semantic_run_text_border},
};

#[hotpath::measure]
pub(super) fn add_block(docx: Docx, document: &DocumentProjection, block: &Block, theme: &DocumentTheme) -> Docx {
  match block {
    Block::Paragraph(paragraph) => docx.add_paragraph(export_document_paragraph(document, paragraph, theme)),
    Block::Table(table) => docx.add_table(export_table(table, theme)),
    Block::Image(image) => docx.add_paragraph(export_image(document, image, theme)),
    Block::Equation(equation) => docx.add_paragraph(placeholder_paragraph_for_equation(equation, theme)),
  }
}

#[hotpath::measure]
fn export_document_paragraph(document: &DocumentProjection, paragraph: &Paragraph, theme: &DocumentTheme) -> DocxParagraph {
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
  for run in &paragraph.runs {
    let start = byte.min(text.len());
    let end = (byte + run.len).min(text.len()).max(start);
    out = add_text_run(out, &text[start..end], run.styles, paragraph.style, theme);
    byte = end;
  }
  if paragraph.runs.is_empty() && text.is_empty() {
    out = out.add_run(Run::new());
  }
  if matches!(paragraph.style, flowstate_document::PARAGRAPH_POCKET | flowstate_document::PARAGRAPH_HAT) {
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
      let segment = segment.replace('\u{f8ff}', "¶");
      paragraph = paragraph.add_run(apply_run_style(Run::new().add_text(segment), styles, paragraph_style, theme));
    }
  }
  paragraph
}

#[hotpath::measure]
fn apply_run_style(run: Run, styles: RunStyles, paragraph_style: ParagraphStyle, theme: &DocumentTheme) -> Run {
  let mut run = run.fonts(docx_fonts(theme));
  run = match styles.semantic {
    flowstate_document::SEMANTIC_CITE => apply_semantic_run_text_border(run.style("Style13ptBold"), theme, 1),
    flowstate_document::SEMANTIC_EMPHASIS => apply_semantic_run_text_border(run.style("Emphasis"), theme, 2),
    flowstate_document::SEMANTIC_UNDERLINE => apply_semantic_run_text_border(run.style("StyleUnderline"), theme, 3),
    flowstate_document::SEMANTIC_CONDENSED => apply_semantic_run_text_border(run.style("Condensed"), theme, 4),
    flowstate_document::SEMANTIC_ULTRACONDENSED => apply_semantic_run_text_border(run.style("UltraCondensed"), theme, 5),
    RunSemanticStyle::Plain | RunSemanticStyle::Custom(_) => run,
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
    run = run.shading(
      Shading::new().fill(color_hex(match highlight {
        HighlightStyle::Custom(slot) => theme
          .custom_highlight_styles
          .get(&(slot & 0x7f))
          .map(|style| style.color)
          .unwrap_or(theme.default_highlight_color),
      })),
    );
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
fn export_image(document: &DocumentProjection, image: &ImageBlock, theme: &DocumentTheme) -> DocxParagraph {
  if let Some(asset) = document.assets.assets.get(&image.asset_id)
    && !asset.is_loading_placeholder()
    && asset_is_embeddable_image(asset.bytes.as_ref())
  {
    let (width_px, height_px) = image_dimensions(asset.bytes.as_ref(), image);
    let paragraph = match image.alignment {
      flowstate_document::BlockAlignment::Left => DocxParagraph::new(),
      flowstate_document::BlockAlignment::Center => DocxParagraph::new().align(AlignmentType::Center),
      flowstate_document::BlockAlignment::Right => DocxParagraph::new().align(AlignmentType::Right),
    };
    return paragraph.add_run(
      Run::new()
        .fonts(docx_fonts(theme))
        .add_image(Pic::new_with_dimensions(asset.bytes.as_ref().clone(), width_px, height_px)),
    );
  }
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

/// OOXML natively supports PNG and JPEG image parts (see docx content-type
/// defaults), so both are embedded directly; other formats fall back to an
/// alt-text run.
fn asset_is_embeddable_image(bytes: &[u8]) -> bool {
  let is_png = bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]);
  let is_jpeg = bytes.starts_with(&[0xFF, 0xD8, 0xFF]);
  is_png || is_jpeg
}

fn image_dimensions(bytes: &[u8], image: &ImageBlock) -> (u32, u32) {
  if let flowstate_document::ImageSizing::Fixed { width_px, height_px } = image.sizing {
    return (width_px.max(1), height_px.unwrap_or(width_px).max(1));
  }
  imagesize::blob_size(bytes)
    .map(|size| (size.width.max(1) as u32, size.height.max(1) as u32))
    .unwrap_or((640, 480))
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
