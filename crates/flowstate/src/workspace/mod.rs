mod document_panel;
pub mod document_search;
pub mod document_search_overlay;
mod file_management;
mod file_search_overlay;
mod icons;
mod workspace;

pub use workspace::{Workspace, install_workspace_close_prompt, open_workspace_window};
