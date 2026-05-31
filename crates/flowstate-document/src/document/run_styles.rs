
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum RunSemanticStyle {
  #[default]
  Plain,
  Cite,
  Emphasis,
  Underline,
  Condensed,
  Ultracondensed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum HighlightStyle {
  Spoken,
  Insert,
  Alternative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStyle {
  Plain,
  Cite,
  Underline,
  Emphasis,
  Condensed,
  Ultracondensed,
  HighlightSpoken,
  HighlightInsert,
  HighlightAlternative,
}

#[hotpath::measure_all]
impl From<RunStyle> for RunStyles {
  fn from(style: RunStyle) -> Self {
    let mut styles = Self::default();
    styles.apply(style);
    styles
  }
}

#[hotpath::measure_all]
impl RunStyles {
  pub fn apply(&mut self, style: RunStyle) {
    match style {
      RunStyle::Plain => self.semantic = RunSemanticStyle::Plain,
      RunStyle::Cite => self.semantic = RunSemanticStyle::Cite,
      RunStyle::Underline => self.semantic = RunSemanticStyle::Underline,
      RunStyle::Emphasis => self.semantic = RunSemanticStyle::Emphasis,
      RunStyle::Condensed => self.semantic = RunSemanticStyle::Condensed,
      RunStyle::Ultracondensed => self.semantic = RunSemanticStyle::Ultracondensed,
      RunStyle::HighlightSpoken => self.highlight = Some(HighlightStyle::Spoken),
      RunStyle::HighlightInsert => self.highlight = Some(HighlightStyle::Insert),
      RunStyle::HighlightAlternative => self.highlight = Some(HighlightStyle::Alternative),
    }
  }

  #[must_use]
  pub fn with(mut self, style: RunStyle) -> Self {
    self.apply(style);
    self
  }

  #[must_use]
  pub fn with_direct_underline(mut self) -> Self {
    self.direct_underline = true;
    self
  }

  #[must_use]
  pub fn with_strikethrough(mut self) -> Self {
    self.strikethrough = true;
    self
  }
}

// -- Theme ----------------------------------------------------------------
