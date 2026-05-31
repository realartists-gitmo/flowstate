//! Ribbon UI for formatting the active rich text editor.
//!
//! This module owns the toolbar surface only. Document mutation still belongs
//! to `RichTextEditor`, so button handlers call the editor's public style API
//! instead of editing paragraphs or runs directly.

mod editor_ribbon;
mod style_catalog;

pub use editor_ribbon::{
  EditorRibbon, LegacyStylesRibbon, ModernRibbonOptions, ModernStylesRibbon, OverflowBehavior, RibbonAccent, RibbonCommand, RibbonCommandGroup,
  RibbonCommandId, RibbonDensity, RibbonMode, ShortcutVisibility, StylesRibbon,
};
pub use style_catalog::{
  HIGHLIGHT_STYLE_SPECS, HighlightStyleSpec, PARAGRAPH_STYLE_SPECS, ParagraphStyleSpec, SEMANTIC_STYLE_SPECS, SemanticStyleSpec,
};
