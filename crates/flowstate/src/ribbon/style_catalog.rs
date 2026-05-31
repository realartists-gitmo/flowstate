use crate::rich_text_element::{HighlightStyle, ParagraphStyle, RunSemanticStyle};

/// Display metadata for a paragraph-level document style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParagraphStyleSpec {
  pub style: ParagraphStyle,
  pub label: &'static str,
}

/// Display metadata for a semantic inline style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticStyleSpec {
  pub style: RunSemanticStyle,
  pub label: &'static str,
}

/// Display metadata for a highlight style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightStyleSpec {
  pub style: HighlightStyle,
  pub label: &'static str,
}

/// Styles are ordered as the ribbon should present them, not alphabetically.
pub const PARAGRAPH_STYLE_SPECS: &[ParagraphStyleSpec] = &[
  ParagraphStyleSpec {
    style: ParagraphStyle::Normal,
    label: "Normal",
  },
  ParagraphStyleSpec {
    style: ParagraphStyle::Pocket,
    label: "Pocket",
  },
  ParagraphStyleSpec {
    style: ParagraphStyle::Hat,
    label: "Hat",
  },
  ParagraphStyleSpec {
    style: ParagraphStyle::Block,
    label: "Block",
  },
  ParagraphStyleSpec {
    style: ParagraphStyle::Tag,
    label: "Tag",
  },
  ParagraphStyleSpec {
    style: ParagraphStyle::Analytic,
    label: "Analytic",
  },
  ParagraphStyleSpec {
    style: ParagraphStyle::Undertag,
    label: "Undertag",
  },
];

pub const SEMANTIC_STYLE_SPECS: &[SemanticStyleSpec] = &[
  SemanticStyleSpec {
    style: RunSemanticStyle::Cite,
    label: "Cite",
  },
  SemanticStyleSpec {
    style: RunSemanticStyle::Emphasis,
    label: "Emphasis",
  },
  SemanticStyleSpec {
    style: RunSemanticStyle::Condensed,
    label: "Condensed",
  },
  SemanticStyleSpec {
    style: RunSemanticStyle::Ultracondensed,
    label: "Ultracondensed",
  },
];

pub const HIGHLIGHT_STYLE_SPECS: &[HighlightStyleSpec] = &[
  HighlightStyleSpec {
    style: HighlightStyle::Spoken,
    label: "Spoken",
  },
  HighlightStyleSpec {
    style: HighlightStyle::Insert,
    label: "Insert",
  },
  HighlightStyleSpec {
    style: HighlightStyle::Alternative,
    label: "Alt",
  },
];
