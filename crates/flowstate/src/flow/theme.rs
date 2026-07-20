//! The flow board's OWN color schema — a client-side palette, independent of
//! the app UI theme (`cx.theme()`) and, deliberately, of the `.fl0` document.
//!
//! Accessibility law: visual info attaches to the CLIENT, never to the doc. So
//! a `FlowTheme` is never serialized into a flow, never travels to peers, and
//! is not part of the format. It lives in app settings (one look per install),
//! exactly like the rich-text `DocumentTheme`, and is resolved fresh per render
//! from the cached settings (see `resolve_flow_theme`). Like the document
//! theme, it is FULLY independent of the app light/dark mode: the default is a
//! single fixed palette (`excel_light`), never chosen by `cx.theme()`. The dark
//! palette (`excel_dark`) is a user-selectable preset, not an automatic default.
//!
//! The default is the "Excel Classic" grid (Axis C2): NEUTRAL cells with the
//! aff/neg identity carried in the column header, crisp gridlines. The side
//! wash the flow shipped with is preserved as machinery — set `cell_wash > 0`
//! to bring it back — but is off by default.

use gpui::Hsla;

use super::FlowSidePalette;

/// One argument side's accent ramp (aff or neg). Shape matches the legacy
/// `FlowSidePalette` so every existing call site keeps compiling.
pub(crate) type FlowSideColors = FlowSidePalette;

#[derive(Clone, Copy)]
pub(crate) struct FlowTheme {
  /// The sheet: grid background AND the neutral cell fill (C2 cells are flat
  /// surface — color lives in the header, not the cell body).
  pub surface: Hsla,
  /// The hairline between cells. The ONLY separation — cells have no border.
  pub gridline: Hsla,
  /// Header/gutter bottom borders and the column-boundary separators. Slightly
  /// stronger than `gridline` so frozen chrome reads as an edge.
  pub chrome_border: Hsla,
  /// Neutral cell + body text (C2: cell text is not side-colored).
  pub text: Hsla,
  /// Row numbers, grip dots, clipped-ellipsis, drag hints.
  pub muted_text: Hsla,
  /// The frozen column-header band.
  pub header_bg: Hsla,
  /// The frozen row-number gutter band.
  pub gutter_bg: Hsla,
  /// Drop targets, row/column drag bars, row selection — the interaction accent
  /// (was `cx.theme().primary`). Distinct from the aff/neg side hues.
  pub selection: Hsla,
  /// Affirmative side (`ArgumentSide::One`) accent ramp.
  pub aff: FlowSideColors,
  /// Negative side (`ArgumentSide::Two`) accent ramp.
  pub neg: FlowSideColors,
  /// How strongly the side hue washes into a cell body. `0.0` = C2 neutral
  /// cells (default). Raise to reintroduce the tinted-field look; the fill
  /// machinery in `cell_theme::flow_cell_fill` honors it.
  pub cell_wash: f32,
}

/// Hex → `Hsla` (opaque), for the built-in defaults.
fn c(hex: u32) -> Hsla {
  gpui::rgb(hex).into()
}

impl FlowTheme {
  /// The side ramp for a column's argument side.
  pub fn side(&self, side: flowstate_flow::ArgumentSide) -> FlowSideColors {
    match side {
      flowstate_flow::ArgumentSide::One => self.aff,
      flowstate_flow::ArgumentSide::Two => self.neg,
    }
  }

  /// Excel Classic, light: white sheet, warm-grey gridlines, green aff / blue
  /// neg carried in the header.
  pub fn excel_light() -> Self {
    Self {
      surface: c(0xffffff),
      gridline: c(0xd4d4d4),
      chrome_border: c(0xc2c2c2),
      text: c(0x1f1f1f),
      muted_text: c(0x8a8a8a),
      header_bg: c(0xf3f3f3),
      gutter_bg: c(0xf3f3f3),
      selection: c(0x2f6fdb),
      aff: FlowSideColors {
        base: c(0x217346),
        foreground: c(0xffffff),
        hover: c(0x1b5e39),
        active: c(0x18512f),
      },
      neg: FlowSideColors {
        base: c(0x2f5597),
        foreground: c(0xffffff),
        hover: c(0x274a85),
        active: c(0x213f70),
      },
      cell_wash: 0.0,
    }
  }

  /// Excel Classic, dark: charcoal sheet, lifted gridlines, brighter accents so
  /// the header identity survives on a dark ground.
  pub fn excel_dark() -> Self {
    Self {
      surface: c(0x1b1d1f),
      gridline: c(0x3a3d40),
      chrome_border: c(0x4a4e52),
      text: c(0xe6e6e6),
      muted_text: c(0x9098a0),
      header_bg: c(0x232629),
      gutter_bg: c(0x232629),
      selection: c(0x4a8bf0),
      aff: FlowSideColors {
        base: c(0x3faa72),
        foreground: c(0x08110b),
        hover: c(0x4bbb82),
        active: c(0x2f9564),
      },
      neg: FlowSideColors {
        base: c(0x5b8fd6),
        foreground: c(0x0a1120),
        hover: c(0x6c9ce0),
        active: c(0x4a7ec4),
      },
      cell_wash: 0.0,
    }
  }
}

impl Default for FlowTheme {
  fn default() -> Self {
    Self::excel_light()
  }
}
