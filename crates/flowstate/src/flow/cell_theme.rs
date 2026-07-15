use flowstate_document::{DocumentProjection, DocumentTheme};
use gpui::{Hsla, transparent_black};

// D-S2: the color machinery lives in the app-wide visual engine now; this
// module keeps only the flow-specific pieces (its public shape is frozen
// while the flow editor WIP is out).
use crate::visual_engine::{composite_over, contrast_ratio, mix_srgb, transform_color};

pub(super) fn apply_flow_cell_theme(
  document: &mut DocumentProjection,
  client_theme: &DocumentTheme,
  foreground: Hsla,
  background: Hsla,
  zoom: f32,
) {
  document.theme = client_theme.clone();
  document.theme.zoom_factor *= zoom;
  scale_flow_layout_metrics(&mut document.theme, zoom);
  let source_default = document.theme.default_text_color;
  document.theme.default_text_color = foreground;
  document.theme.document_background_color = transparent_black();
  document.theme.pageless_inset_x = gpui::px(0.0);
  document.theme.pageless_inset_top = gpui::px(0.0);
  document.theme.pageless_inset_bottom = gpui::px(0.0);
  document.theme.invisibility_visible_paragraph_styles.clear();
  for slot in [3, 4, 6] {
    document
      .theme
      .invisibility_visible_paragraph_styles
      .insert(slot);
  }
  document.theme.invisibility_visible_semantic_styles.clear();
  document
    .theme
    .invisibility_visible_semantic_styles
    .insert(1);
  document.theme.invisibility_visible_highlight_styles.clear();
  document.theme.default_highlight_color =
    transform_color(document.theme.default_highlight_color, source_default, foreground, background, false);

  for style in document.theme.custom_paragraph_styles.values_mut() {
    style.color = transform_color(style.color, source_default, foreground, background, true);
  }
  for style in document.theme.custom_semantic_styles.values_mut() {
    if let Some(color) = style.color.as_mut() {
      *color = transform_color(*color, source_default, foreground, background, true);
    }
  }
  for style in document.theme.custom_highlight_styles.values_mut() {
    style.color = transform_color(style.color, source_default, foreground, background, false);
  }
}

fn scale_flow_layout_metrics(theme: &mut DocumentTheme, zoom: f32) {
  theme.paragraph_after *= zoom;
  for style in theme.custom_paragraph_styles.values_mut() {
    style.spacing_before *= zoom;
    style.spacing_after *= zoom;
    if let Some(border) = style.border.as_mut() {
      border.width *= zoom;
      border.space_x *= zoom;
      border.space_y *= zoom;
    }
  }
}

/// Part 4 visual system: the soft-fill borderless card, DERIVED from the
/// active theme at paint time (no stored colors anywhere).
pub(super) struct FlowCardVisuals {
  /// Composited card fill (side wash over the theme surface).
  pub fill: Hsla,
  /// Contrast guard: a 1px `theme().border` hairline when the fill-vs-canvas
  /// contrast collapses under the active theme.
  pub hairline: Option<Hsla>,
  /// Light themes elevate with a shadow; dark themes lift the fill instead.
  pub shadow: bool,
}

pub(super) fn flow_card_visuals(
  side_base: Hsla,
  background: Hsla,
  foreground: Hsla,
  border: Hsla,
  is_dark: bool,
  emphasis: f32,
) -> FlowCardVisuals {
  let mut fill = composite_over(side_base.opacity(0.08 + emphasis), background);
  if is_dark {
    // Elevation on dark themes: lift the fill ~4% toward the foreground.
    fill = mix_srgb(fill, foreground, 0.04);
  }
  let hairline = (contrast_ratio(fill, background) < 1.02).then_some(border);
  FlowCardVisuals {
    fill,
    hairline,
    shadow: !is_dark,
  }
}






#[cfg(test)]
mod tests {
  use super::*;
  use gpui::px;

  #[test]
  fn flow_layout_metrics_scale_with_board_zoom() {
    let mut theme = DocumentTheme {
      paragraph_after: px(12.0),
      ..DocumentTheme::default()
    };

    scale_flow_layout_metrics(&mut theme, 0.25);

    assert_eq!(theme.paragraph_after, px(3.0));
  }



}
