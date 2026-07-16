//! Ribbon UI for formatting the active rich text editor.
//!
//! This module owns the toolbar surface only. Document mutation still belongs
//! to `RichTextEditor`, so button handlers call the editor's public style API
//! instead of editing paragraphs or runs directly.

mod editor_ribbon;
pub mod shared;
mod style_catalog;

pub use editor_ribbon::{
  EditorRibbon, ModernRibbonOptions, ModernStylesRibbon, OverflowBehavior, RibbonAccent, RibbonCommand, RibbonCommandGroup,
  RibbonCommandId, RibbonDensity, ShortcutVisibility, StylesRibbon,
};
pub(crate) use editor_ribbon::{CONDENSE_PILCROW_MARKER, apply_shrink_editor_selection, condense_editor_selection, uncondense_editor_selection};
pub use style_catalog::{
  HIGHLIGHT_STYLE_SPECS, HighlightStyleSpec, PARAGRAPH_STYLE_SPECS, ParagraphStyleSpec, SEMANTIC_STYLE_SPECS, SemanticStyleSpec,
};
