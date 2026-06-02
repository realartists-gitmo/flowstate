#[hotpath::measure]
fn paragraph_style(theme: &DocumentTheme, slot: u8) -> CustomParagraphStyle {
  flowstate_document::custom_paragraph_style(theme, slot)
}

#[hotpath::measure]
fn semantic_style(theme: &DocumentTheme, slot: u8) -> CustomSemanticStyle {
  flowstate_document::custom_semantic_style(theme, slot)
}

#[hotpath::measure_all]
impl From<&DocumentTheme> for DocumentThemeSettings {
  fn from(theme: &DocumentTheme) -> Self {
    let pocket = paragraph_style(theme, 0);
    let hat = paragraph_style(theme, 1);
    let block = paragraph_style(theme, 2);
    let tag = paragraph_style(theme, 3);
    let analytic = paragraph_style(theme, 4);
    let undertag = paragraph_style(theme, 6);
    let cite = semantic_style(theme, 1);
    let emphasis = semantic_style(theme, 2);
    let underline = semantic_style(theme, 3);
    let condensed = semantic_style(theme, 4);
    let ultracondensed = semantic_style(theme, 5);
    Self {
      default_font_family: theme.default_font_family.to_string(),
      default_text_color: theme.default_text_color.into(),
      document_background_color: theme.document_background_color.into(),
      pageless_inset_x: theme.pageless_inset_x.as_f32(),
      pageless_inset_top: theme.pageless_inset_top.as_f32(),
      pageless_inset_bottom: theme.pageless_inset_bottom.as_f32(),
      body_font_size: theme.body_font_size.as_f32(),
      cite_font_size: cite.font_size.unwrap_or(theme.body_font_size).as_f32(),
      condensed_font_size: condensed.font_size.unwrap_or(theme.body_font_size).as_f32(),
      ultracondensed_font_size: ultracondensed
        .font_size
        .unwrap_or(theme.body_font_size)
        .as_f32(),
      pocket_font_size: pocket.font_size.as_f32(),
      hat_font_size: hat.font_size.as_f32(),
      block_font_size: block.font_size.as_f32(),
      tag_font_size: tag.font_size.as_f32(),
      undertag_font_size: undertag.font_size.as_f32(),
      line_spacing: theme.line_spacing,
      line_gap_fraction: theme.line_gap_fraction,
      paragraph_after: theme.paragraph_after.as_f32(),
      pocket_before: pocket.spacing_before.as_f32(),
      hat_before: hat.spacing_before.as_f32(),
      block_before: block.spacing_before.as_f32(),
      tag_before: tag.spacing_before.as_f32(),
      pocket_box_enabled: pocket.border.is_some(),
      pocket_border_width: pocket.border.map_or(1.0, |border| border.width.as_f32()),
      pocket_border_space_x: pocket.border.map_or(6.0, |border| border.space_x.as_f32()),
      pocket_border_space_y: pocket.border.map_or(2.0, |border| border.space_y.as_f32()),
      hat_box_enabled: hat.border.is_some(),
      hat_border_width: hat.border.map_or(1.0, |border| border.width.as_f32()),
      block_box_enabled: block.border.is_some(),
      block_border_width: block.border.map_or(1.0, |border| border.width.as_f32()),
      tag_box_enabled: tag.border.is_some(),
      tag_border_width: tag.border.map_or(1.0, |border| border.width.as_f32()),
      analytic_box_enabled: analytic.border.is_some(),
      analytic_border_width: analytic.border.map_or(1.0, |border| border.width.as_f32()),
      undertag_box_enabled: undertag.border.is_some(),
      undertag_border_width: undertag.border.map_or(1.0, |border| border.width.as_f32()),
      cite_box_enabled: cite.border_width.is_some(),
      cite_border_width: cite.border_width.unwrap_or(px(1.0)).as_f32(),
      emphasis_box_enabled: emphasis.border_width.is_some(),
      emphasis_border_width: emphasis.border_width.unwrap_or(px(1.0)).as_f32(),
      underline_box_enabled: underline.border_width.is_some(),
      underline_border_width: underline.border_width.unwrap_or(px(1.0)).as_f32(),
      condensed_box_enabled: condensed.border_width.is_some(),
      condensed_border_width: condensed.border_width.unwrap_or(px(1.0)).as_f32(),
      ultracondensed_box_enabled: ultracondensed.border_width.is_some(),
      ultracondensed_border_width: ultracondensed.border_width.unwrap_or(px(1.0)).as_f32(),
      emphasis_border_paint_width: theme.inline_border_paint_width.as_f32(),
      box_padding_left: theme.box_padding_left.as_f32(),
      box_padding_right: theme.box_padding_right.as_f32(),
      box_padding_top: theme.box_padding_top.as_f32(),
      box_padding_bottom: theme.box_padding_bottom.as_f32(),
      highlight_pad_x: theme.highlight_pad_x.as_f32(),
      highlight_top_extra_fraction: theme.highlight_top_extra_fraction,
      highlight_bottom_extra_fraction: theme.highlight_bottom_extra_fraction,
      underline_fallback_top_from_baseline: theme.underline_fallback_top_from_baseline.as_f32(),
      underline_rule_thickness: theme.underline_rule_thickness.as_f32(),
      snap_underline_rules_to_pixels: theme.snap_underline_rules_to_pixels,
      double_underline_top_from_baseline: theme.double_underline_top_from_baseline.as_f32(),
      double_underline_gap: theme.double_underline_gap.as_f32(),
      highlight_spoken: flowstate_document::custom_highlight_color(theme, 1).into(),
      highlight_insert: flowstate_document::custom_highlight_color(theme, 2).into(),
      highlight_alternative: flowstate_document::custom_highlight_color(theme, 3).into(),
      pocket_color: pocket.color.into(),
      hat_color: hat.color.into(),
      block_color: block.color.into(),
      tag_color: tag.color.into(),
      analytic_color: analytic.color.into(),
      undertag_color: undertag.color.into(),
      cite_color: cite.color.unwrap_or(theme.default_text_color).into(),
      underline_color: underline.color.unwrap_or(theme.default_text_color).into(),
      emphasis_color: emphasis.color.unwrap_or(theme.default_text_color).into(),
      condensed_color: condensed.color.unwrap_or(theme.default_text_color).into(),
      ultracondensed_color: ultracondensed
        .color
        .unwrap_or(theme.default_text_color)
        .into(),
      normal_bold: theme.normal_bold,
      normal_italic: theme.normal_italic,
      normal_underline: theme.normal_underline.into(),
      pocket_bold: pocket.bold,
      pocket_italic: pocket.italic,
      pocket_underline: pocket.underline.into(),
      hat_bold: hat.bold,
      hat_italic: hat.italic,
      hat_underline: hat.underline.into(),
      block_bold: block.bold,
      block_italic: block.italic,
      block_underline: block.underline.into(),
      tag_bold: tag.bold,
      tag_italic: tag.italic,
      tag_underline: tag.underline.into(),
      analytic_bold: analytic.bold,
      analytic_italic: analytic.italic,
      analytic_underline: analytic.underline.into(),
      undertag_bold: undertag.bold,
      undertag_italic: undertag.italic,
      undertag_underline: undertag.underline.into(),
      cite_bold: cite.bold.unwrap_or(false),
      cite_italic: cite.italic.unwrap_or(false),
      cite_underline: cite.underline.unwrap_or_default().into(),
      underline_bold: underline.bold.unwrap_or(false),
      underline_italic: underline.italic.unwrap_or(false),
      underline_underline: underline.underline.unwrap_or_default().into(),
      emphasis_bold: emphasis.bold.unwrap_or(false),
      emphasis_italic: emphasis.italic.unwrap_or(false),
      emphasis_underline: emphasis.underline.unwrap_or_default().into(),
      condensed_bold: condensed.bold.unwrap_or(false),
      condensed_italic: condensed.italic.unwrap_or(false),
      condensed_underline: condensed.underline.unwrap_or_default().into(),
      ultracondensed_bold: ultracondensed.bold.unwrap_or(false),
      ultracondensed_italic: ultracondensed.italic.unwrap_or(false),
      ultracondensed_underline: ultracondensed.underline.unwrap_or_default().into(),
    }
  }
}

