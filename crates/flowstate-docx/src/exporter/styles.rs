use docx_rs::{
  AlignmentType, BorderType, Docx, Paragraph as DocxParagraph, ParagraphBorder, ParagraphBorderPosition, Style, StyleType, TextBorder,
};
use flowstate_document::{CustomParagraphStyle, CustomSemanticStyle, DocumentTheme, ParagraphStyle};
use gpui::px;

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
      paragraph_style(theme, 0).font_size,
      paragraph_style(theme, 0).color,
      paragraph_style(theme, 0).bold,
      paragraph_style(theme, 0).italic,
      paragraph_style(theme, 0).underline,
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
      paragraph_style(theme, 1).font_size,
      paragraph_style(theme, 1).color,
      paragraph_style(theme, 1).bold,
      paragraph_style(theme, 1).italic,
      paragraph_style(theme, 1).underline,
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
      paragraph_style(theme, 2).font_size,
      paragraph_style(theme, 2).color,
      paragraph_style(theme, 2).bold,
      paragraph_style(theme, 2).italic,
      paragraph_style(theme, 2).underline,
    ))
    .add_style(apply_style_text_format(
      Style::new("Heading4", StyleType::Paragraph)
        .name("Heading 4")
        .based_on("Normal")
        .next("Normal")
        .ui_priority(9)
        .outline_lvl(3),
      theme,
      paragraph_style(theme, 3).font_size,
      paragraph_style(theme, 3).color,
      paragraph_style(theme, 3).bold,
      paragraph_style(theme, 3).italic,
      paragraph_style(theme, 3).underline,
    ))
    .add_style(apply_style_text_format(
      Style::new("Analytic", StyleType::Paragraph)
        .name("Analytic")
        .based_on("Normal")
        .next("Normal"),
      theme,
      paragraph_style(theme, 4).font_size,
      paragraph_style(theme, 4).color,
      paragraph_style(theme, 4).bold,
      paragraph_style(theme, 4).italic,
      paragraph_style(theme, 4).underline,
    ))
    .add_style(apply_style_text_format(
      Style::new("Undertag", StyleType::Paragraph)
        .name("Undertag")
        .based_on("Normal")
        .next("Normal"),
      theme,
      paragraph_style(theme, 6).font_size,
      paragraph_style(theme, 6).color,
      paragraph_style(theme, 6).bold,
      paragraph_style(theme, 6).italic,
      paragraph_style(theme, 6).underline,
    ))
    .add_style(apply_style_text_format(
      Style::new("Style13ptBold", StyleType::Character)
        .name("Style 13 pt Bold")
        .based_on("DefaultParagraphFont"),
      theme,
      semantic_style(theme, 1)
        .font_size
        .unwrap_or(theme.body_font_size),
      semantic_style(theme, 1)
        .color
        .unwrap_or(theme.default_text_color),
      semantic_style(theme, 1).bold.unwrap_or(false),
      semantic_style(theme, 1).italic.unwrap_or(false),
      semantic_style(theme, 1).underline.unwrap_or_default(),
    ))
    .add_style(
      apply_style_text_format(
        Style::new("Emphasis", StyleType::Character)
          .name("Emphasis")
          .based_on("DefaultParagraphFont"),
        theme,
        semantic_style(theme, 2)
          .font_size
          .unwrap_or(theme.body_font_size),
        semantic_style(theme, 2)
          .color
          .unwrap_or(theme.default_text_color),
        semantic_style(theme, 2).bold.unwrap_or(false),
        semantic_style(theme, 2).italic.unwrap_or(false),
        semantic_style(theme, 2).underline.unwrap_or_default(),
      )
      .text_border(emphasis_text_border(theme)),
    )
    .add_style(apply_style_text_format(
      Style::new("StyleUnderline", StyleType::Character)
        .name("Style Underline")
        .based_on("DefaultParagraphFont"),
      theme,
      theme.body_font_size,
      semantic_style(theme, 3)
        .color
        .unwrap_or(theme.default_text_color),
      semantic_style(theme, 3).bold.unwrap_or(false),
      semantic_style(theme, 3).italic.unwrap_or(false),
      semantic_style(theme, 3).underline.unwrap_or_default(),
    ))
}

#[hotpath::measure]
pub(super) fn apply_paragraph_style(paragraph: DocxParagraph, style: ParagraphStyle, theme: &DocumentTheme) -> DocxParagraph {
  match style {
    flowstate_document::PARAGRAPH_POCKET => apply_pocket_border(paragraph.style("Heading1"), theme),
    flowstate_document::PARAGRAPH_HAT => paragraph.style("Heading2"),
    flowstate_document::PARAGRAPH_BLOCK => paragraph.style("Heading3"),
    flowstate_document::PARAGRAPH_TAG => paragraph.style("Heading4"),
    flowstate_document::PARAGRAPH_ANALYTIC => paragraph.style("Analytic"),
    flowstate_document::PARAGRAPH_UNDERTAG => paragraph.style("Undertag"),
    ParagraphStyle::Normal | ParagraphStyle::Custom(_) => paragraph.style("Normal"),
  }
}

#[hotpath::measure]
pub(super) fn emphasis_text_border(theme: &DocumentTheme) -> TextBorder {
  TextBorder::new()
    .border_type(BorderType::Single)
    .size(border_eighth_points(semantic_style(theme, 2).border_width.unwrap_or(px(0.0))))
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
    .size(border_eighth_points(
      paragraph_style(theme, 0)
        .border
        .as_ref()
        .map(|border| border.width)
        .unwrap_or(px(0.0)),
    ))
    .space(
      pixels_to_pt(
        paragraph_style(theme, 0)
          .border
          .as_ref()
          .map(|border| border.space_x)
          .unwrap_or(px(0.0)),
      )
      .round()
      .max(0.0) as usize,
    )
    .color(color_hex(theme.default_text_color))
}

fn paragraph_style(theme: &DocumentTheme, slot: u8) -> &CustomParagraphStyle {
  theme
    .custom_paragraph_styles
    .get(&slot)
    .expect("Flowstate document theme must define paragraph style slot")
}

fn semantic_style(theme: &DocumentTheme, slot: u8) -> &CustomSemanticStyle {
  theme
    .custom_semantic_styles
    .get(&slot)
    .expect("Flowstate document theme must define semantic style slot")
}
