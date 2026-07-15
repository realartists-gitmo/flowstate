//! D-S2: the Living Grid visual engine, promoted app-wide (design-language
//! decision L3). Every derived surface color in the app routes through here:
//! Oklch-space color transformation, WCAG contrast enforcement, sRGB
//! compositing, and THE ELEVATION LAW — light themes elevate with shadows,
//! dark themes lift the fill toward the foreground instead. Extracted from
//! `flow/cell_theme.rs`, which now delegates (its public shape is frozen while
//! the flow editor WIP is out).

use gpui::Hsla;
use gpui_component::{ActiveTheme as _, theme::Theme};
use palette::{FromColor as _, LinSrgb, Oklch, Srgb};

/// A derived chrome surface: the composited fill, an optional contrast-guard
/// hairline, and whether this surface casts a shadow (the elevation law).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceVisuals {
  /// Composited surface fill (wash over the theme background).
  pub fill: Hsla,
  /// Contrast guard: a 1px hairline when the fill-vs-canvas contrast
  /// collapses under the active theme.
  pub hairline: Option<Hsla>,
  /// Light themes elevate with a shadow; dark themes lift the fill instead.
  pub shadow: bool,
}

/// The elevation law, parameterized. `wash` tints the surface (pass the
/// theme background itself for a neutral card); `emphasis` adds wash alpha
/// on top of the 0.08 base (0.0 = resting surface).
#[must_use]
pub fn derive_surface(wash: Hsla, background: Hsla, foreground: Hsla, border: Hsla, is_dark: bool, emphasis: f32) -> SurfaceVisuals {
  let mut fill = composite_over(wash.opacity(0.08 + emphasis), background);
  if is_dark {
    // Elevation on dark themes: lift the fill ~4% toward the foreground.
    fill = mix_srgb(fill, foreground, 0.04);
  }
  let hairline = (contrast_ratio(fill, background) < 1.02).then_some(border);
  SurfaceVisuals {
    fill,
    hairline,
    shadow: !is_dark,
  }
}

/// [`derive_surface`] fed from the active gpui-component theme.
#[must_use]
pub fn surface_from_theme(theme: &Theme, wash: Hsla, emphasis: f32) -> SurfaceVisuals {
  derive_surface(
    wash,
    theme.background,
    theme.foreground,
    theme.border,
    theme.mode.is_dark(),
    emphasis,
  )
}

/// [`surface_from_theme`] with the theme read from the app context.
#[must_use]
pub fn chrome_surface(cx: &gpui::App, wash: Hsla, emphasis: f32) -> SurfaceVisuals {
  surface_from_theme(cx.theme(), wash, emphasis)
}

/// Straight alpha-over compositing in encoded sRGB (UI-surface math).
#[must_use]
pub fn composite_over(over: Hsla, under: Hsla) -> Hsla {
  let over_rgb = over.to_rgb();
  let under_rgb = under.to_rgb();
  let alpha = over_rgb.a;
  Hsla::from(gpui::Rgba {
    r: over_rgb.r * alpha + under_rgb.r * (1.0 - alpha),
    g: over_rgb.g * alpha + under_rgb.g * (1.0 - alpha),
    b: over_rgb.b * alpha + under_rgb.b * (1.0 - alpha),
    a: 1.0,
  })
}

#[must_use]
pub fn mix_srgb(base: Hsla, toward: Hsla, amount: f32) -> Hsla {
  let base_rgb = base.to_rgb();
  let toward_rgb = toward.to_rgb();
  Hsla::from(gpui::Rgba {
    r: base_rgb.r + (toward_rgb.r - base_rgb.r) * amount,
    g: base_rgb.g + (toward_rgb.g - base_rgb.g) * amount,
    b: base_rgb.b + (toward_rgb.b - base_rgb.b) * amount,
    a: 1.0,
  })
}

/// Re-express a themed color relative to a NEW default text color, preserving
/// the source's lightness/chroma/hue offsets in Oklch space; optionally
/// contrast-guard the result against `background` (text needs >= 3.0).
#[must_use]
pub fn transform_color(source: Hsla, source_default: Hsla, target_default: Hsla, background: Hsla, enforce_text_contrast: bool) -> Hsla {
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

#[must_use]
pub fn to_oklch(color: Hsla) -> (Oklch, f32) {
  let rgb = color.to_rgb();
  let linear: LinSrgb = Srgb::new(rgb.r, rgb.g, rgb.b).into_linear();
  (Oklch::from_color(linear), color.a)
}

#[must_use]
pub fn from_oklch(color: Oklch, alpha: f32) -> Hsla {
  let rgb: Srgb = LinSrgb::from_color(color).into_encoding();
  Hsla::from(gpui::Rgba {
    r: rgb.red.clamp(0.0, 1.0),
    g: rgb.green.clamp(0.0, 1.0),
    b: rgb.blue.clamp(0.0, 1.0),
    a: alpha,
  })
}

#[must_use]
pub fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
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

/// Binary-search the lightness that clears `minimum_ratio` against
/// `background`, moving toward whichever pole (black/white) contrasts more.
#[must_use]
pub fn ensure_contrast(color: Hsla, background: Hsla, minimum_ratio: f32) -> Hsla {
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

  #[test]
  fn elevation_law_shadows_light_and_lifts_dark() {
    let background_light = Hsla::from(rgba(0xf5f5_f0ff));
    let background_dark = Hsla::from(rgba(0x1616_18ff));
    let foreground_light = Hsla::from(rgba(0x2222_22ff));
    let foreground_dark = Hsla::from(rgba(0xeeee_eeff));
    let border = Hsla::from(rgba(0x8888_88ff));
    let wash = Hsla::from(rgba(0x3366_99ff));

    let light = derive_surface(wash, background_light, foreground_light, border, false, 0.0);
    let dark = derive_surface(wash, background_dark, foreground_dark, border, true, 0.0);

    assert!(light.shadow, "light themes elevate with shadows");
    assert!(!dark.shadow, "dark themes lift the fill instead");
    // The dark lift moves the fill toward the foreground (lighter).
    let unlifted = composite_over(wash.opacity(0.08), background_dark);
    assert!(dark.fill.to_rgb().r >= unlifted.to_rgb().r);
  }
}
