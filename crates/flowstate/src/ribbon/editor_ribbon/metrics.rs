#[hotpath::measure_all]
impl RibbonLayoutMetrics {
  fn from_height(height: gpui::Pixels) -> Self {
    let height = clamp_pixels(height, min_ribbon_height(), max_ribbon_height());
    let scale =
      ((height.as_f32() - min_ribbon_height().as_f32()) / (max_ribbon_height().as_f32() - min_ribbon_height().as_f32())).clamp(0.0, 1.0);
    let group_padding_top = px(3.0 + 3.0 * scale);
    let chip_gap = px(2.0 + 4.0 * scale);
    let chip_height = px(20.0 + 10.0 * scale);
    // P4-S2 (labels pick: reclaim the band): group labels were measured and
    // height-reserved but never rendered — the 12px band goes back to chips.
    let group_label_height = px(0.0);
    let group_body_gap = px(3.0);
    let group_bottom_guard = px(5.0);
    let max_chip_rows = chip_rows_for_height(
      height,
      chip_height,
      chip_gap,
      group_padding_top,
      group_label_height,
      group_body_gap,
      group_bottom_guard,
    );
    let outer_padding_x = px(8.0);
    let inner_padding_x = px(8.0);
    let group_divider_padding_left = px(8.0);

    Self {
      height,
      chip_height,
      chip_max_width: px(112.0 + 40.0 * scale),
      chip_padding_x: px(3.0 + 7.0 * scale),
      chip_text_size: px(9.5 + 3.0 * scale),
      chip_gap,
      max_chip_rows,
      group_gap: px(4.0 + 7.0 * scale),
      group_padding_top,
      outer_padding_x,
      inner_padding_x,
      group_divider_padding_left,
    }
  }
}

#[hotpath::measure]
fn default_ribbon_height() -> gpui::Pixels {
  px(112.0)
}

#[hotpath::measure]
fn min_ribbon_height() -> gpui::Pixels {
  px(56.0)
}

#[hotpath::measure]
fn max_ribbon_height() -> gpui::Pixels {
  px(158.0)
}

#[hotpath::measure]
fn clamp_pixels(value: gpui::Pixels, min: gpui::Pixels, max: gpui::Pixels) -> gpui::Pixels {
  px(value.as_f32().clamp(min.as_f32(), max.as_f32()))
}

#[hotpath::measure]
fn group_outer_width(content_width: gpui::Pixels, has_divider: bool, metrics: RibbonLayoutMetrics) -> gpui::Pixels {
  let divider_chrome = if has_divider {
    metrics.group_divider_padding_left.as_f32() + 1.0
  } else {
    0.0
  };
  px(content_width.as_f32() + divider_chrome)
}

