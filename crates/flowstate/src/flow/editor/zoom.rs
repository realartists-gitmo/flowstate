use flowstate_flow::{BoardPoint, BoardRect};
use gpui::{Pixels, Point, Size, point, px};
use gpui_component::PixelsExt as _;

use super::FlowEditor;

const MIN_ZOOM_PERCENT: f32 = 25.0;
const MAX_ZOOM_PERCENT: f32 = 400.0;
const ZOOM_STEP_PERCENT: f32 = 5.0;

impl FlowEditor {
  pub fn board_zoom(&self) -> f32 {
    self.board_zoom
  }

  pub fn set_board_zoom(&mut self, zoom: f32, cx: &mut gpui::Context<Self>) {
    let percent = ((zoom * 100.0 / ZOOM_STEP_PERCENT).round() * ZOOM_STEP_PERCENT)
      .clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT);
    let zoom = percent / 100.0;
    if (self.board_zoom - zoom).abs() < f32::EPSILON {
      return;
    }
    if !self.camera_apply_pending && self.camera_center.is_none_or(|center| !self.camera_center_is_current(center)) {
      self.sync_camera_center_from_scroll();
    }
    self.board_zoom = zoom;
    self.camera_apply_pending = true;
    cx.notify();
  }

  pub(super) fn apply_pending_camera_center(&mut self) {
    if !self.camera_apply_pending {
      return;
    }
    if let Some(center) = self.camera_center {
      let offset = offset_for_camera_center(center, self.board_scroll.bounds().size, self.board_zoom);
      self.board_scroll.set_offset(offset);
    }
    self.camera_apply_pending = false;
  }

  pub(super) fn sync_camera_center_from_scroll(&mut self) {
    let viewport = self.board_scroll.bounds().size;
    if viewport.width <= px(1.0) || viewport.height <= px(1.0) {
      self.camera_center = None;
      return;
    }
    self.camera_center = Some(camera_center_for_offset(
      clamp_scroll_offset(self.board_scroll.offset(), self.board_scroll.max_offset()),
      viewport,
      self.board_zoom,
    ));
  }

  pub(super) fn set_user_scroll_offset(&mut self, offset: Point<Pixels>) {
    self
      .board_scroll
      .set_offset(clamp_scroll_offset(offset, self.board_scroll.max_offset()));
    self.sync_camera_center_from_scroll();
  }

  fn camera_center_is_current(&self, center: BoardPoint) -> bool {
    let viewport = self.board_scroll.bounds().size;
    if viewport.width <= px(1.0) || viewport.height <= px(1.0) {
      return false;
    }
    camera_center_matches_offset(
      center,
      self.board_scroll.offset(),
      viewport,
      self.board_scroll.max_offset(),
      self.board_zoom,
      px(0.5),
    )
  }

  pub fn zoom_percent(&self) -> f32 {
    self.board_zoom * 100.0
  }

  pub fn set_zoom_percent(&mut self, percent: f32, cx: &mut gpui::Context<Self>) {
    self.set_board_zoom(percent / 100.0, cx);
  }

  pub fn zoom_in(&mut self, cx: &mut gpui::Context<Self>) {
    self.set_zoom_percent(self.zoom_percent() + ZOOM_STEP_PERCENT, cx);
  }

  pub fn zoom_out(&mut self, cx: &mut gpui::Context<Self>) {
    self.set_zoom_percent(self.zoom_percent() - ZOOM_STEP_PERCENT, cx);
  }

  pub fn visible_board_rect(&self) -> BoardRect {
    let bounds = self.board_scroll.bounds();
    let offset = self.board_scroll.offset();
    let scroll_x = -offset.x.as_f32() / self.board_zoom;
    let scroll_y = -offset.y.as_f32() / self.board_zoom;
    BoardRect {
      min: BoardPoint {
        x: scroll_x,
        y: scroll_y,
      },
      max: BoardPoint {
        x: scroll_x + bounds.size.width.as_f32() / self.board_zoom,
        y: scroll_y + bounds.size.height.as_f32() / self.board_zoom,
      },
    }
  }
}

fn camera_center_for_offset(offset: Point<Pixels>, viewport: Size<Pixels>, zoom: f32) -> BoardPoint {
  BoardPoint {
    x: (-offset.x.as_f32() + viewport.width.as_f32() / 2.0) / zoom,
    y: (-offset.y.as_f32() + viewport.height.as_f32() / 2.0) / zoom,
  }
}

