mod collaboration;
mod demo;
mod document;
mod edit_ops;
mod persistence;

pub use collaboration::*;
pub use demo::*;
pub use document::*;
pub use edit_ops::*;
pub use persistence::*;

use std::time::Instant;

const TIMING_ENV: &str = "DEBATEPROCESSOR_TIMING";

fn timing_enabled() -> bool {
  std::env::var_os(TIMING_ENV).is_some()
}

pub(crate) fn log_timing_lazy(label: &str, start: Instant, detail: impl FnOnce() -> String) {
  if timing_enabled() {
    eprintln!("[timing] {label}: {:?} {}", start.elapsed(), detail());
  }
}
