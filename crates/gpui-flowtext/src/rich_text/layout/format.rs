#[hotpath::measure]
pub(super) fn paragraph_format(document: &Document, style: ParagraphStyle) -> EffectiveParagraphFormat {
  let theme = &document.theme;
  let zoom = theme.zoom_factor.max(0.01);
  let normal = EffectiveParagraphFormat {
    font_size: theme.body_font_size * zoom,
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
    ParagraphStyle::Custom(slot) => theme
      .custom_paragraph_styles
      .get(&(slot & 0x7f))
      .map(|style| EffectiveParagraphFormat {
        font_size: style.font_size * zoom,
        font_family: style.font_family.clone().unwrap_or_else(|| normal.font_family.clone()),
        bold: style.bold,
        italic: style.italic,
        color: style.color,
        align: match style.align {
          CustomParagraphAlign::Left => ParagraphAlign::Left,
          CustomParagraphAlign::Center => ParagraphAlign::Center,
        },
        spacing_before: style.spacing_before,
        spacing_after: style.spacing_after,
        border: style.border.map(|border| ParagraphBorder {
          width: border.width,
          space_x: border.space_x,
          space_y: border.space_y,
        }),
        underline: style.underline.into(),
        line_spacing: normal.line_spacing,
      })
      .unwrap_or(normal),
  }
}

#[hotpath::measure]
pub(super) fn run_format(document: &Document, paragraph: &EffectiveParagraphFormat, styles: RunStyles) -> EffectiveRunFormat {
  let theme = &document.theme;
  let zoom = theme.zoom_factor.max(0.01);
  let mut format = EffectiveRunFormat {
    font_size: paragraph.font_size,
    font_family: paragraph.font_family.clone(),
    bold: paragraph.bold,
    italic: paragraph.italic,
    color: paragraph.color,
    underline: paragraph.underline,
    strikethrough: styles.strikethrough,
    highlight: styles.highlight.map(|highlight| match highlight {
      HighlightStyle::Custom(slot) => theme
        .custom_highlight_styles
        .get(&(slot & 0x7f))
        .map(|style| style.color)
        .unwrap_or(theme.default_highlight_color),
    }),
    border_width: px(0.0),
  };

  match styles.semantic {
    RunSemanticStyle::Plain => {},
    RunSemanticStyle::Custom(slot) => {
      if let Some(style) = theme.custom_semantic_styles.get(&(slot & 0x7f)) {
        if let Some(font_size) = style.font_size {
          format.font_size = font_size * zoom;
        }
        if let Some(font_family) = &style.font_family {
          format.font_family = font_family.clone();
        }
        if let Some(color) = style.color {
          format.color = color;
        }
        if let Some(bold) = style.bold {
          format.bold = bold;
        }
        if let Some(italic) = style.italic {
          format.italic = italic;
        }
        if let Some(underline) = style.underline {
          format.underline = underline.into();
        }
        if let Some(border_width) = style.border_width {
          format.border_width = border_width;
        }
      }
    },
  };
  if styles.direct_underline {
    format.underline = UnderlineKind::Single;
  }

  format
}
