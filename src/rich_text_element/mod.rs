// Submodules. Public items are re-exported below to preserve the old
// `rich_text_element` API, while internal imports keep sibling modules able to
// share implementation details without exposing them outside this module tree.
mod collaboration;
mod benchmarks;
mod demo;
mod document;
mod edit_ops;
mod editor;
mod element;
mod invisibility;
mod layout;
mod paint;
mod persistence;
mod selection;
mod tools;
mod word_boundary;

pub use collaboration::{BlockId, CanonicalOperation, CollaborationEdit, ParagraphId, TableCellId};
pub use benchmarks::{BenchmarkOptions, BenchmarkRunner};
pub use demo::{blank_document, demo_document, document_from_paragraphs};
pub use document::{
  AssetId, AssetRecord, AssetStore, Block, BlockAlignment, Document, DocumentOffset, DocumentParagraphInput, DocumentPosition, DocumentRunInput,
  DocumentTheme, EquationBlock, EquationDisplay, EquationSyntax, HighlightStyle, ImageBlock, ImageSizing, ObjectAffinity, Paragraph,
  ParagraphStyle, RunSemanticStyle, RunStyle, RunStyles, TableBlock, TableCell, TableCellBlock, TableCellParagraph, TableColumnWidth, TableRow,
  TableStyle, TextRun, ThemeUnderline,
};
pub use editor::*;
pub use element::RichTextDocumentElement;
// `read_db8` is part of the public persistence API even though only tests
// consume it inside this crate today; allow the unused-import lint so the
// re-export stays in place.
#[allow(unused_imports)]
pub use persistence::{load_or_create_document, read_db8, write_db8};
pub use tools::ArmedInlineTool;

// Internal imports used by sibling modules via `use super::*;`.
use collaboration::DocumentIdentityMap;
use document::{
  InputAsset, InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay, InputEquationSyntax, InputImageBlock, InputImageSizing,
  InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock, InputTableColumnWidth, InputTableRow, InputTableStyle,
  ParagraphOffsetIndex, RichClipboardFragment, SOFT_LINE_BREAK, SOFT_LINE_BREAK_STR, block_ix_for_paragraph, document_offset_for_position,
  document_position_for_offset, paragraph_blocks_from_paragraphs, paragraphs_mut, replace_paragraph_blocks, update_paragraph_block,
};
use edit_ops::*;
use editor::SelectionGranularity;
use element::*;
use invisibility::*;
use layout::*;
use paint::*;
use persistence::recovery_path_for_document;
use selection::*;
use word_boundary::*;

// Private re-imports for the test module. Tests live in `mod tests;` (a child
// of this module) and use `use super::*;`, so the names need to be in this
// module's namespace. Outside `#[cfg(test)]` they would be unused.
#[cfg(test)]
use demo::{document_from_input, plain, run};
#[cfg(test)]
use editor::{EditOperation, adjust_drop_after_source_delete};

use std::time::Instant;

// Shared timing utility. Setting `DEBATEPROCESSOR_TIMING=1` in the environment
// turns on per-operation `[timing] ...` lines on stderr; useful for spotting
// regressions in editing/layout hot paths. Visible to submodules so they can
// instrument their own work.
const TIMING_ENV: &str = "DEBATEPROCESSOR_TIMING";

pub(crate) fn timing_enabled() -> bool {
  std::env::var_os(TIMING_ENV).is_some()
}

pub(crate) fn log_timing(label: &str, start: Instant, detail: impl AsRef<str>) {
  if timing_enabled() {
    eprintln!("[timing] {label}: {:?} {}", start.elapsed(), detail.as_ref());
  }
}

#[cfg(test)]
mod tests;
