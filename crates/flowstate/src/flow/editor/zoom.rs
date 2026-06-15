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
    if self.camera_center.is_none() {
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
      self.board_scroll.offset(),
      viewport,
      self.board_zoom,
    ));
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
  fn subpixel_grid_dots_fade_instead_of_becoming_heavier() {
    let (small_size, small_opacity) = grid_dot_metrics(0.25, 1.0);
    let (normal_size, normal_opacity) = grid_dot_metrics(1.0, 1.0);

    assert_eq!(small_size, px(1.0));
    assert!(small_opacity < normal_opacity);
    assert_eq!(normal_size, px(1.5));
    assert_eq!(normal_opacity, 1.0);
  }
}
