use docx_rs::{
  AlignmentType, BorderType, Docx, Paragraph as DocxParagraph, ParagraphBorder, ParagraphBorderPosition, Style, StyleType, TextBorder,
};
use flowstate_document::{DocumentTheme, ParagraphStyle};

use super::formatting::{apply_style_text_format, border_eighth_points, color_hex, pixels_to_pt};

#[hotpath::measure]
pub(super) fn add_flowstate_styles(docx: Docx, theme: &DocumentTheme) -> Docx {
  docx
    .add_style(apply_style_text_format(
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
    ))
    .add_style(apply_style_text_format(
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
    ))
    .add_style(apply_style_text_format(
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
    ))
    .add_style(apply_style_text_format(
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
    ))
    .add_style(apply_style_text_format(
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
    ))
    .add_style(apply_style_text_format(
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
    ))
    .add_style(apply_style_text_format(
      Style::new("Style13ptBold", StyleType::Character)
        .name("Style 13 pt Bold")
        .based_on("DefaultParagraphFont"),
      theme,
      theme.cite_font_size,
      theme.cite_color,
      theme.cite_bold,
      theme.cite_italic,
      theme.cite_underline,
    ))
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
    .add_style(apply_style_text_format(
      Style::new("StyleUnderline", StyleType::Character)
        .name("Style Underline")
        .based_on("DefaultParagraphFont"),
      theme,
      theme.body_font_size,
      theme.underline_color,
      theme.underline_bold,
      theme.underline_italic,
      theme.underline_underline,
    ))
}

#[hotpath::measure]
pub(super) fn apply_paragraph_style(paragraph: DocxParagraph, style: ParagraphStyle, theme: &DocumentTheme) -> DocxParagraph {
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
pub(super) fn emphasis_text_border(theme: &DocumentTheme) -> TextBorder {
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
    paragraph.property = paragraph
      .property
      .set_border(pocket_paragraph_border(position, theme));
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
