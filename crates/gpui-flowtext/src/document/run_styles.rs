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

/// Vertical alignment of a run's glyphs — OOXML `w:vertAlign` (superscript /
/// subscript). Orthogonal to [`RunSemanticStyle`]: a run can be, e.g.,
/// emphasized *and* superscript, so this is a separate field on [`RunStyles`]
/// rather than a semantic slot (mirrors `strikethrough`).
///
/// PHASE-3 NOTE (deferred — not done here): as of this change super/subscript is
/// **pure data**. It is captured on `.docx` import, persisted as a Loro mark, and
/// re-emitted on `.docx` export, but it is **not rendered in the editor and not
/// user-toggleable yet**. To finish it:
///   * Rendering — `gpui-flowtext/src/rich_text/layout/shaping.rs`: apply a
///     baseline shift + ~0.65× font scale for `Superscript`/`Subscript` (this is
///     NOT a decoration line like `strikethrough` in `paint.rs`; it is a
///     glyph-metrics change, closer to how font size is handled).
///   * Editor toggle + selection state — follow `strikethrough` across
///     `rich_text/tools.rs`, `rich_text/editor/formatting.rs`,
///     `rich_text/editor/style_state.rs`, `rich_text/editor/commands.rs`.
///   * Collab runtime — mirror `MARK_STRIKETHROUGH` in the collab write/clear
///     paths (`flowstate-collab/src/crdt_runtime.rs`,
///     `crdt_runtime/projection_patch.rs`, `local_write/commit.rs`) so live
///     edits read/clear the mark; the batch import/export pipeline already does.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum VertAlign {
  #[default]
  Baseline,
  Superscript,
  Subscript,
}

impl VertAlign {
  /// Loro mark value for this alignment; `None` for `Baseline` (the mark is
  /// simply absent). Mirrors the `slot()` pattern of the other run-style marks.
  #[must_use]
  pub const fn mark_value(self) -> Option<i64> {
    match self {
      Self::Baseline => None,
      Self::Superscript => Some(1),
      Self::Subscript => Some(2),
    }
  }

  /// Decode a Loro mark value back into an alignment (unknown ⇒ `Baseline`).
  #[must_use]
  pub const fn from_mark_value(value: i64) -> Self {
    match value {
      1 => Self::Superscript,
      2 => Self::Subscript,
      _ => Self::Baseline,
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

  #[must_use]
  #[hotpath::skip]
  pub const fn with_vert_align(mut self, vert_align: VertAlign) -> Self {
    self.vert_align = vert_align;
    self
  }
}

// -- Theme ----------------------------------------------------------------
