use flowstate_document::{Document, DocumentTheme};
use gpui::{Hsla, transparent_black};
use palette::{FromColor as _, LinSrgb, Oklch, Srgb};

pub(super) fn apply_flow_cell_theme(document: &mut Document, client_theme: &DocumentTheme, foreground: Hsla, background: Hsla) {
  document.theme = client_theme.clone();
  let source_default = document.theme.default_text_color;
  document.theme.default_text_color = foreground;
  document.theme.document_background_color = transparent_black();
  document.theme.pageless_inset_x = gpui::px(0.0);
  document.theme.pageless_inset_top = gpui::px(0.0);
  document.theme.pageless_inset_bottom = gpui::px(0.0);
  document.theme.invisibility_visible_paragraph_styles.clear();
  for slot in [3, 4, 6] {
    document.theme.invisibility_visible_paragraph_styles.insert(slot);
  }
  document.theme.invisibility_visible_semantic_styles.clear();
  document.theme.invisibility_visible_semantic_styles.insert(1);
  document.theme.invisibility_visible_highlight_styles.clear();
  document.theme.default_highlight_color = transform_color(document.theme.default_highlight_color, source_default, foreground, background, false);

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

fn transform_color(source: Hsla, source_default: Hsla, target_default: Hsla, background: Hsla, enforce_text_contrast: bool) -> Hsla {
  let source = to_oklch(source);
  let source_default = to_oklch(source_default);
  let target_default = to_oklch(target_default);
  let transformed = Oklch::new(
    (target_default.0.l + source.0.l - source_default.0.l).clamp(0.0, 1.0),
    (target_default.0.chroma + source.0.chroma - source_default.0.chroma).max(0.0),
    target_default.0.hue + (source.0.hue - source_default.0.hue),
  );
  let transformed = from_oklch(transformed, source.1);
  if enforce_text_contrast {
    ensure_contrast(transformed, background, 3.0)
  } else {
    transformed
  }
}

fn to_oklch(color: Hsla) -> (Oklch, f32) {
  let rgb = color.to_rgb();
  let linear: LinSrgb = Srgb::new(rgb.r, rgb.g, rgb.b).into_linear();
  (Oklch::from_color(linear), color.a)
}

fn from_oklch(color: Oklch, alpha: f32) -> Hsla {
  let rgb: Srgb = LinSrgb::from_color(color).into_encoding();
  Hsla::from(gpui::Rgba {
    r: rgb.red.clamp(0.0, 1.0),
    g: rgb.green.clamp(0.0, 1.0),
    b: rgb.blue.clamp(0.0, 1.0),
    a: alpha,
  })
}

fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
  let luminance = |color: Hsla| {
    let rgb = color.to_rgb();
    let linear = |channel: f32| {
      if channel <= 0.04045 {
        channel / 12.92
      } else {
        ((channel + 0.055) / 1.055).powf(2.4)
      }
    };
    0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b)
  };
  let foreground = luminance(foreground);
  let background = luminance(background);
  (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

fn ensure_contrast(color: Hsla, background: Hsla, minimum_ratio: f32) -> Hsla {
  if color.a <= 0.5 || contrast_ratio(color, background) >= minimum_ratio {
    return color;
  }
  let mut dark = color;
  dark.l = 0.0;
  let mut light = color;
  light.l = 1.0;
  let target_lightness = if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
    1.0
  } else {
    0.0
  };
  let mut lower = 0.0;
  let mut upper = 1.0;
  for _ in 0..16 {
    let amount = (lower + upper) / 2.0;
    let mut candidate = color;
    candidate.l = color.l + (target_lightness - color.l) * amount;
    if contrast_ratio(candidate, background) >= minimum_ratio {
      upper = amount;
    } else {
      lower = amount;
    }
  }
  let mut adjusted = color;
  adjusted.l = color.l + (target_lightness - color.l) * upper;
  adjusted
}

#[cfg(test)]
mod tests {
  use super::*;
  use gpui::rgba;

  #[test]
  fn transformed_style_colors_preserve_alpha() {
    let source = Hsla::from(rgba(0x3366_994d));
    let transformed = transform_color(
      source,
      Hsla::from(rgba(0x0000_00ff)),
      Hsla::from(rgba(0xeeee_eeff)),
      Hsla::from(rgba(0x1111_11ff)),
      false,
    );
    assert!((transformed.a - source.a).abs() < f32::EPSILON);
  }

  #[test]
  fn transformed_opaque_text_remains_readable() {
    let background = Hsla::from(rgba(0xf8f8_f8ff));
    let transformed = transform_color(
      Hsla::from(rgba(0x5555_55ff)),
      Hsla::from(rgba(0x0000_00ff)),
      Hsla::from(rgba(0xf0f0_f0ff)),
      background,
      true,
    );
    assert!(contrast_ratio(transformed, background) >= 3.0);
  }

  #[test]
  fn transformed_style_hues_remain_distinct_with_achromatic_theme_text() {
    let source_default = Hsla::from(rgba(0x0000_00ff));
    let target_default = Hsla::from(rgba(0xffff_ffff));
    let background = Hsla::from(rgba(0x1111_11ff));
    let one = transform_color(Hsla::from(rgba(0x2563_ebff)), source_default, target_default, background, true).to_rgb();
    let two = transform_color(Hsla::from(rgba(0xdc26_26ff)), source_default, target_default, background, true).to_rgb();
    let distance = (one.r - two.r).abs() + (one.g - two.g).abs() + (one.b - two.b).abs();
    assert!(distance > 0.25);
  }
}