#[hotpath::measure_all]
impl From<DocumentThemeSettings> for DocumentTheme {
  fn from(settings: DocumentThemeSettings) -> Self {
    let mut theme = flowstate_document_theme();
    theme.zoom_factor = 1.0;
    theme.default_font_family = settings.default_font_family.into();
    theme.default_text_color = settings.default_text_color.into();
    theme.document_background_color = settings.document_background_color.into();
    theme.pageless_inset_x = px(settings.pageless_inset_x);
    theme.pageless_inset_top = px(settings.pageless_inset_top);
    theme.pageless_inset_bottom = px(settings.pageless_inset_bottom);
    theme.body_font_size = px(settings.body_font_size);
    theme.line_spacing = settings.line_spacing;
    theme.line_gap_fraction = settings.line_gap_fraction;
    theme.paragraph_after = px(settings.paragraph_after);
    theme.inline_border_paint_width = px(settings.emphasis_border_paint_width);
    theme.box_padding_left = px(settings.box_padding_left);
    theme.box_padding_right = px(settings.box_padding_right);
    theme.box_padding_top = px(settings.box_padding_top);
    theme.box_padding_bottom = px(settings.box_padding_bottom);
    theme.highlight_pad_x = px(settings.highlight_pad_x);
    theme.highlight_top_extra_fraction = settings.highlight_top_extra_fraction;
    theme.highlight_bottom_extra_fraction = settings.highlight_bottom_extra_fraction;
    theme.underline_fallback_top_from_baseline = px(settings.underline_fallback_top_from_baseline);
    theme.underline_rule_thickness = px(settings.underline_rule_thickness);
    theme.snap_underline_rules_to_pixels = settings.snap_underline_rules_to_pixels;
    theme.double_underline_top_from_baseline = px(settings.double_underline_top_from_baseline);
    theme.double_underline_gap = px(settings.double_underline_gap);
    theme.normal_bold = settings.normal_bold;
    theme.normal_italic = settings.normal_italic;
    theme.normal_underline = settings.normal_underline.into();

    update_paragraph_style(
      &mut theme,
      0,
      settings.pocket_font_size,
      settings.pocket_color.into(),
      settings.pocket_bold,
      settings.pocket_italic,
      settings.pocket_underline.into(),
      settings.pocket_before,
      settings.pocket_box_enabled.then_some((
        settings.pocket_border_width,
        settings.pocket_border_space_x,
        settings.pocket_border_space_y,
      )),
    );
    update_paragraph_style(
      &mut theme,
      1,
      settings.hat_font_size,
      settings.hat_color.into(),
      settings.hat_bold,
      settings.hat_italic,
      settings.hat_underline.into(),
      settings.hat_before,
      settings
        .hat_box_enabled
        .then_some((settings.hat_border_width, settings.pocket_border_space_x, settings.pocket_border_space_y)),
    );
    update_paragraph_style(
      &mut theme,
      2,
      settings.block_font_size,
      settings.block_color.into(),
      settings.block_bold,
      settings.block_italic,
      settings.block_underline.into(),
      settings.block_before,
      settings.block_box_enabled.then_some((
        settings.block_border_width,
        settings.pocket_border_space_x,
        settings.pocket_border_space_y,
      )),
    );
    update_paragraph_style(
      &mut theme,
      3,
      settings.tag_font_size,
      settings.tag_color.into(),
      settings.tag_bold,
      settings.tag_italic,
      settings.tag_underline.into(),
      settings.tag_before,
      settings
        .tag_box_enabled
        .then_some((settings.tag_border_width, settings.pocket_border_space_x, settings.pocket_border_space_y)),
    );
    update_paragraph_style(
      &mut theme,
      4,
      settings.tag_font_size,
      settings.analytic_color.into(),
      settings.analytic_bold,
      settings.analytic_italic,
      settings.analytic_underline.into(),
      settings.tag_before,
      settings.analytic_box_enabled.then_some((
        settings.analytic_border_width,
        settings.pocket_border_space_x,
        settings.pocket_border_space_y,
      )),
    );
    update_paragraph_style(
      &mut theme,
      6,
      settings.undertag_font_size,
      settings.undertag_color.into(),
      settings.undertag_bold,
      settings.undertag_italic,
      settings.undertag_underline.into(),
      0.0,
      settings.undertag_box_enabled.then_some((
        settings.undertag_border_width,
        settings.pocket_border_space_x,
        settings.pocket_border_space_y,
      )),
    );

    update_semantic_style(
      &mut theme,
      1,
      settings.cite_font_size,
      settings.cite_color.into(),
      settings.cite_bold,
      settings.cite_italic,
      settings.cite_underline.into(),
      settings
        .cite_box_enabled
        .then_some(settings.cite_border_width),
    );
    update_semantic_style(
      &mut theme,
      2,
      settings.cite_font_size,
      settings.emphasis_color.into(),
      settings.emphasis_bold,
      settings.emphasis_italic,
      settings.emphasis_underline.into(),
      settings
        .emphasis_box_enabled
        .then_some(settings.emphasis_border_width),
    );
    update_semantic_style(
      &mut theme,
      3,
      settings.body_font_size,
      settings.underline_color.into(),
      settings.underline_bold,
      settings.underline_italic,
      settings.underline_underline.into(),
      settings
        .underline_box_enabled
        .then_some(settings.underline_border_width),
    );
    update_semantic_style(
      &mut theme,
      4,
      settings.condensed_font_size,
      settings.condensed_color.into(),
      settings.condensed_bold,
      settings.condensed_italic,
      settings.condensed_underline.into(),
      settings
        .condensed_box_enabled
        .then_some(settings.condensed_border_width),
    );
    update_semantic_style(
      &mut theme,
      5,
      settings.ultracondensed_font_size,
      settings.ultracondensed_color.into(),
      settings.ultracondensed_bold,
      settings.ultracondensed_italic,
      settings.ultracondensed_underline.into(),
      settings
        .ultracondensed_box_enabled
        .then_some(settings.ultracondensed_border_width),
    );

    flowstate_document::set_custom_highlight_color(&mut theme, 1, settings.highlight_spoken.into());
    flowstate_document::set_custom_highlight_color(&mut theme, 2, settings.highlight_insert.into());
    flowstate_document::set_custom_highlight_color(&mut theme, 3, settings.highlight_alternative.into());
    theme
  }
}

