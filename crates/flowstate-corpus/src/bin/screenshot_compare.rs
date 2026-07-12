//! §act-eleven / A11.6 net: tolerance comparison of two raster screenshots.
//! `screenshot_compare <golden.png> <current.png>` exits 0 when the current
//! raster matches the golden within tolerance, 1 on divergence (with a report
//! and a diff heatmap written next to the current image), 2 on usage/shape
//! errors.
//!
//! Tolerances (env-tunable): antialiasing and subpixel text render slightly
//! differently across driver/font updates, so the gate is two-tier —
//! `FLOWSTATE_SCREENSHOT_MEAN_TOLERANCE` (mean absolute channel delta,
//! default 1.5/255) catches broad drift; `FLOWSTATE_SCREENSHOT_HOT_FRACTION`
//! (fraction of pixels whose max channel delta exceeds 32, default 0.5%)
//! catches localized breakage (a missing glyph run, a wrong quad) that a
//! whole-screen mean would dilute.

use std::process::ExitCode;

fn main() -> ExitCode {
  let mut args = std::env::args().skip(1);
  let (Some(golden_path), Some(current_path)) = (args.next(), args.next()) else {
    eprintln!("usage: screenshot_compare <golden.png> <current.png>");
    return ExitCode::from(2);
  };
  let golden = match image::open(&golden_path) {
    Ok(image) => image.into_rgba8(),
    Err(error) => {
      eprintln!("screenshot_compare: cannot open golden {golden_path}: {error}");
      return ExitCode::from(2);
    },
  };
  let current = match image::open(&current_path) {
    Ok(image) => image.into_rgba8(),
    Err(error) => {
      eprintln!("screenshot_compare: cannot open current {current_path}: {error}");
      return ExitCode::from(2);
    },
  };
  if golden.dimensions() != current.dimensions() {
    eprintln!(
      "screenshot_compare: dimensions differ — golden {:?} vs current {:?} (display mode changed? re-baseline with `heaven.sh screenshot-baseline`)",
      golden.dimensions(),
      current.dimensions()
    );
    return ExitCode::from(2);
  }

  let mean_tolerance: f64 = std::env::var("FLOWSTATE_SCREENSHOT_MEAN_TOLERANCE")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(1.5);
  let hot_fraction_tolerance: f64 = std::env::var("FLOWSTATE_SCREENSHOT_HOT_FRACTION")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(0.005);
  const HOT_DELTA: u8 = 32;

  let (width, height) = golden.dimensions();
  let mut total_delta: u64 = 0;
  let mut hot_pixels: u64 = 0;
  let mut heatmap = image::GrayImage::new(width, height);
  for (golden_pixel, current_pixel, heat) in itertools_free_zip(&golden, &current, &mut heatmap) {
    let mut max_channel_delta = 0u8;
    let mut pixel_delta = 0u64;
    for channel in 0..3 {
      let delta = golden_pixel.0[channel].abs_diff(current_pixel.0[channel]);
      pixel_delta += u64::from(delta);
      max_channel_delta = max_channel_delta.max(delta);
    }
    total_delta += pixel_delta;
    if max_channel_delta > HOT_DELTA {
      hot_pixels += 1;
    }
    heat.0[0] = max_channel_delta.saturating_mul(4);
  }
  let pixel_count = u64::from(width) * u64::from(height);
  let mean = total_delta as f64 / (pixel_count as f64 * 3.0);
  let hot_fraction = hot_pixels as f64 / pixel_count as f64;
  eprintln!(
    "screenshot_compare: {width}x{height} mean_delta={mean:.3} (tolerance {mean_tolerance}) hot_pixels={hot_pixels} ({:.4}%, tolerance {:.4}%)",
    hot_fraction * 100.0,
    hot_fraction_tolerance * 100.0
  );
  if mean <= mean_tolerance && hot_fraction <= hot_fraction_tolerance {
    return ExitCode::SUCCESS;
  }
  let heat_path = format!("{current_path}.diff.png");
  if heatmap.save(&heat_path).is_ok() {
    eprintln!("screenshot_compare: FAILED — diff heatmap written to {heat_path}");
  } else {
    eprintln!("screenshot_compare: FAILED");
  }
  ExitCode::from(1)
}

/// Zip three same-dimension images pixel-wise without extra deps.
fn itertools_free_zip<'imgs>(
  golden: &'imgs image::RgbaImage,
  current: &'imgs image::RgbaImage,
  heatmap: &'imgs mut image::GrayImage,
) -> impl Iterator<Item = (&'imgs image::Rgba<u8>, &'imgs image::Rgba<u8>, &'imgs mut image::Luma<u8>)> {
  golden
    .pixels()
    .zip(current.pixels())
    .zip(heatmap.pixels_mut())
    .map(|((golden_pixel, current_pixel), heat)| (golden_pixel, current_pixel, heat))
}
