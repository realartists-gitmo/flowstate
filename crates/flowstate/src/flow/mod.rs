mod cell_theme;
pub mod editor;
pub mod panel;
pub mod ribbon;

pub use editor::{AnnotationTool, FlowEditor, FlowExternalPresence, FlowPresenceSnapshot};
pub use panel::FlowPanel;
pub use ribbon::FlowRibbon;

use gpui::{App, Hsla};
use gpui_component::ActiveTheme as _;

#[derive(Clone, Copy)]
pub(crate) struct FlowSidePalette {
  pub base: Hsla,
  pub foreground: Hsla,
  pub hover: Hsla,
  pub active: Hsla,
}

pub(crate) fn flow_side_palette(side: flowstate_flow::ArgumentSide, cx: &App) -> FlowSidePalette {
  match side {
    flowstate_flow::ArgumentSide::One => FlowSidePalette {
      base: cx.theme().primary,
      foreground: cx.theme().primary_foreground,
      hover: cx.theme().primary_hover,
      active: cx.theme().primary_active,
    },
    flowstate_flow::ArgumentSide::Two => FlowSidePalette {
      base: cx.theme().info,
      foreground: cx.theme().info_foreground,
      hover: cx.theme().info_hover,
      active: cx.theme().info_active,
    },
  }
}
