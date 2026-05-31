use std::{
  fs::File,
  io::{self, Cursor},
  path::Path,
};

use docx_rs::{
  AlignmentType, BorderType, BreakType, Docx, Paragraph as DocxParagraph, ParagraphBorder, ParagraphBorderPosition, Run, RunFonts,
  Shading, Style, StyleType, Table as DocxTable, TableCell as DocxTableCell, TableRow as DocxTableRow, TextBorder,
};
use flowstate_document::{
  Block, Document, DocumentTheme, EquationBlock, HighlightStyle, ImageBlock, Paragraph, ParagraphStyle, RunSemanticStyle, RunStyles,
  SOFT_LINE_BREAK, TableBlock, TableCellBlock, TableCellParagraph, ThemeUnderline, document_text_slice,
};
use gpui::{Hsla, Pixels};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

#[hotpath::measure]
pub fn write_docx(path: impl AsRef<Path>, document: &Document) -> io::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    std::fs::create_dir_all(parent)?;
  }
  let mut docx = add_flowstate_styles(Docx::new(), &document.theme);
  for block in document.blocks.iter() {
    docx = add_block(docx, document, block, &document.theme);
  }
  let mut uncompressed_package = Cursor::new(Vec::new());
  docx
    .build()
    .pack(&mut uncompressed_package)
    .map_err(|error| io::Error::other(format!("failed to write docx package: {error}")))?;
  write_recompressed_docx(path, uncompressed_package.into_inner())
}

