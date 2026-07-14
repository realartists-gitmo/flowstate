#[hotpath::measure]
pub(super) fn measure_line_width(
  document: &DocumentProjection,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  paragraph_text: &str,
  source_range: Range<usize>,
  rendered_len: usize,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> Pixels {
  let mut width = px(0.0);
  let rendered_start = clamp_to_char_boundary(paragraph_text, source_range.start);
  let rendered_end = clamp_to_char_boundary(
    paragraph_text,
    source_range
      .start
      .saturating_add(rendered_len)
      .min(source_range.end)
      .min(paragraph_text.len()),
  )
  .max(rendered_start);
  let rendered_range = rendered_start..rendered_end;
  let rendered_text = &paragraph_text[rendered_range.clone()];
  let measure_key = LineMeasureCacheKey {
    start: rendered_range.start,
    end: rendered_range.end,
  };
  if let Some(width) = shape_cache.line_widths.get(&measure_key) {
    return *width;
  }
  let mut fragments = std::mem::take(&mut shape_cache.fragment_scratch);
  formatted_fragments_for_range_into(
    document,
    p_format,
    paragraph,
    &rendered_range,
    rendered_text,
    &mut fragments,
    &mut shape_cache.run_formats,
  );
  for (fragment_ix, fragment) in fragments.iter().enumerate() {
    let text = &rendered_text[fragment.fragment.line_range.clone()];
    if text.is_empty() {
      continue;
    }
    let run_start = clamp_to_char_boundary(paragraph_text, fragment.fragment.run_range.start.min(paragraph_text.len()));
    let run_end = clamp_to_char_boundary(paragraph_text, fragment.fragment.run_range.end.min(paragraph_text.len())).max(run_start);
    let run_text = &paragraph_text[run_start..run_end];
    let shaped = shape_fragment_cached(window, run_text, &fragment.format, run_start, fragment.fragment.styles, shape_cache);
    let fragment_start = fragment
      .fragment
      .source_start
      .saturating_sub(run_start)
      .min(run_text.len());
    let fragment_end = fragment_start
      .saturating_add(text.len())
      .min(run_text.len());
    let fragment_width = (shaped.x_for_index(fragment_end) - shaped.x_for_index(fragment_start)).max(px(0.0));
    let (box_pad_left, box_pad_right) = boxed_fragment_padding(&fragments, fragment_ix, document.theme.box_padding_left, document.theme.box_padding_right);
    width += box_pad_left;
    width += fragment_width;
    width += box_pad_right;
  }
  shape_cache.line_widths.insert(measure_key, width);
  fragments.clear();
  shape_cache.fragment_scratch = fragments;
  width
}

#[hotpath::measure]
pub(super) fn shape_line(
  document: &DocumentProjection,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  line_text: &str,
  source_range: Range<usize>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutLine {
  let mut fragments = std::mem::take(&mut shape_cache.fragment_scratch);
  formatted_fragments_for_range_into(
    document,
    &p_format,
    paragraph,
    &source_range,
    line_text,
    &mut fragments,
    &mut shape_cache.run_formats,
  );
  let mut x = px(0.0);
  let mut segments = Vec::with_capacity(fragments.len().max(1));

  for (fragment_ix, fragment) in fragments.iter().enumerate() {
    let text = &line_text[fragment.fragment.line_range.clone()];
    if text.is_empty() {
      continue;
    }
    let format = &fragment.format;
    let shaped = shape_fragment_cached(window, text, format, fragment.fragment.source_start, fragment.fragment.styles, shape_cache);
    let width = shaped.width;
    let (box_pad_left, box_pad_right) = boxed_fragment_padding(&fragments, fragment_ix, document.theme.box_padding_left, document.theme.box_padding_right);
    let segment_ascent = shaped.ascent;
    let segment_descent = shaped.descent;
    x += box_pad_left;
    segments.push(LaidOutSegment {
      shaped,
      x,
      width,
      box_pad_left,
      box_pad_right,
      format: format.clone(),
      ascent: segment_ascent,
      descent: segment_descent,
      font_size: format.font_size,
      start_byte: fragment.fragment.source_start,
    });
    x += width + box_pad_right;
  }

  if segments.is_empty() {
    let format = run_format(document, &p_format, RunStyles::default());
    let shaped = std::sync::Arc::new(shape_fragment(window, "", &format));
    #[cfg(target_os = "linux")]
    let (segment_ascent, segment_descent) = {
      let (font_ascent, font_descent) = font_metrics_for_format(&format, cx);
      (shaped.ascent.max(font_ascent), shaped.descent.max(font_descent))
    };
    #[cfg(not(target_os = "linux"))]
    let (segment_ascent, segment_descent) = (shaped.ascent, shaped.descent);
    segments.push(LaidOutSegment {
      shaped,
      format: format.clone(),
      x: px(0.0),
      width: px(0.0),
      box_pad_left: px(0.0),
      box_pad_right: px(0.0),
      ascent: segment_ascent,
      descent: segment_descent,
      font_size: format.font_size,
      start_byte: source_range.start,
    });
  }

  // §perf: ascent/descent derive solely from the final segment set, so compute them
  // once here instead of also max-accumulating them per segment inside the loop above.
  let ascent = segments
    .iter()
    .map(|segment| segment.ascent)
    .fold(px(0.0), Pixels::max);
  let descent = segments
    .iter()
    .map(|segment| segment.descent)
    .fold(px(0.0), Pixels::max);

  let max_font_size = segments
    .iter()
    .map(|segment| segment.font_size)
    .fold(p_format.font_size, Pixels::max);
  let line_gap = max_font_size * document.theme.line_gap_fraction;
  let line_height = (ascent + descent + line_gap) * p_format.line_spacing;
  let mut line = LaidOutLine {
    origin: point(px(0.0), px(0.0)),
    line_height,
    ascent,
    descent,
    width: x,
    start_byte: source_range.start,
    end_byte: source_range.end,
    segments,
    rects: Vec::new(),
    underlines: Vec::new(),
    strikethroughs: Vec::new(),
  };
  line.rects = rects_for_line(document, &line);
  line.underlines = underlines_for_line(document, &line, cx);
  line.strikethroughs = strikethroughs_for_line(document, &line);
  fragments.clear();
  shape_cache.fragment_scratch = fragments;
  line
}

#[derive(Default)]
pub(super) struct FragmentShapeCache {
  // Arc'd values: a cache HIT previously deep-cloned the `ShapedLine`
  // (glyph runs + decorations, ~1.4KB) — ~1.3GB of pure hit-path churn per
  // profiled session. An Arc bump serves the same layout.
  shapes: FxHashMap<FragmentShapeCacheKey, std::sync::Arc<ShapedLine>>,
  line_widths: FxHashMap<LineMeasureCacheKey, Pixels>,
  fragment_scratch: Vec<FormattedFragment>,
  run_formats: FxHashMap<RunStyles, EffectiveRunFormat>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct FragmentShapeCacheKey {
  source_start: usize,
  len: usize,
  styles: RunStyles,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LineMeasureCacheKey {
  start: usize,
  end: usize,
}

#[hotpath::measure]
pub(super) fn shape_fragment_cached(
  window: &mut Window,
  text: &str,
  format: &EffectiveRunFormat,
  source_start: usize,
  styles: RunStyles,
  cache: &mut FragmentShapeCache,
) -> std::sync::Arc<ShapedLine> {
  let key = FragmentShapeCacheKey {
    source_start,
    len: text.len(),
    styles,
  };
  if let Some(shaped) = cache.shapes.get(&key) {
    return std::sync::Arc::clone(shaped);
  }
  let shaped = std::sync::Arc::new(shape_fragment(window, text, format));
  cache.shapes.insert(key, std::sync::Arc::clone(&shaped));
  shaped
}

#[hotpath::measure]
pub(super) fn shape_fragment(window: &mut Window, text: &str, format: &EffectiveRunFormat) -> ShapedLine {
  let mut run_font = font(format.font_family.clone());
  run_font.weight = if format.bold { FontWeight::BOLD } else { FontWeight::NORMAL };
  run_font.style = if format.italic { FontStyle::Italic } else { FontStyle::Normal };
  let run = GpuiTextRun {
    len: text.len(),
    font: run_font,
    color: format.color,
    background_color: None,
    underline: None,
    strikethrough: None,
  };
  window
    .text_system()
    .shape_line(SharedString::new(text), format.font_size, &[run], None)
}

#[derive(Clone)]
pub(super) struct FormattedFragment {
  pub(super) fragment: VisualFragment,
  pub(super) format: EffectiveRunFormat,
}

#[cfg(test)]
#[hotpath::measure]
pub(super) fn formatted_fragments_for_range(
  document: &DocumentProjection,
  p_format: &EffectiveParagraphFormat,
  paragraph: &Paragraph,
  range: &Range<usize>,
  rendered_text: &str,
) -> Vec<FormattedFragment> {
  let mut fragments = Vec::with_capacity(paragraph.runs.len());
  let mut run_formats = FxHashMap::default();
  formatted_fragments_for_range_into(
    document,
    p_format,
    paragraph,
    range,
    rendered_text,
    &mut fragments,
    &mut run_formats,
  );
  fragments
}

#[hotpath::measure]
pub(super) fn formatted_fragments_for_range_into(
  document: &DocumentProjection,
  p_format: &EffectiveParagraphFormat,
  paragraph: &Paragraph,
  range: &Range<usize>,
  rendered_text: &str,
  fragments: &mut Vec<FormattedFragment>,
  run_formats: &mut FxHashMap<RunStyles, EffectiveRunFormat>,
) {
  fragments.clear();
  run_formats.clear();
  let mut byte_offset = 0;
  let rendered_len = rendered_text.len();
  fragments.reserve(paragraph.runs.len());
  for run in &paragraph.runs {
    let run_start = byte_offset;
    let run_end = byte_offset + run.len;
    byte_offset = run_end;
    let start = run_start.max(range.start);
    let end = run_end.min(range.end);
    if start >= end || rendered_len == 0 {
      continue;
    }
    let line_start = ceil_char_boundary(rendered_text, start.saturating_sub(range.start).min(rendered_len));
    let line_end = ceil_char_boundary(rendered_text, end.saturating_sub(range.start).min(rendered_len));
    if line_start >= line_end {
      continue;
    }
    let visual = VisualFragment {
      styles: run.styles,
      line_range: line_start..line_end,
      run_range: run_start..run_end,
      source_start: range.start + line_start,
    };
    let format = run_formats
      .entry(visual.styles)
      .or_insert_with(|| run_format(document, p_format, visual.styles))
      .clone();
    fragments.push(FormattedFragment {
      format,
      fragment: visual,
    });
  }
}

#[hotpath::measure]
pub(super) fn boxed_fragment_padding(
  fragments: &[FormattedFragment],
  fragment_ix: usize,
  box_padding_left: Pixels,
  box_padding_right: Pixels,
) -> (Pixels, Pixels) {
  let Some(fragment) = fragments.get(fragment_ix) else {
    return (px(0.0), px(0.0));
  };
  if fragment.format.border_width <= px(0.0) {
    return (px(0.0), px(0.0));
  }

  let has_previous_boxed_fragment = fragment_ix > 0 && fragments[fragment_ix - 1].format.border_width > px(0.0);
  let has_next_boxed_fragment = fragments
    .get(fragment_ix + 1)
    .is_some_and(|next| next.format.border_width > px(0.0));
  (
    if has_previous_boxed_fragment {
      px(0.0)
    } else {
      box_padding_left
    },
    if has_next_boxed_fragment {
      px(0.0)
    } else {
      box_padding_right
    },
  )
}

#[cfg(target_os = "linux")]
#[hotpath::measure]
fn font_metrics_for_format(format: &EffectiveRunFormat, cx: &mut App) -> (Pixels, Pixels) {
  let key = FontMetricsCacheKey {
    font_family: format.font_family.clone(),
    bold: format.bold,
    italic: format.italic,
    font_size: format.font_size,
  };
  let cache = LINUX_FONT_METRICS_CACHE.get_or_init(|| std::sync::Mutex::new(FxHashMap::default()));
  if let Some(metrics) = cache.lock().ok().and_then(|cache| cache.get(&key).copied()) {
    return metrics;
  }
  let mut run_font = font(format.font_family.clone());
  run_font.weight = if format.bold { FontWeight::BOLD } else { FontWeight::NORMAL };
  run_font.style = if format.italic { FontStyle::Italic } else { FontStyle::Normal };
  let font_id = cx.text_system().resolve_font(&run_font);
  let metrics = (
    cx.text_system().ascent(font_id, format.font_size),
    cx.text_system().descent(font_id, format.font_size),
  );
  if let Ok(mut cache) = cache.lock() {
    cache.insert(key, metrics);
  }
  metrics
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FontMetricsCacheKey {
  font_family: SharedString,
  bold: bool,
  italic: bool,
  font_size: Pixels,
}

#[cfg(target_os = "linux")]
static LINUX_FONT_METRICS_CACHE: std::sync::OnceLock<std::sync::Mutex<FxHashMap<FontMetricsCacheKey, (Pixels, Pixels)>>> =
  std::sync::OnceLock::new();

#[derive(Clone)]
pub(super) struct VisualFragment {
  pub(super) styles: RunStyles,
  pub(super) line_range: Range<usize>,
  pub(super) run_range: Range<usize>,
  pub(super) source_start: usize,
}