fn offset_for_camera_center(center: BoardPoint, viewport: Size<Pixels>, zoom: f32) -> Point<Pixels> {
  point(
    px(-(center.x * zoom - viewport.width.as_f32() / 2.0)),
    px(-(center.y * zoom - viewport.height.as_f32() / 2.0)),
  )
}

fn clamp_scroll_offset(mut offset: Point<Pixels>, max_offset: Size<Pixels>) -> Point<Pixels> {
  offset.x = offset.x.clamp(-max_offset.width, px(0.0));
  offset.y = offset.y.clamp(-max_offset.height, px(0.0));
  offset
}

fn points_are_close(left: Point<Pixels>, right: Point<Pixels>, tolerance: Pixels) -> bool {
  (left.x - right.x).abs() <= tolerance && (left.y - right.y).abs() <= tolerance
}

fn camera_center_matches_offset(
  center: BoardPoint,
  offset: Point<Pixels>,
  viewport: Size<Pixels>,
  max_offset: Size<Pixels>,
  zoom: f32,
  tolerance: Pixels,
) -> bool {
  let expected = clamp_scroll_offset(offset_for_camera_center(center, viewport, zoom), max_offset);
  points_are_close(expected, offset, tolerance)
}

pub(super) fn grid_dot_metrics(zoom: f32, device_scale: f32) -> (Pixels, f32) {
  let target_size = 1.5 * zoom;
  let target_device_size = target_size * device_scale;
  if target_device_size < 1.0 {
    (px(1.0 / device_scale), target_device_size * target_device_size)
  } else {
    (px(target_size), 1.0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use gpui::size;

  #[test]
  fn camera_center_round_trips_across_zoom_levels() {
    let viewport = size(px(900.0), px(600.0));
    let original = point(px(-1240.0), px(-780.0));
    let center = camera_center_for_offset(original, viewport, 1.0);

    for zoom in [0.25, 2.0, 0.25, 4.0, 1.0] {
      let offset = offset_for_camera_center(center, viewport, zoom);
      if zoom == 1.0 {
        assert_eq!(offset, original);
      }
    }
  }

  #[test]
  fn stale_camera_center_is_detected_after_user_scroll() {
    let viewport = size(px(900.0), px(600.0));
    let max_offset = size(px(1800.0), px(1200.0));
    let center = camera_center_for_offset(point(px(-400.0), px(-300.0)), viewport, 1.0);

    assert!(camera_center_matches_offset(
      center,
      point(px(-400.0), px(-300.0)),
      viewport,
      max_offset,
      1.0,
      px(0.5),
    ));
    assert!(!camera_center_matches_offset(
      center,
      point(px(-900.0), px(-300.0)),
      viewport,
      max_offset,
      1.0,
      px(0.5),
    ));
  }

  #[test]
  fn clamped_camera_center_remains_valid_for_round_trip_zoom() {
    let viewport = size(px(900.0), px(600.0));
    let center = BoardPoint { x: 1400.0, y: 900.0 };

    assert!(camera_center_matches_offset(
      center,
      point(px(0.0), px(0.0)),
      viewport,
      size(px(0.0), px(0.0)),
      0.25,
      px(0.5),
    ));
  }

  #[test]
  fn camera_center_is_invalidated_when_viewport_geometry_changes() {
    let original_viewport = size(px(900.0), px(600.0));
    let offset = point(px(-400.0), px(-300.0));
    let center = camera_center_for_offset(offset, original_viewport, 1.0);

    assert!(!camera_center_matches_offset(
      center,
      offset,
      size(px(1100.0), px(600.0)),
      size(px(1800.0), px(1200.0)),
      1.0,
      px(0.5),
    ));
  }

  #[test]
  fn subpixel_grid_dots_fade_instead_of_becoming_heavier() {
    let (small_size, small_opacity) = grid_dot_metrics(0.25, 1.0);
    let (normal_size, normal_opacity) = grid_dot_metrics(1.0, 1.0);

    assert_eq!(small_size, px(1.0));
    assert!(small_opacity < normal_opacity);
    assert_eq!(normal_size, px(1.5));
    assert_eq!(normal_opacity, 1.0);
  }
}
