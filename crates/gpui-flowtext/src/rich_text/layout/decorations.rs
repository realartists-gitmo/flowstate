#[hotpath::measure]
pub(super) fn rects_for_line(document: &DocumentProjection, line: &LaidOutLine) -> Vec<RunRect> {
  let mut backgrounds = Vec::new();
  let mut borders = Vec::new();
  let text_top = line.baseline_y() - line.ascent;
  let text_bottom = line.baseline_y() + line.descent;
  let max_font_size = line
    .segments
    .iter()
    .map(|segment| segment.font_size)
    .fold(px(0.0), Pixels::max);
  let bottom_pad = max_font_size * document.theme.highlight_bottom_extra_fraction;
  // Highlights share the same theoretical top as Word's inline run border:
  // even when no border is painted, the highlight should look like it fills
  // the box that would be drawn for the run.
  let paint_top = text_top - document.theme.box_padding_top;
  let paint_height = (text_bottom + bottom_pad - paint_top).max(px(1.0));

  for segment in &line.segments {
    let highlight_pad_left = if segment.format.border_width > px(0.0) {
      segment.box_pad_left
    } else {
      document.theme.highlight_pad_x
    };
    let highlight_pad_right = if segment.format.border_width > px(0.0) {
      segment.box_pad_right
    } else {
      document.theme.highlight_pad_x
    };
    let paint_box = Bounds::new(
      point(segment.x - highlight_pad_left, paint_top),
      size((segment.width + highlight_pad_left + highlight_pad_right).max(px(1.0)), paint_height),
    );

    if let Some(background) = segment.format.highlight {
      backgrounds.push(RunRect {
        bounds: paint_box,
        color: background,
        snap: RuleSnap::None,
      });
    }
    if segment.format.border_width > px(0.0) {
      let box_bounds = Bounds::new(
        point(segment.x - segment.box_pad_left, text_top - document.theme.box_padding_top),
        size(
          (segment.width + segment.box_pad_left + segment.box_pad_right).max(px(1.0)),
          (text_bottom - text_top + document.theme.box_padding_top + document.theme.box_padding_bottom).max(px(1.0)),
        ),
      );
      push_merged_box(&mut borders, InlineBorderBox { bounds: box_bounds, thickness: segment.format.border_width });
    }
  }
  let border_color = document.theme.default_text_color;
  // Word paints fills before border rules. Keeping all run borders after all
  // run highlights prevents a following highlighted run from hiding the right
  // edge of the previous boxed run.
  backgrounds.extend(
    borders
      .into_iter()
      .flat_map(|border| box_rules(border.bounds, border.thickness, border_color)),
  );
  backgrounds
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct InlineBorderBox {
  bounds: Bounds<Pixels>,
  thickness: Pixels,
}

#[hotpath::measure]
fn push_merged_box(boxes: &mut Vec<InlineBorderBox>, border: InlineBorderBox) {
  const EPSILON: f32 = 0.5;
  if let Some(last) = boxes.last_mut() {
    let same_band = (f32::from(last.bounds.origin.y) - f32::from(border.bounds.origin.y)).abs() <= EPSILON
      && (f32::from(last.bounds.size.height) - f32::from(border.bounds.size.height)).abs() <= EPSILON;
    let same_thickness = (f32::from(last.thickness) - f32::from(border.thickness)).abs() <= EPSILON;
    let touching = f32::from(border.bounds.origin.x) <= f32::from(last.bounds.origin.x + last.bounds.size.width) + EPSILON;
    if same_band && same_thickness && touching {
      let right = (last.bounds.origin.x + last.bounds.size.width).max(border.bounds.origin.x + border.bounds.size.width);
      last.bounds.size.width = right - last.bounds.origin.x;
      return;
    }
  }
  boxes.push(border);
}

#[hotpath::measure]
fn box_rules(bounds: Bounds<Pixels>, thickness: Pixels, color: Hsla) -> [RunRect; 4] {
  [
    RunRect {
      bounds: Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
      color,
      snap: RuleSnap::Horizontal,
    },
    RunRect {
      bounds: Bounds::new(
        point(bounds.origin.x, bounds.origin.y + bounds.size.height - thickness),
        size(bounds.size.width, thickness),
      ),
      color,
      snap: RuleSnap::Horizontal,
    },
    RunRect {
      bounds: Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
      color,
      snap: RuleSnap::Vertical,
    },
    RunRect {
      bounds: Bounds::new(
        point(bounds.origin.x + bounds.size.width - thickness, bounds.origin.y),
        size(thickness, bounds.size.height),
      ),
      color,
      snap: RuleSnap::Vertical,
    },
  ]
}

#[hotpath::measure]
pub(super) fn push_box_rules(rects: &mut Vec<RunRect>, bounds: Bounds<Pixels>, thickness: Pixels, color: Hsla) {
  rects.push(RunRect {
    bounds: Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
    color,
    snap: RuleSnap::Horizontal,
  });
  rects.push(RunRect {
    bounds: Bounds::new(
      point(bounds.origin.x, bounds.origin.y + bounds.size.height - thickness),
      size(bounds.size.width, thickness),
    ),
    color,
    snap: RuleSnap::Horizontal,
  });
  rects.push(RunRect {
    bounds: Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
    color,
    snap: RuleSnap::Vertical,
  });
  rects.push(RunRect {
    bounds: Bounds::new(
      point(bounds.origin.x + bounds.size.width - thickness, bounds.origin.y),
      size(thickness, bounds.size.height),
    ),
    color,
    snap: RuleSnap::Vertical,
  });
}

#[hotpath::measure]
pub(super) fn underlines_for_line(document: &DocumentProjection, line: &LaidOutLine, cx: &mut App) -> Vec<Decoration> {
  // §perf: the common prose line has no underlines, so start empty (no allocation
  // until the first push) rather than eagerly allocating a buffer that stays empty.
  let mut underlines = Vec::new();
  let baseline = line.baseline_y();
  for segment in &line.segments {
    match segment.format.underline {
      UnderlineKind::None => {},
      UnderlineKind::Single => {
        let (offset, thickness) = single_underline_metrics_for_segment(segment, document, cx);
        underlines.push(Decoration::from(DecorationSource {
          x: segment.x,
          width: segment.width,
          y: baseline + offset,
          thickness,
          color: document.theme.default_text_color,
        }));
      },
      UnderlineKind::Double => {
        let (offset, thickness) = double_underline_metrics_for_segment(document);
        let y = baseline + offset;
        underlines.push(Decoration::from(DecorationSource {
          x: segment.x,
          width: segment.width,
          y,
          thickness,
          color: document.theme.default_text_color,
        }));
        underlines.push(Decoration::from(DecorationSource {
          x: segment.x,
          width: segment.width,
          y: y + thickness + document.theme.double_underline_gap,
          thickness,
          color: document.theme.default_text_color,
        }));
      },
    }
  }
  merge_inline_decorations(underlines)
}

#[hotpath::measure]
pub(super) fn strikethroughs_for_line(document: &DocumentProjection, line: &LaidOutLine) -> Vec<Decoration> {
  let baseline = line.baseline_y();
  // §perf: most lines have no strikethrough; defer allocation until the first push.
  let mut decorations = Vec::new();
  for segment in &line.segments {
    if segment.format.strikethrough {
      let thickness = document.theme.underline_rule_thickness.max(px(1.0));
      let y = baseline - segment.font_size * 0.30;
      decorations.push(Decoration::from(DecorationSource {
        x: segment.x,
        width: segment.width,
        y,
        thickness,
        color: document.theme.default_text_color,
      }));
    }
  }
  merge_inline_decorations(decorations)
}

#[derive(Clone, Copy)]
pub(super) struct DecorationSource {
  pub(super) x: Pixels,
  pub(super) width: Pixels,
  pub(super) y: Pixels,
  pub(super) thickness: Pixels,
  pub(super) color: Hsla,
}

#[hotpath::measure_all]
impl From<DecorationSource> for Decoration {
  fn from(source: DecorationSource) -> Self {
    Self {
      bounds: Bounds::new(point(source.x, source.y), size(source.width.max(px(1.0)), source.thickness)),
      color: source.color,
    }
  }
}

#[hotpath::measure]
pub(super) fn merge_inline_decorations(decorations: Vec<Decoration>) -> Vec<Decoration> {
  let mut merged: Vec<Decoration> = Vec::with_capacity(decorations.len());
  for decoration in decorations {
    push_merged_decoration(&mut merged, decoration);
  }
  // Fidelity: underline/strikethrough rules are painted (paint.rs) at these
  // exact bounds, so a non-finite coordinate here paints a corrupt or invisible
  // rule that trails the model text. Gated; runs only when tracing is enabled.
  if flowstate_fidelity::enabled() {
    let class = flowstate_fidelity::FidelityClass::Structure;
    flowstate_fidelity::event(class, "decorations", || format!("count={}", merged.len()));
    let finite = merged.iter().all(|decoration| {
      f32::from(decoration.bounds.origin.x).is_finite()
        && f32::from(decoration.bounds.origin.y).is_finite()
        && f32::from(decoration.bounds.size.width).is_finite()
        && f32::from(decoration.bounds.size.height).is_finite()
    });
    flowstate_fidelity::check(finite, class, "decoration-geometry", || {
      format!("non-finite decoration bounds among {} decorations", merged.len())
    });
  }
  merged
}

#[hotpath::measure]
fn push_merged_decoration(decorations: &mut Vec<Decoration>, decoration: Decoration) {
  for existing in decorations.iter_mut().rev() {
    if !same_decoration_band(existing, &decoration) {
      continue;
    }
    const EPSILON: f32 = 0.75;
    let existing_left = f32::from(existing.bounds.origin.x);
    let existing_right = f32::from(existing.bounds.origin.x + existing.bounds.size.width);
    let decoration_left = f32::from(decoration.bounds.origin.x);
    let decoration_right = f32::from(decoration.bounds.origin.x + decoration.bounds.size.width);
    if decoration_left <= existing_right + EPSILON && decoration_right + EPSILON >= existing_left {
      let right = (existing.bounds.origin.x + existing.bounds.size.width).max(decoration.bounds.origin.x + decoration.bounds.size.width);
      existing.bounds.origin.x = existing.bounds.origin.x.min(decoration.bounds.origin.x);
      existing.bounds.size.width = right - existing.bounds.origin.x;
      return;
    }
    break;
  }
  decorations.push(decoration);
}

#[hotpath::measure]
fn same_decoration_band(a: &Decoration, b: &Decoration) -> bool {
  const EPSILON: f32 = 0.25;
  same_color(a.color, b.color)
    && (f32::from(a.bounds.origin.y) - f32::from(b.bounds.origin.y)).abs() <= EPSILON
    && (f32::from(a.bounds.size.height) - f32::from(b.bounds.size.height)).abs() <= EPSILON
}

#[hotpath::measure]
fn same_color(a: Hsla, b: Hsla) -> bool {
  a.h == b.h && a.s == b.s && a.l == b.l && a.a == b.a
}

#[hotpath::measure]
pub(super) fn single_underline_metrics_for_segment(segment: &LaidOutSegment, document: &DocumentProjection, cx: &mut App) -> (Pixels, Pixels) {
  // GPUI exposes glyph bounds in font coordinates. For Calibri, the
  // underscore bbox is below the baseline. The origin is the lower
  // edge of the glyph box on this metric path; Word positions an
  // underline at the top of the underscore glyph, so subtract the
  // glyph height from the baseline-to-origin distance.
  //
  // On Linux, GPUI's `typographic_bounds` is a stub returning
  // `origin = (0, 0)` with the advance box as the size (see gpui's
  // platform/linux/text_system.rs). That makes the formula collapse to 0
  // and paint the underline at the baseline, cutting through descenders.
  // So on Linux we skip the glyph-derived path entirely and use the
  // theme's Word-derived fallback constant.
  #[cfg(target_os = "linux")]
  let offset = {
    let _ = (segment, cx); // silence unused warnings on linux
    document.theme.underline_fallback_top_from_baseline
  };
  #[cfg(not(target_os = "linux"))]
  let offset = regular_underscore_bounds(segment, cx)
    .map(|bounds| (bounds.origin.y.abs() - bounds.size.height).max(px(0.0)))
    .unwrap_or(document.theme.underline_fallback_top_from_baseline);
  (offset, document.theme.underline_rule_thickness)
}

#[hotpath::measure]
pub(super) fn double_underline_metrics_for_segment(document: &DocumentProjection) -> (Pixels, Pixels) {
  (document.theme.double_underline_top_from_baseline, document.theme.underline_rule_thickness)
}

#[cfg(not(target_os = "linux"))]
#[hotpath::measure]
pub(super) fn regular_underscore_bounds(segment: &LaidOutSegment, cx: &mut App) -> Option<Bounds<Pixels>> {
  let mut underline_font = font(segment.format.font_family.clone());
  // Word's underline metric follows the regular face's underscore metrics;
  // bold text remains bold, but the underline itself does not get bolded.
  underline_font.weight = FontWeight::NORMAL;
  underline_font.style = if segment.format.italic { FontStyle::Italic } else { FontStyle::Normal };
  let font_id = cx.text_system().resolve_font(&underline_font);
  cx.text_system()
    .typographic_bounds(font_id, segment.font_size, '_')
    .ok()
}

