#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum RunSemanticStyle {
  #[default]
  Plain,
  Custom(u8),
}

impl RunSemanticStyle {
  #[must_use]
  pub const fn slot(self) -> u64 {
    match self {
      Self::Plain => 0,
      Self::Custom(slot) => 128 + slot as u64,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum HighlightStyle {
  Custom(u8),
}

impl HighlightStyle {
  #[must_use]
  pub const fn slot(self) -> u64 {
    match self {
      Self::Custom(slot) => 128 + slot as u64,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStyle {
  Plain,
  Semantic(u8),
  Highlight(u8),
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
  #[hotpath::skip]
  pub const fn apply(&mut self, style: RunStyle) {
    match style {
      RunStyle::Plain => self.semantic = RunSemanticStyle::Plain,
      RunStyle::Semantic(slot) => self.semantic = RunSemanticStyle::Custom(slot),
      RunStyle::Highlight(slot) => self.highlight = Some(HighlightStyle::Custom(slot)),
    }
  }

  #[must_use]
  #[hotpath::skip]
  pub const fn with(mut self, style: RunStyle) -> Self {
    self.apply(style);
    self
  }

  #[must_use]
  #[hotpath::skip]
  pub const fn with_direct_underline(mut self) -> Self {
    self.direct_underline = true;
    self
  }

  #[must_use]
  #[hotpath::skip]
  pub const fn with_strikethrough(mut self) -> Self {
    self.strikethrough = true;
    self
  }
}

// -- Theme ----------------------------------------------------------------
