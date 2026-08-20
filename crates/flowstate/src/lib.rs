//! Reusable library surface for the Flowstate editor.
//!
//! The binary in `main.rs` is intentionally thin: it parses CLI arguments and
//! calls into this library. The future full editor can depend on this crate,
//! create a `RichTextEditor`, and render it through `RichTextEditorView`.

pub mod app;
pub mod app_settings;
#[cfg(not(target_family = "wasm"))]
pub mod collab;
pub mod commands;
#[cfg(not(target_family = "wasm"))]
pub mod docx_conversion;
#[cfg(not(target_family = "wasm"))]
pub mod file_search;
#[cfg(not(target_family = "wasm"))]
pub mod flow;
#[cfg(not(target_family = "wasm"))]
pub mod logging;
pub mod ribbon;
pub mod rich_text_element;
pub mod workspace;

#[cfg(target_family = "wasm")]
mod web;
#[cfg(target_family = "wasm")]
mod web_fonts;

#[cfg(not(target_family = "wasm"))]
pub use app::{RichTextEditorView, register_rich_text_editor_keybindings, run_standalone, write_demo_document};
pub use rich_text_element::*;
