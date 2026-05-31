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
    style: flowstate_document::PARAGRAPH_POCKET,
    label: "Pocket",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_HAT,
    label: "Hat",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_BLOCK,
    label: "Block",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_TAG,
    label: "Tag",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_ANALYTIC,
    label: "Analytic",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_UNDERTAG,
    label: "Undertag",
  },
];

pub const SEMANTIC_STYLE_SPECS: &[SemanticStyleSpec] = &[
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_CITE,
    label: "Cite",
  },
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_EMPHASIS,
    label: "Emphasis",
  },
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_CONDENSED,
    label: "Condensed",
  },
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_ULTRACONDENSED,
    label: "Ultracondensed",
  },
];

pub const HIGHLIGHT_STYLE_SPECS: &[HighlightStyleSpec] = &[
  HighlightStyleSpec {
    style: flowstate_document::HIGHLIGHT_SPOKEN,
    label: "Spoken",
  },
  HighlightStyleSpec {
    style: flowstate_document::HIGHLIGHT_INSERT,
    label: "Insert",
  },
  HighlightStyleSpec {
    style: flowstate_document::HIGHLIGHT_ALTERNATIVE,
    label: "Alt",
  },
];
