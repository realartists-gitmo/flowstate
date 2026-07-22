mod cell_theme;
pub mod editor;
pub mod panel;
pub mod ribbon;
pub mod sheet_strip;
mod theme;

pub use editor::{AnnotationTool, FlowEditor, FlowEditorEvent, FlowExternalPresence, FlowPresenceSnapshot, PenPreset};
pub(crate) use editor::{FlowPreview, preview_cell_ids, render_flow_board_preview, theme_flow_preview};
pub use panel::FlowPanel;
pub use ribbon::FlowRibbon;
pub use sheet_strip::FlowSheetStrip;
pub(crate) use theme::FlowTheme;

use gpui::Hsla;

#[derive(Clone, Copy)]
pub(crate) struct FlowSidePalette {
  pub base: Hsla,
  pub foreground: Hsla,
  pub hover: Hsla,
  pub active: Hsla,
}

/// The flow board's palette for the current render, resolved from app settings.
/// Takes NO `cx` — the flow palette is fully independent of the app UI theme
/// (its unset default is a single fixed palette, never mode-chosen). Cheap:
/// `load_flow_theme` reads only the cached `flow_theme` field.
pub(crate) fn resolve_flow_theme() -> FlowTheme {
  crate::app_settings::load_flow_theme()
}

/// The aff/neg accent ramp for a column's side, off the resolved flow palette.
pub(crate) fn flow_side_palette(side: flowstate_flow::ArgumentSide) -> FlowSidePalette {
  resolve_flow_theme().side(side)
}
