// Submodules. Public items are re-exported below to preserve the old
// `rich_text_element` API, while internal imports keep sibling modules able to
// share implementation details without exposing them outside this module tree.
mod demo;
mod document;
mod edit_ops;
mod editor;
mod element;
mod layout;
mod paint;
mod persistence;
mod word_boundary;

pub use demo::demo_document;
pub use document::{Document, DocumentOffset, DocumentTheme, HighlightStyle, Paragraph, ParagraphStyle, RunStyle, RunStyles, TextRun};
pub use element::RichTextDocumentElement;
pub use editor::*;
// `read_db8` is part of the public persistence API even though only tests
// consume it inside this crate today; allow the unused-import lint so the
// re-export stays in place.
#[allow(unused_imports)]
pub use persistence::{load_or_create_document, read_db8, write_db8};

// Internal imports used by sibling modules via `use super::*;`.
use document::{InputParagraph, InputRun, ParagraphOffsetIndex, RichClipboardFragment, SOFT_LINE_BREAK, SOFT_LINE_BREAK_STR, paragraphs_mut};
use edit_ops::*;
use editor::{DocumentSpan, SelectionGranularity};
use element::*;
use layout::*;
use paint::*;
use persistence::recovery_path_for_document;
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

pub(super) fn timing_enabled() -> bool {
  std::env::var_os(TIMING_ENV).is_some()
}

pub(super) fn log_timing(label: &str, start: Instant, detail: impl AsRef<str>) {
  if timing_enabled() {
    eprintln!("[timing] {label}: {:?} {}", start.elapsed(), detail.as_ref());
  }
}

#[cfg(test)]
mod tests;
