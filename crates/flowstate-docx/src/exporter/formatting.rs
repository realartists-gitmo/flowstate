use docx_rs::{Run, RunFonts, Style};
use flowstate_document::{DocumentTheme, ThemeUnderline};
use gpui::{Hsla, Pixels};

#[hotpath::measure]
pub(super) fn apply_style_text_format(
  mut style: Style,
  theme: &DocumentTheme,
  size: Pixels,
  color: Hsla,
  bold: bool,
  italic: bool,
  underline: ThemeUnderline,
) -> Style {
  style = style
    .fonts(docx_fonts(theme))
    .size(half_points(size))
    .color(color_hex(color));
  if bold {
    style = style.bold();
  }
  if italic {
    style = style.italic();
  }
  apply_style_underline(style, underline)
}

#[hotpath::measure]
pub(super) fn apply_run_text_format(mut run: Run, size: Pixels, color: Hsla, bold: bool, italic: bool, underline: ThemeUnderline) -> Run {
  run = run.size(half_points(size)).color(color_hex(color));
  if bold {
    run = run.bold();
  }
  if italic {
    run = run.italic();
  }
  apply_run_underline(run, underline)
}

#[hotpath::measure]
fn apply_style_underline(style: Style, underline: ThemeUnderline) -> Style {
  match underline {
    ThemeUnderline::None => style,
    ThemeUnderline::Single => style.underline("single"),
    ThemeUnderline::Double => style.underline("double"),
  }
}

#[hotpath::measure]
fn apply_run_underline(run: Run, underline: ThemeUnderline) -> Run {
  match underline {
    ThemeUnderline::None => run,
    ThemeUnderline::Single => run.underline("single"),
    ThemeUnderline::Double => run.underline("double"),
  }
}

#[hotpath::measure]
pub(super) fn docx_fonts(theme: &DocumentTheme) -> RunFonts {
  let family = theme.default_font_family.to_string();
  RunFonts::new().ascii(family.clone()).hi_ansi(family)
}

#[hotpath::measure]
fn half_points(size: Pixels) -> usize {
  (pixels_to_pt(size) * 2.0).round().max(1.0) as usize
}

#[hotpath::measure]
pub(super) fn border_eighth_points(size: Pixels) -> usize {
  (pixels_to_pt(size) * 8.0).round().max(1.0) as usize
}

#[hotpath::measure]
pub(super) fn pixels_to_pt(value: Pixels) -> f32 {
  value.to_f64() as f32 * 72.0 / 96.0
}

#[hotpath::measure]
pub(super) fn color_hex(color: Hsla) -> String {
  let (r, g, b) = hsla_to_rgb(color);
  format!("{r:02X}{g:02X}{b:02X}")
}

#[hotpath::measure]
fn hsla_to_rgb(color: Hsla) -> (u8, u8, u8) {
  if color.s <= 0.0 {
    let value = color_channel(color.l);
    return (value, value, value);
  }
  let q = if color.l < 0.5 {
    color.l * (1.0 + color.s)
  } else {
    color.l + color.s - color.l * color.s
  };
  let p = 2.0 * color.l - q;
  (
    color_channel(hue_to_rgb(p, q, color.h + 1.0 / 3.0)),
    color_channel(hue_to_rgb(p, q, color.h)),
    color_channel(hue_to_rgb(p, q, color.h - 1.0 / 3.0)),
  )
}

#[hotpath::measure]
fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
  if t < 0.0 {
    t += 1.0;
  }
  if t > 1.0 {
    t -= 1.0;
  }
  if t < 1.0 / 6.0 {
    return p + (q - p) * 6.0 * t;
  }
  if t < 1.0 / 2.0 {
    return q;
  }
  if t < 2.0 / 3.0 {
    return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
  }
  p
}

#[hotpath::measure]
fn color_channel(value: f32) -> u8 {
  (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
