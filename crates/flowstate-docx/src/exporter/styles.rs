use docx_rs::{
  AlignmentType, BorderType, Docx, Paragraph as DocxParagraph, ParagraphBorder, ParagraphBorderPosition, Style, StyleType, TextBorder,
};
use flowstate_document::{CustomParagraphStyle, CustomSemanticStyle, DocumentTheme, ParagraphStyle};
use gpui::px;

use super::formatting::{apply_style_text_format, border_eighth_points, color_hex, pixels_to_pt};

#[hotpath::measure]
pub(super) fn add_flowstate_styles(docx: Docx, theme: &DocumentTheme) -> Docx {
  docx
    .add_style(apply_paragraph_style_border_to_style(
      apply_style_text_format(
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
      ),
      theme,
      0,
    ))
    .add_style(apply_paragraph_style_border_to_style(
      apply_style_text_format(
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
      ),
      theme,
      1,
    ))
    .add_style(apply_paragraph_style_border_to_style(
      apply_style_text_format(
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
      ),
      theme,
      2,
    ))
    .add_style(apply_paragraph_style_border_to_style(
      apply_style_text_format(
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
      ),
      theme,
      3,
    ))
    .add_style(apply_paragraph_style_border_to_style(
      apply_style_text_format(
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
      ),
      theme,
      4,
    ))
    .add_style(apply_paragraph_style_border_to_style(
      apply_style_text_format(
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
      ),
      theme,
      6,
    ))
    .add_style(semantic_character_style(
      "Style13ptBold",
      "Style 13 pt Bold",
      theme,
      1,
      semantic_style(theme, 1)
        .font_size
        .unwrap_or(theme.body_font_size),
    ))
    .add_style(semantic_character_style(
      "Emphasis",
      "Emphasis",
      theme,
      2,
      semantic_style(theme, 2)
        .font_size
        .unwrap_or(theme.body_font_size),
    ))
    .add_style(semantic_character_style(
      "StyleUnderline",
      "Style Underline",
      theme,
      3,
      theme.body_font_size,
    ))
    .add_style(semantic_character_style(
      "Condensed",
      "Condensed",
      theme,
      4,
      semantic_style(theme, 4)
        .font_size
        .unwrap_or(theme.body_font_size),
    ))
    .add_style(semantic_character_style(
      "UltraCondensed",
      "Ultra Condensed",
      theme,
      5,
      semantic_style(theme, 5)
        .font_size
        .unwrap_or(theme.body_font_size),
    ))
}

#[hotpath::measure]
pub(super) fn apply_paragraph_style(paragraph: DocxParagraph, style: ParagraphStyle, theme: &DocumentTheme) -> DocxParagraph {
  let paragraph = match style {
    flowstate_document::PARAGRAPH_POCKET => paragraph.style("Heading1"),
    flowstate_document::PARAGRAPH_HAT => paragraph.style("Heading2"),
    flowstate_document::PARAGRAPH_BLOCK => paragraph.style("Heading3"),
    flowstate_document::PARAGRAPH_TAG => paragraph.style("Heading4"),
    flowstate_document::PARAGRAPH_ANALYTIC => paragraph.style("Analytic"),
    flowstate_document::PARAGRAPH_UNDERTAG => paragraph.style("Undertag"),
    ParagraphStyle::Normal | ParagraphStyle::Custom(_) => paragraph.style("Normal"),
  };
  apply_paragraph_border(paragraph, style, theme)
}

#[hotpath::measure]
fn apply_paragraph_style_border_to_style(mut style: Style, theme: &DocumentTheme, slot: u8) -> Style {
  let Some(border) = paragraph_style(theme, slot).border.as_ref() else {
    return style;
  };
  for position in [
    ParagraphBorderPosition::Top,
    ParagraphBorderPosition::Left,
    ParagraphBorderPosition::Bottom,
    ParagraphBorderPosition::Right,
  ] {
    style.paragraph_property = style
      .paragraph_property
      .set_border(paragraph_border(position, border, theme));
  }
  style
}

