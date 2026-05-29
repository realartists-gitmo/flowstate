#[hotpath::measure]
pub(super) fn paragraph_format(document: &Document, style: ParagraphStyle) -> EffectiveParagraphFormat {
  let theme = &document.theme;
  let normal = EffectiveParagraphFormat {
    font_size: theme.body_font_size,
    font_family: theme.default_font_family.clone(),
    bold: theme.normal_bold,
    italic: theme.normal_italic,
    color: theme.default_text_color,
    align: ParagraphAlign::Left,
    spacing_before: px(0.0),
    spacing_after: theme.paragraph_after,
    line_spacing: theme.line_spacing,
    border: None,
    underline: theme.normal_underline.into(),
  };

  match style {
    ParagraphStyle::Normal => normal,
    ParagraphStyle::Pocket => EffectiveParagraphFormat {
      font_size: theme.pocket_font_size,
      color: theme.pocket_color,
      bold: theme.pocket_bold,
      italic: theme.pocket_italic,
      align: ParagraphAlign::Center,
      spacing_before: theme.pocket_before,
      spacing_after: px(0.0),
      border: Some(ParagraphBorder {
        width: theme.pocket_border_width,
        space_x: theme.pocket_border_space_x,
        space_y: theme.pocket_border_space_y,
      }),
      underline: theme.pocket_underline.into(),
      ..normal
    },
    ParagraphStyle::Hat => EffectiveParagraphFormat {
      font_size: theme.hat_font_size,
      color: theme.hat_color,
      bold: theme.hat_bold,
      italic: theme.hat_italic,
      align: ParagraphAlign::Center,
      spacing_before: theme.hat_before,
      spacing_after: px(0.0),
      underline: theme.hat_underline.into(),
      ..normal
    },
    ParagraphStyle::Block => EffectiveParagraphFormat {
      font_size: theme.block_font_size,
      color: theme.block_color,
      bold: theme.block_bold,
      italic: theme.block_italic,
      align: ParagraphAlign::Center,
      spacing_before: theme.block_before,
      spacing_after: px(0.0),
      underline: theme.block_underline.into(),
      ..normal
    },
    ParagraphStyle::Tag => EffectiveParagraphFormat {
      font_size: theme.tag_font_size,
      color: theme.tag_color,
      bold: theme.tag_bold,
      italic: theme.tag_italic,
      underline: theme.tag_underline.into(),
      spacing_before: theme.tag_before,
      spacing_after: px(0.0),
      ..normal
    },
    ParagraphStyle::Analytic => EffectiveParagraphFormat {
      font_size: theme.tag_font_size,
      bold: theme.analytic_bold,
      italic: theme.analytic_italic,
      color: theme.analytic_color,
      underline: theme.analytic_underline.into(),
      spacing_before: theme.tag_before,
      spacing_after: px(0.0),
      ..normal
    },
    ParagraphStyle::Undertag => EffectiveParagraphFormat {
      font_size: theme.undertag_font_size,
      font_family: theme.default_font_family.clone(),
      bold: theme.undertag_bold,
      italic: theme.undertag_italic,
      color: theme.undertag_color,
      underline: theme.undertag_underline.into(),
      spacing_after: px(0.0),
      ..normal
    },
  }
}

#[hotpath::measure]
pub(super) fn run_format(document: &Document, paragraph: &EffectiveParagraphFormat, styles: RunStyles) -> EffectiveRunFormat {
  let theme = &document.theme;
  let mut format = EffectiveRunFormat {
    font_size: paragraph.font_size,
    font_family: paragraph.font_family.clone(),
    bold: paragraph.bold,
    italic: paragraph.italic,
    color: paragraph.color,
    underline: paragraph.underline,
    strikethrough: styles.strikethrough,
    highlight: styles.highlight.map(|highlight| match highlight {
      HighlightStyle::Spoken => theme.highlight_spoken,
      HighlightStyle::Insert => theme.highlight_insert,
      HighlightStyle::Alternative => theme.highlight_alternative,
    }),
    border_width: px(0.0),
  };

  match styles.semantic {
    RunSemanticStyle::Plain => {},
    RunSemanticStyle::Underline => {
      format.font_size = theme.body_font_size;
      format.color = theme.underline_color;
      format.bold = theme.underline_bold;
      format.italic = theme.underline_italic;
      format.underline = theme.underline_underline.into();
    },
    RunSemanticStyle::Cite => {
      format.font_size = theme.cite_font_size;
      format.color = theme.cite_color;
      format.bold = theme.cite_bold;
      format.italic = theme.cite_italic;
      format.underline = theme.cite_underline.into();
    },
    RunSemanticStyle::Emphasis => {
      format.font_family = theme.default_font_family.clone();
      format.font_size = theme.cite_font_size;
      format.color = theme.emphasis_color;
      format.bold = theme.emphasis_bold;
      format.italic = theme.emphasis_italic;
      format.underline = theme.emphasis_underline.into();
      format.border_width = theme.emphasis_border_width;
    },
    RunSemanticStyle::Condensed => {
      format.font_size = theme.condensed_font_size;
      format.color = theme.condensed_color;
      format.bold = theme.condensed_bold;
      format.italic = theme.condensed_italic;
      format.underline = theme.condensed_underline.into();
    },
    RunSemanticStyle::Ultracondensed => {
      format.font_size = theme.ultracondensed_font_size;
      format.color = theme.ultracondensed_color;
      format.bold = theme.ultracondensed_bold;
      format.italic = theme.ultracondensed_italic;
      format.underline = theme.ultracondensed_underline.into();
    },
  };
  if styles.direct_underline {
    format.underline = UnderlineKind::Single;
  }

  format
}

