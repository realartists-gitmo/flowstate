#[cfg(not(target_family = "wasm"))]
mod comment_dialog;
mod document_panel;
pub mod document_search;
pub mod document_search_overlay;
#[cfg(not(target_family = "wasm"))]
mod file_management;
#[cfg(not(target_family = "wasm"))]
mod file_search_overlay;
mod icons;
#[cfg(not(target_family = "wasm"))]
mod revision_dialog;
mod workspace;

pub use workspace::{Workspace, open_workspace_window};
#[cfg(not(target_family = "wasm"))]
pub use workspace::install_workspace_close_prompt;
