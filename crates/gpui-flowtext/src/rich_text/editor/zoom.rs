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
    self.zoom_percent = percent;
    self.document.theme.zoom_factor = percent / 100.0;
    self.invalidate_document_layout_caches();
    cx.notify();
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
