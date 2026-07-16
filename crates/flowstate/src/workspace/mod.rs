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

pub use workspace::{
  Workspace, install_workspace_close_prompt, live_workspace_windows, open_workspace_window, request_quit_all_windows,
};
pub(crate) use workspace::{
  render_collaboration_bluetooth, render_collaboration_discovery_pause, render_collaboration_profile, render_collaboration_squads,
  render_trusted_collaborators,
};
pub(crate) use workspace::PaletteEntry;
