//! D-S4: the motion framework — one curve family, one duration scale, one
//! reduced-motion gate (design-language decision: "orchestrated moments" on a
//! settled-organic base). Every app-level animation reads its timing from
//! here so the app has a single temperament instead of scattered constants.

use std::time::Duration;

use gpui::Animation;
use gpui_component::animation::cubic_bezier;

/// Quick state changes: chip selection, dot arrival, hover response.
pub const SETTLE_SHORT: Duration = Duration::from_millis(160);
/// Standard transitions: panel slide, pin snap, list settle.
pub const SETTLE: Duration = Duration::from_millis(200);
/// Composed sequences reserve longer beats (history takeover entry,
/// review-mode dress) — D-S5 builds on this.
pub const SEQUENCE_BEAT: Duration = Duration::from_millis(320);

/// The settle curve: fast attack, long soft landing. The app's signature
/// easing — pair it with [`SETTLE`]/[`SETTLE_SHORT`], nothing bounces.
pub fn settle_easing() -> impl Fn(f32) -> f32 {
  cubic_bezier(0.22, 1.0, 0.36, 1.0)
}

/// Whether motion is enabled. Honors the `reduce_motion` setting (no OS-level
/// signal is exposed by the toolkit on Linux); when reduced, durations
/// collapse to zero and animated reveals render in their final state.
#[must_use]
pub fn motion_enabled() -> bool {
  !crate::app_settings::load_app_settings().editor.reduce_motion
}

/// A settle animation honoring the reduced-motion gate: zero-duration when
/// motion is off (gpui renders the final frame immediately).
#[must_use]
pub fn settle_animation(duration: Duration) -> Animation {
  let duration = if motion_enabled() { duration } else { Duration::ZERO };
  Animation::new(duration).with_easing(settle_easing())
}
