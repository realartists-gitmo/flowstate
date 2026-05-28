pub(super) fn rects_for_line(document: &Document, line: &LaidOutLine) -> Vec<RunRect> {
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
      push_merged_box(&mut borders, box_bounds);
    }
  }
  let border_color = document.theme.default_text_color;
  let border_thickness = document.theme.emphasis_border_paint_width;
  // Word paints fills before border rules. Keeping all run borders after all
  // run highlights prevents a following highlighted run from hiding the right
  // edge of the previous boxed run.
  backgrounds.extend(
    borders
      .into_iter()
      .flat_map(|bounds| box_rules(bounds, border_thickness, border_color)),
  );
  backgrounds
}

fn push_merged_box(boxes: &mut Vec<Bounds<Pixels>>, bounds: Bounds<Pixels>) {
  const EPSILON: f32 = 0.5;
  if let Some(last) = boxes.last_mut() {
    let same_band = (f32::from(last.origin.y) - f32::from(bounds.origin.y)).abs() <= EPSILON
      && (f32::from(last.size.height) - f32::from(bounds.size.height)).abs() <= EPSILON;
    let touching = f32::from(bounds.origin.x) <= f32::from(last.origin.x + last.size.width) + EPSILON;
    if same_band && touching {
      let right = (last.origin.x + last.size.width).max(bounds.origin.x + bounds.size.width);
      last.size.width = right - last.origin.x;
      return;
    }
  }
  boxes.push(bounds);
}

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

pub(super) fn underlines_for_line(document: &Document, line: &LaidOutLine, cx: &mut App) -> Vec<Decoration> {
  let mut underlines = Vec::with_capacity(line.segments.len().saturating_mul(2));
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

pub(super) fn strikethroughs_for_line(document: &Document, line: &LaidOutLine) -> Vec<Decoration> {
  let baseline = line.baseline_y();
  let mut decorations = Vec::with_capacity(line.segments.len());
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

impl From<DecorationSource> for Decoration {
  fn from(source: DecorationSource) -> Self {
    Self {
      bounds: Bounds::new(point(source.x, source.y), size(source.width.max(px(1.0)), source.thickness)),
      color: source.color,
    }
  }
}

pub(super) fn merge_inline_decorations(decorations: Vec<Decoration>) -> Vec<Decoration> {
  let mut merged: Vec<Decoration> = Vec::with_capacity(decorations.len());
  for decoration in decorations {
    push_merged_decoration(&mut merged, decoration);
  }
  merged
}

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

fn same_decoration_band(a: &Decoration, b: &Decoration) -> bool {
  const EPSILON: f32 = 0.25;
  same_color(a.color, b.color)
    && (f32::from(a.bounds.origin.y) - f32::from(b.bounds.origin.y)).abs() <= EPSILON
    && (f32::from(a.bounds.size.height) - f32::from(b.bounds.size.height)).abs() <= EPSILON
}

fn same_color(a: Hsla, b: Hsla) -> bool {
  a.h == b.h && a.s == b.s && a.l == b.l && a.a == b.a
}

pub(super) fn single_underline_metrics_for_segment(segment: &LaidOutSegment, document: &Document, cx: &mut App) -> (Pixels, Pixels) {
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

pub(super) fn double_underline_metrics_for_segment(document: &Document) -> (Pixels, Pixels) {
  (document.theme.double_underline_top_from_baseline, document.theme.underline_rule_thickness)
}

#[cfg(not(target_os = "linux"))]
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

