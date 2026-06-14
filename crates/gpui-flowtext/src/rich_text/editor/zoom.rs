const MIN_ZOOM_PERCENT: f32 = 25.0;
const MAX_ZOOM_PERCENT: f32 = 400.0;
const ZOOM_STEP_PERCENT: f32 = 5.0;

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn zoom_percent(&self) -> f32 {
    self.zoom_percent
  }

  pub fn set_zoom_percent(&mut self, percent: f32, cx: &mut Context<Self>) {
    let percent = ((percent / ZOOM_STEP_PERCENT).round() * ZOOM_STEP_PERCENT).clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT);
    if (self.zoom_percent - percent).abs() < f32::EPSILON {
      return;
    }
    if self.zoom_scroll_anchor.is_none() {
      self.zoom_scroll_anchor = self.capture_scroll_anchor();
    }
    self.zoom_percent = percent;
    self.document.theme.zoom_factor = percent / 100.0;
    self.zoom_anchor_apply_pending = true;
    self.invalidate_document_layout_caches();
    cx.notify();
  }

  pub(super) fn apply_pending_zoom_center(&mut self) {
    if !self.zoom_anchor_apply_pending {
      return;
    }
    self.restore_scroll_anchor(self.zoom_scroll_anchor.clone());
    self.zoom_anchor_apply_pending = false;
    self.zoom_scroll_anchor = None;
  }

  pub(super) fn sync_camera_center_from_scroll(&mut self) {
    self.zoom_scroll_anchor = None;
    self.zoom_anchor_apply_pending = false;
  }

  fn zoom_by(&mut self, delta_percent: f32, cx: &mut Context<Self>) {
    self.set_zoom_percent(self.zoom_percent + delta_percent, cx);
  }

  pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
    self.zoom_by(ZOOM_STEP_PERCENT, cx);
  }

  pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
    self.zoom_by(-ZOOM_STEP_PERCENT, cx);
  }
}
