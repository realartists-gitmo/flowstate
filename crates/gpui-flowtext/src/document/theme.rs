
#[derive(Clone, Debug)]
pub struct DocumentTheme {
  pub zoom_factor: f32,
  pub default_font_family: SharedString,
  pub default_text_color: Hsla,
  pub document_background_color: Hsla,
  pub pageless_inset_x: Pixels,
  pub pageless_inset_top: Pixels,
  pub pageless_inset_bottom: Pixels,
  pub body_font_size: Pixels,
  pub line_spacing: f32,
  pub line_gap_fraction: f32,
  pub paragraph_after: Pixels,
  pub inline_border_paint_width: Pixels,
  pub box_padding_left: Pixels,
  pub box_padding_right: Pixels,
  pub box_padding_top: Pixels,
  pub box_padding_bottom: Pixels,
  pub highlight_pad_x: Pixels,
  pub highlight_top_extra_fraction: f32,
  pub highlight_bottom_extra_fraction: f32,
  pub underline_fallback_top_from_baseline: Pixels,
  pub underline_rule_thickness: Pixels,
  pub snap_underline_rules_to_pixels: bool,
  pub double_underline_top_from_baseline: Pixels,
  pub double_underline_gap: Pixels,
  pub default_highlight_color: Hsla,
  pub normal_bold: bool,
  pub normal_italic: bool,
  pub normal_underline: ThemeUnderline,
  pub custom_paragraph_styles: FxHashMap<u8, CustomParagraphStyle>,
  pub custom_semantic_styles: FxHashMap<u8, CustomSemanticStyle>,
  pub custom_highlight_styles: FxHashMap<u8, CustomHighlightStyle>,
  pub invisibility_visible_paragraph_styles: FxHashSet<u8>,
  pub invisibility_visible_semantic_styles: FxHashSet<u8>,
  pub invisibility_visible_highlight_styles: FxHashSet<u8>,
}

#[derive(Clone, Debug)]
pub struct CustomParagraphStyle {
  pub font_size: Pixels,
  pub font_family: Option<SharedString>,
  pub color: Hsla,
  pub bold: bool,
  pub italic: bool,
  pub underline: ThemeUnderline,
  pub align: CustomParagraphAlign,
  pub spacing_before: Pixels,
  pub spacing_after: Pixels,
  pub border: Option<CustomParagraphBorder>,
  pub section_kind: Option<u8>,
  pub section_level: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CustomParagraphAlign {
  #[default]
  Left,
  Center,
}

#[derive(Clone, Copy, Debug)]
pub struct CustomParagraphBorder {
  pub width: Pixels,
  pub space_x: Pixels,
  pub space_y: Pixels,
}

#[derive(Clone, Debug)]
#[derive(Default)]
pub struct CustomSemanticStyle {
  pub font_size: Option<Pixels>,
  pub font_family: Option<SharedString>,
  pub color: Option<Hsla>,
  pub bold: Option<bool>,
  pub italic: Option<bool>,
  pub underline: Option<ThemeUnderline>,
  pub border_width: Option<Pixels>,
}


#[derive(Clone, Debug)]
pub struct CustomHighlightStyle {
  pub color: Hsla,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeUnderline {
  #[default]
  None,
  Single,
  Double,
}

#[hotpath::measure_all]
impl Default for DocumentTheme {
  fn default() -> Self {
    Self {
      zoom_factor: 1.0,
      default_font_family: "Carlito".into(),
      default_text_color: black(),
      document_background_color: rgb(0x00ff_ffff).into(),
      // Word page margins are 1in = 96px at 96dpi. Pageless mode should
      // not use full page margins, but a proportional inset keeps content
      // from sitting on the viewport edge.
      pageless_inset_x: px(24.0),
      pageless_inset_top: px(16.0),
      pageless_inset_bottom: px(24.0),
      body_font_size: pt(11.0),
      line_spacing: 259.0 / 240.0,
      // GPUI exposes shaped ascent/descent but not Word/DirectWrite's
      // full line gap here. Add a Calibri-like internal leading term so
      // Word's 1.08 multiple is applied to a Word-like line box.
      line_gap_fraction: 0.18,
      paragraph_after: pt(8.0),
      inline_border_paint_width: px(0.5),
      // Word run borders report zero DOCX spacing in our fixture, but
      // measured paint geometry shows a stable hidden inset around ink.
      // Keep this box-only; highlights continue using the highlight band.
      box_padding_left: pt(0.96),
      box_padding_right: pt(1.01),
      box_padding_top: pt(1.47),
      box_padding_bottom: pt(1.09),
      // These paint values come from layout-engine-handoff, whose PDF
      // measurements are in points. Keep the values in Word/PDF points,
      // then convert to GPUI logical px with pt().
      highlight_pad_x: pt(0.0),
      // Word highlights are paint rectangles, not ink boxes. The third
      // measurement pass has censored body-size rows because the analyzer
      // clipped at 12pt, but uncensored larger-size rows converge around
      // a 0.20-0.24em top expansion. Use that general rule so highlights
      // do not climb too far above the line.
      highlight_top_extra_fraction: 0.22,
      highlight_bottom_extra_fraction: 0.092,
      underline_fallback_top_from_baseline: pt(1.246),
      // GPUI paints to the screen in logical pixels. A PDF 0.25pt
      // hairline becomes subpixel-thin at 96dpi, so use a Word-like
      // one-pixel screen rule while keeping metric-based y placement.
      underline_rule_thickness: px(1.0),
      snap_underline_rules_to_pixels: true,
      double_underline_top_from_baseline: pt(17.79 - 16.5),
      double_underline_gap: pt(1.20),
      default_highlight_color: rgb(0x00ff_f59d).into(),
      normal_bold: false,
      normal_italic: false,
      normal_underline: ThemeUnderline::None,
      custom_paragraph_styles: FxHashMap::default(),
      custom_semantic_styles: FxHashMap::default(),
      custom_highlight_styles: FxHashMap::default(),
      invisibility_visible_paragraph_styles: FxHashSet::default(),
      invisibility_visible_semantic_styles: FxHashSet::default(),
      invisibility_visible_highlight_styles: FxHashSet::default(),
    }
  }
}

impl DocumentTheme {
  pub fn set_custom_paragraph_style(&mut self, slot: u8, style: CustomParagraphStyle) {
    self.custom_paragraph_styles.insert(slot & 0x7f, style);
  }

  pub fn set_custom_semantic_style(&mut self, slot: u8, style: CustomSemanticStyle) {
    self.custom_semantic_styles.insert(slot & 0x7f, style);
  }

  pub fn set_custom_highlight_style(&mut self, slot: u8, style: CustomHighlightStyle) {
    self.custom_highlight_styles.insert(slot & 0x7f, style);
  }

  pub fn set_invisibility_visible_paragraph_style(&mut self, slot: u8) {
    self.invisibility_visible_paragraph_styles.insert(slot & 0x7f);
  }

  pub fn set_invisibility_visible_semantic_style(&mut self, slot: u8) {
    self.invisibility_visible_semantic_styles.insert(slot & 0x7f);
  }

  pub fn set_invisibility_visible_highlight_style(&mut self, slot: u8) {
    self.invisibility_visible_highlight_styles.insert(slot & 0x7f);
  }
}

// -- DocumentProjection offset ------------------------------------------------------
