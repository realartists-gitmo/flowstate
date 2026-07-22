//! Headless GPUI tests for the workspace/view layer.
//!
//! These run the REAL app wiring — `open_workspace_window`, the real
//! `Workspace`, real dialogs — under `gpui::TestAppContext` (no GPU, no
//! fonts, no real config dir). They exist because this layer had zero
//! coverage: the collab soaks enter below the workspace, so a double-lease
//! panic in dialog wiring shipped to the field (share-dialog constructor
//! calling `workspace.update` inside the workspace's own update).
//!
//! Ground rules for adding tests here:
//! - Go through `support::open_workspace` so config/data dirs stay sandboxed.
//! - Never install the app's custom prompt renderer: prompts must reach the
//!   test platform queue for `simulate_prompt_answer`.
//! - Discovery is force-paused by the sandbox settings; don't unpause it.

mod support;

mod actions;
mod collab_glue;
mod cutting;
mod comments;
mod dialogs;
mod documents;
mod flow_interaction;
mod flows_and_keystrokes;
mod history;
mod tabs_and_overlays;
mod windows;