#[hotpath::measure]
fn write_recompressed_docx(path: &Path, package: Vec<u8>) -> io::Result<()> {
  let mut archive = ZipArchive::new(Cursor::new(package))
    .map_err(|error| io::Error::other(format!("failed to read generated docx package: {error}")))?;
  let file = File::create(path)?;
  let mut writer = ZipWriter::new(file);
  for index in 0..archive.len() {
    let mut entry = archive
      .by_index(index)
      .map_err(|error| io::Error::other(format!("failed to read generated docx entry: {error}")))?;
    let name = entry.name().to_owned();
    let mut options = FileOptions::default()
      .compression_method(CompressionMethod::Deflated)
      .last_modified_time(entry.last_modified());
    if let Some(mode) = entry.unix_mode() {
      options = options.unix_permissions(mode);
    }
    if entry.is_dir() {
      writer
        .add_directory(name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx directory: {error}")))?;
    } else {
      writer
        .start_file(name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx entry: {error}")))?;
      io::copy(&mut entry, &mut writer)?;
    }
  }
  writer
    .finish()
    .map_err(|error| io::Error::other(format!("failed to finish docx package: {error}")))?;
  Ok(())
}

#[hotpath::measure]
fn add_flowstate_styles(docx: Docx, theme: &DocumentTheme) -> Docx {
  docx
    .add_style(
      apply_style_text_format(
        Style::new("Heading1", StyleType::Paragraph)
          .name("Heading 1")
          .based_on("Normal")
          .next("Normal")
          .ui_priority(9)
          .outline_lvl(0)
          .align(AlignmentType::Center),
        theme,
        theme.pocket_font_size,
        theme.pocket_color,
        theme.pocket_bold,
        theme.pocket_italic,
        theme.pocket_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Heading2", StyleType::Paragraph)
          .name("Heading 2")
          .based_on("Normal")
          .next("Normal")
          .ui_priority(9)
          .outline_lvl(1)
          .align(AlignmentType::Center),
        theme,
        theme.hat_font_size,
        theme.hat_color,
        theme.hat_bold,
        theme.hat_italic,
        theme.hat_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Heading3", StyleType::Paragraph)
          .name("Heading 3")
          .based_on("Normal")
          .next("Normal")
          .ui_priority(9)
          .outline_lvl(2)
          .align(AlignmentType::Center),
        theme,
        theme.block_font_size,
        theme.block_color,
        theme.block_bold,
        theme.block_italic,
        theme.block_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Heading4", StyleType::Paragraph)
          .name("Heading 4")
          .based_on("Normal")
          .next("Normal")
          .ui_priority(9)
          .outline_lvl(3),
        theme,
        theme.tag_font_size,
        theme.tag_color,
        theme.tag_bold,
        theme.tag_italic,
        theme.tag_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Analytic", StyleType::Paragraph)
          .name("Analytic")
          .based_on("Normal")
          .next("Normal"),
        theme,
        theme.tag_font_size,
        theme.analytic_color,
        theme.analytic_bold,
        theme.analytic_italic,
        theme.analytic_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Undertag", StyleType::Paragraph)
          .name("Undertag")
          .based_on("Normal")
          .next("Normal"),
        theme,
        theme.undertag_font_size,
        theme.undertag_color,
        theme.undertag_bold,
        theme.undertag_italic,
        theme.undertag_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Style13ptBold", StyleType::Character)
          .name("Style 13 pt Bold")
          .based_on("DefaultParagraphFont"),
        theme,
        theme.cite_font_size,
        theme.cite_color,
        theme.cite_bold,
        theme.cite_italic,
        theme.cite_underline,
      ),
    )
    .add_style(
      apply_style_text_format(
        Style::new("Emphasis", StyleType::Character)
          .name("Emphasis")
          .based_on("DefaultParagraphFont"),
        theme,
        theme.cite_font_size,
        theme.emphasis_color,
        theme.emphasis_bold,
        theme.emphasis_italic,
        theme.emphasis_underline,
      )
      .text_border(emphasis_text_border(theme)),
    )
    .add_style(
      apply_style_text_format(
        Style::new("StyleUnderline", StyleType::Character)
          .name("Style Underline")
          .based_on("DefaultParagraphFont"),
        theme,
        theme.body_font_size,
        theme.underline_color,
        theme.underline_bold,
        theme.underline_italic,
        theme.underline_underline,
      ),
    )
}

#[hotpath::measure]
pub fn convert_db8_to_docx(input: impl AsRef<Path>, output: impl AsRef<Path>) -> io::Result<()> {
  let document = flowstate_document::read_db8(input)?;
  write_docx(output, &document)
}

#[hotpath::measure]
fn add_block(docx: Docx, document: &Document, block: &Block, theme: &DocumentTheme) -> Docx {
  match block {
    Block::Paragraph(paragraph) => docx.add_paragraph(export_document_paragraph(document, paragraph, theme)),
    Block::Table(table) => docx.add_table(export_table(table, theme)),
    Block::Image(image) => docx.add_paragraph(placeholder_paragraph_for_image(document, image)),
    Block::Equation(equation) => docx.add_paragraph(placeholder_paragraph_for_equation(equation)),
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
fn apply_paragraph_style(paragraph: DocxParagraph, style: ParagraphStyle, theme: &DocumentTheme) -> DocxParagraph {
  match style {
    ParagraphStyle::Pocket => apply_pocket_border(paragraph.style("Heading1"), theme),
    ParagraphStyle::Hat => paragraph.style("Heading2"),
    ParagraphStyle::Block => paragraph.style("Heading3"),
    ParagraphStyle::Tag => paragraph.style("Heading4"),
    ParagraphStyle::Analytic => paragraph.style("Analytic"),
    ParagraphStyle::Undertag => paragraph.style("Undertag"),
    ParagraphStyle::Normal => paragraph.style("Normal"),
  }
}

#[hotpath::measure]
fn apply_run_style(run: Run, styles: RunStyles, paragraph_style: ParagraphStyle, theme: &DocumentTheme) -> Run {
  let mut run = run.fonts(docx_fonts(theme));
  run = match styles.semantic {
    RunSemanticStyle::Plain => run,
    RunSemanticStyle::Cite => run.style("Style13ptBold"),
    RunSemanticStyle::Emphasis => run.style("Emphasis").text_border(emphasis_text_border(theme)),
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
fn emphasis_text_border(theme: &DocumentTheme) -> TextBorder {
  TextBorder::new()
    .border_type(BorderType::Single)
    .size(border_eighth_points(theme.emphasis_border_width))
    .space(0)
    .color(color_hex(theme.default_text_color))
}

#[hotpath::measure]
fn apply_pocket_border(paragraph: DocxParagraph, theme: &DocumentTheme) -> DocxParagraph {
  [
    ParagraphBorderPosition::Top,
    ParagraphBorderPosition::Left,
    ParagraphBorderPosition::Bottom,
    ParagraphBorderPosition::Right,
  ]
  .into_iter()
  .fold(paragraph, |mut paragraph, position| {
    paragraph.property = paragraph.property.set_border(pocket_paragraph_border(position, theme));
    paragraph
  })
}

#[hotpath::measure]
fn pocket_paragraph_border(position: ParagraphBorderPosition, theme: &DocumentTheme) -> ParagraphBorder {
  ParagraphBorder::new(position)
    .val(BorderType::Single)
    .size(border_eighth_points(theme.pocket_border_width))
    .space(pixels_to_pt(theme.pocket_border_space_x).round().max(0.0) as usize)
    .color(color_hex(theme.default_text_color))
}

#[hotpath::measure]
fn apply_style_text_format(
  mut style: Style,
  theme: &DocumentTheme,
  size: Pixels,
  color: Hsla,
  bold: bool,
  italic: bool,
  underline: ThemeUnderline,
) -> Style {
  style = style.fonts(docx_fonts(theme)).size(half_points(size)).color(color_hex(color));
  if bold {
    style = style.bold();
  }
  if italic {
    style = style.italic();
  }
  apply_style_underline(style, underline)
}

#[hotpath::measure]
fn apply_run_text_format(mut run: Run, size: Pixels, color: Hsla, bold: bool, italic: bool, underline: ThemeUnderline) -> Run {
  run = run.size(half_points(size)).color(color_hex(color));
  if bold {
    run = run.bold();
  }
  if italic {
    run = run.italic();
  }
  apply_run_underline(run, underline)
}

#[hotpath::measure]
fn apply_style_underline(style: Style, underline: ThemeUnderline) -> Style {
  match underline {
    ThemeUnderline::None => style,
    ThemeUnderline::Single => style.underline("single"),
    ThemeUnderline::Double => style.underline("double"),
  }
}

#[hotpath::measure]
fn apply_run_underline(run: Run, underline: ThemeUnderline) -> Run {
  match underline {
    ThemeUnderline::None => run,
    ThemeUnderline::Single => run.underline("single"),
    ThemeUnderline::Double => run.underline("double"),
  }
}

#[hotpath::measure]
fn docx_fonts(theme: &DocumentTheme) -> RunFonts {
  let family = theme.default_font_family.to_string();
  RunFonts::new().ascii(family.clone()).hi_ansi(family)
}

#[hotpath::measure]
fn half_points(size: Pixels) -> usize {
  (pixels_to_pt(size) * 2.0).round().max(1.0) as usize
}

#[hotpath::measure]
fn border_eighth_points(size: Pixels) -> usize {
  (pixels_to_pt(size) * 8.0).round().max(1.0) as usize
}

#[hotpath::measure]
fn pixels_to_pt(value: Pixels) -> f32 {
  value.to_f64() as f32 * 72.0 / 96.0
}

#[hotpath::measure]
fn color_hex(color: Hsla) -> String {
  let (r, g, b) = hsla_to_rgb(color);
  format!("{r:02X}{g:02X}{b:02X}")
}

#[hotpath::measure]
fn hsla_to_rgb(color: Hsla) -> (u8, u8, u8) {
  if color.s <= 0.0 {
    let value = color_channel(color.l);
    return (value, value, value);
  }
  let q = if color.l < 0.5 {
    color.l * (1.0 + color.s)
  } else {
    color.l + color.s - color.l * color.s
  };
  let p = 2.0 * color.l - q;
  (
    color_channel(hue_to_rgb(p, q, color.h + 1.0 / 3.0)),
    color_channel(hue_to_rgb(p, q, color.h)),
    color_channel(hue_to_rgb(p, q, color.h - 1.0 / 3.0)),
  )
}

#[hotpath::measure]
fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
  if t < 0.0 {
    t += 1.0;
  }
  if t > 1.0 {
    t -= 1.0;
  }
  if t < 1.0 / 6.0 {
    return p + (q - p) * 6.0 * t;
  }
  if t < 1.0 / 2.0 {
    return q;
  }
  if t < 2.0 / 3.0 {
    return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
  }
  p
}

#[hotpath::measure]
fn color_channel(value: f32) -> u8 {
  (value.clamp(0.0, 1.0) * 255.0).round() as u8
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
