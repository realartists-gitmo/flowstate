//! The intent vocabulary lives in `gpui_flowtext::local_intents` (the editor
//! constructs intents; crate dependency direction requires the types there).
//! This module re-exports it so the write path's internal imports stay stable.

pub use gpui_flowtext::local_intents::*;
