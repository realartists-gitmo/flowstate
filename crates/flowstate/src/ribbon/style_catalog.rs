use crate::rich_text_element::{HighlightStyle, ParagraphStyle, RunSemanticStyle};

/// Display metadata for a paragraph-level document style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParagraphStyleSpec {
  pub style: ParagraphStyle,
  pub id: &'static str,
  pub name: &'static str,
  pub label: &'static str,
}

/// Display metadata for a semantic inline style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticStyleSpec {
  pub style: RunSemanticStyle,
  pub id: &'static str,
  pub name: &'static str,
  pub label: &'static str,
}

/// Display metadata for a highlight style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightStyleSpec {
  pub style: HighlightStyle,
  pub id: &'static str,
  pub name: &'static str,
  pub label: &'static str,
}

/// Styles are ordered as the ribbon should present them, not alphabetically.
pub const PARAGRAPH_STYLE_SPECS: &[ParagraphStyleSpec] = &[
  ParagraphStyleSpec {
    style: ParagraphStyle::Normal,
    id: "paragraph.normal",
    name: "normal",
    label: "Normal",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_POCKET,
    id: "paragraph.pocket",
    name: "pocket",
    label: "Pocket",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_HAT,
    id: "paragraph.hat",
    name: "hat",
    label: "Hat",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_BLOCK,
    id: "paragraph.block",
    name: "block",
    label: "Block",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_TAG,
    id: "paragraph.tag",
    name: "tag",
    label: "Tag",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_ANALYTIC,
    id: "paragraph.analytic",
    name: "analytic",
    label: "Analytic",
  },
  ParagraphStyleSpec {
    style: flowstate_document::PARAGRAPH_UNDERTAG,
    id: "paragraph.undertag",
    name: "undertag",
    label: "Undertag",
  },
];

pub const SEMANTIC_STYLE_SPECS: &[SemanticStyleSpec] = &[
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_CITE,
    id: "semantic.cite",
    name: "cite",
    label: "Cite",
  },
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_EMPHASIS,
    id: "semantic.emphasis",
    name: "emphasis",
    label: "Emphasis",
  },
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_CONDENSED,
    id: "semantic.condensed",
    name: "condensed",
    label: "Condensed",
  },
  SemanticStyleSpec {
    style: flowstate_document::SEMANTIC_ULTRACONDENSED,
    id: "semantic.ultracondensed",
    name: "ultracondensed",
    label: "Ultracondensed",
  },
];

pub const HIGHLIGHT_STYLE_SPECS: &[HighlightStyleSpec] = &[
  HighlightStyleSpec {
    style: flowstate_document::HIGHLIGHT_SPOKEN,
    id: "highlight.spoken",
    name: "spoken",
    label: "Spoken",
  },
  HighlightStyleSpec {
    style: flowstate_document::HIGHLIGHT_INSERT,
    id: "highlight.insert",
    name: "insert",
    label: "Insert",
  },
  HighlightStyleSpec {
    style: flowstate_document::HIGHLIGHT_ALTERNATIVE,
    id: "highlight.alternative",
    name: "alternative",
    label: "Alt",
  },
];