#[hotpath::measure]
fn semantic_character_style(id: &'static str, name: &'static str, theme: &DocumentTheme, slot: u8, fallback_size: gpui::Pixels) -> Style {
  let semantic = semantic_style(theme, slot);
  let style = apply_style_text_format(
    Style::new(id, StyleType::Character)
      .name(name)
      .based_on("DefaultParagraphFont"),
    theme,
    semantic.font_size.unwrap_or(fallback_size),
    semantic.color.unwrap_or(theme.default_text_color),
    semantic.bold.unwrap_or(false),
    semantic.italic.unwrap_or(false),
    semantic.underline.unwrap_or_default(),
  );
  apply_semantic_text_border(style, theme, slot)
}

#[hotpath::measure]
pub(super) fn apply_semantic_text_border(style: Style, theme: &DocumentTheme, slot: u8) -> Style {
  if semantic_style(theme, slot).border_width.is_some() {
    style.text_border(semantic_text_border(theme, slot))
  } else {
    style
  }
}

#[hotpath::measure]
pub(super) fn apply_semantic_run_text_border(run: docx_rs::Run, theme: &DocumentTheme, slot: u8) -> docx_rs::Run {
  if semantic_style(theme, slot).border_width.is_some() {
    run.text_border(semantic_text_border(theme, slot))
  } else {
    run
  }
}

#[hotpath::measure]
fn semantic_text_border(theme: &DocumentTheme, slot: u8) -> TextBorder {
  TextBorder::new()
    .border_type(BorderType::Single)
    .size(border_eighth_points(semantic_style(theme, slot).border_width.unwrap_or(px(0.0))))
    .space(0)
    .color(color_hex(theme.default_text_color))
}

#[hotpath::measure]
fn apply_paragraph_border(paragraph: DocxParagraph, style: ParagraphStyle, theme: &DocumentTheme) -> DocxParagraph {
  let Some(slot) = paragraph_style_slot(style) else {
    return paragraph;
  };
  let Some(border) = paragraph_style(theme, slot).border.as_ref() else {
    return paragraph;
  };
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
      .set_border(paragraph_border(position, border, theme));
    paragraph
  })
}

#[hotpath::measure]
fn paragraph_border(
  position: ParagraphBorderPosition,
  border: &flowstate_document::CustomParagraphBorder,
  theme: &DocumentTheme,
) -> ParagraphBorder {
  ParagraphBorder::new(position)
    .val(BorderType::Single)
    .size(border_eighth_points(border.width))
    .space(pixels_to_pt(border.space_x).round().max(0.0) as usize)
    .color(color_hex(theme.default_text_color))
}

fn paragraph_style_slot(style: ParagraphStyle) -> Option<u8> {
  match style {
    flowstate_document::PARAGRAPH_POCKET => Some(0),
    flowstate_document::PARAGRAPH_HAT => Some(1),
    flowstate_document::PARAGRAPH_BLOCK => Some(2),
    flowstate_document::PARAGRAPH_TAG => Some(3),
    flowstate_document::PARAGRAPH_ANALYTIC => Some(4),
    flowstate_document::PARAGRAPH_UNDERTAG => Some(6),
    ParagraphStyle::Custom(slot) => Some(slot & 0x7f),
    ParagraphStyle::Normal => None,
  }
}

fn paragraph_style(theme: &DocumentTheme, slot: u8) -> &CustomParagraphStyle {
  theme
    .custom_paragraph_styles
    .get(&slot)
    .or_else(|| theme.custom_paragraph_styles.get(&0))
    .expect("Flowstate document theme must define paragraph style slot")
}

fn semantic_style(theme: &DocumentTheme, slot: u8) -> &CustomSemanticStyle {
  theme
    .custom_semantic_styles
    .get(&slot)
    .or_else(|| theme.custom_semantic_styles.get(&1))
    .expect("Flowstate document theme must define semantic style slot")
}
