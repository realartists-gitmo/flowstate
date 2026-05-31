//! Reusable library surface for the Flowstate editor.
//!
//! The binary in `main.rs` is intentionally thin: it parses CLI arguments and
//! calls into this library. The future full editor can depend on this crate,
//! create a `RichTextEditor`, and render it through `RichTextEditorView`.

pub mod app;
pub mod app_settings;
pub mod commands;
pub mod docx_conversion;
pub mod file_search;
pub mod flow;
pub mod ribbon;
pub mod rich_text_element;
pub mod workspace;

pub use app::{RichTextEditorView, register_rich_text_editor_keybindings, run_standalone, write_demo_document};
pub use rich_text_element::*;