#[hotpath::measure]
fn update_paragraph_style(
  theme: &mut DocumentTheme,
  slot: u8,
  font_size: f32,
  color: Hsla,
  bold: bool,
  italic: bool,
  underline: ThemeUnderline,
  spacing_before: f32,
  border: Option<(f32, f32, f32)>,
) {
  let mut style = flowstate_document::custom_paragraph_style(theme, slot);
  style.font_size = px(font_size);
  style.color = color;
  style.bold = bold;
  style.italic = italic;
  style.underline = underline;
  style.spacing_before = px(spacing_before);
  style.border = border.map(|(width, space_x, space_y)| CustomParagraphBorder {
    width: px(width),
    space_x: px(space_x),
    space_y: px(space_y),
  });
  theme.set_custom_paragraph_style(slot, style);
}

#[hotpath::measure]
fn update_semantic_style(
  theme: &mut DocumentTheme,
  slot: u8,
  font_size: f32,
  color: Hsla,
  bold: bool,
  italic: bool,
  underline: ThemeUnderline,
  border_width: Option<f32>,
) {
  let mut style = flowstate_document::custom_semantic_style(theme, slot);
  style.font_size = Some(px(font_size));
  style.color = Some(color);
  style.bold = Some(bold);
  style.italic = Some(italic);
  style.underline = Some(underline);
  style.border_width = border_width.map(px);
  theme.set_custom_semantic_style(slot, style);
}

#[hotpath::measure_all]
impl From<Hsla> for StoredHsla {
  fn from(color: Hsla) -> Self {
    Self {
      h: color.h,
      s: color.s,
      l: color.l,
      a: color.a,
    }
  }
}

#[hotpath::measure_all]
impl From<StoredHsla> for Hsla {
  fn from(color: StoredHsla) -> Self {
    Hsla {
      h: color.h,
      s: color.s,
      l: color.l,
      a: color.a,
    }
  }
}

#[hotpath::measure_all]
impl From<ThemeUnderline> for ThemeUnderlineSetting {
  fn from(value: ThemeUnderline) -> Self {
    match value {
      ThemeUnderline::None => Self::None,
      ThemeUnderline::Single => Self::Single,
      ThemeUnderline::Double => Self::Double,
    }
  }
}

#[hotpath::measure_all]
impl From<ThemeUnderlineSetting> for ThemeUnderline {
  fn from(value: ThemeUnderlineSetting) -> Self {
    match value {
      ThemeUnderlineSetting::None => Self::None,
      ThemeUnderlineSetting::Single => Self::Single,
      ThemeUnderlineSetting::Double => Self::Double,
    }
  }
}
