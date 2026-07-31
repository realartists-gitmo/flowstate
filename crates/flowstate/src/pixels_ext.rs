//! `Pixels` -> `f32`/`f64` conversion helpers.
//!
//! gpui-component provided this as `gpui_component::PixelsExt` up to v0.5.1 and
//! dropped it on the way to main. It is re-declared here rather than patched
//! back into `vendor/gpui-component` so the vendor stays closer to upstream —
//! this is app-side convenience, not component behaviour.

use gpui::Pixels;

/// A trait for converting [`Pixels`] to `f32` and `f64`.
pub trait PixelsExt {
  fn as_f32(&self) -> f32;
  fn as_f64(self) -> f64;
}

impl PixelsExt for Pixels {
  #[inline]
  fn as_f32(&self) -> f32 {
    f32::from(self)
  }

  #[inline]
  fn as_f64(self) -> f64 {
    f64::from(self)
  }
}
