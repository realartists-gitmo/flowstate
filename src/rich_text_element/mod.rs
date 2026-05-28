// Submodules. Document model, edit helpers, persistence, demo builders, and
// collaboration IDs live in `flowstate-document`; this module keeps the old
// `rich_text_element` facade while editor/UI internals stay in the app crate.
mod benchmarks;
mod editor;
mod element;
mod invisibility;
mod layout;
mod paint;
mod selection;
mod tools;
mod word_boundary;

pub use benchmarks::{BenchmarkOptions, BenchmarkRunner};
pub use editor::*;
pub use element::RichTextDocumentElement;
pub use flowstate_document::*;
pub use tools::ArmedInlineTool;

// Internal imports used by sibling modules via `use super::*;`.
use editor::SelectionGranularity;
use element::*;
use invisibility::*;
use layout::*;
use paint::*;
use selection::*;
use word_boundary::*;

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

pub(crate) fn log_timing_lazy(label: &str, start: Instant, detail: impl FnOnce() -> String) {
  if timing_enabled() {
    eprintln!("[timing] {label}: {:?} {}", start.elapsed(), detail());
  }
}

#[cfg(test)]
use editor::{EditOperation, adjust_drop_after_source_delete};

#[cfg(test)]
mod tests;
