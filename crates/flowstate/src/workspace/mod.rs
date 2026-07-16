mod document_panel;
pub mod document_search;
pub mod document_search_overlay;
mod file_management;
pub(crate) mod command_palette;
pub(crate) mod comments_panel;
mod file_search_overlay;
mod icons;
pub mod history_takeover;
mod workspace;

pub use workspace::{Workspace, install_workspace_close_prompt, open_workspace_window};
pub(crate) use workspace::PaletteEntry;
