mod comment_dialog;
mod document_panel;
pub mod document_search;
pub mod document_search_overlay;
mod file_management;
pub(crate) mod command_palette;
mod file_search_overlay;
mod icons;
mod revision_dialog;
mod workspace;

pub use workspace::{Workspace, install_workspace_close_prompt, open_workspace_window};
pub(crate) use workspace::PaletteEntry;
