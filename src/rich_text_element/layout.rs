use std::{
  collections::{HashMap, hash_map::DefaultHasher},
  hash::{Hash, Hasher},
  ops::Range,
};

use gpui::{
  App, Bounds, FontStyle, FontWeight, Hsla, Pixels, Point, ShapedLine, SharedString, Size, TextRun as GpuiTextRun, Window, font, point, px, size,
};

use super::*;

#[derive(Clone)]
pub(super) struct LayoutState {
  pub(super) paragraphs: Vec<LaidOutParagraph>,
  pub(super) bounds: Option<Bounds<Pixels>>,
  pub(super) size: Size<Pixels>,
  pub(super) width: Pixels,
  pub(super) snap_underline_rules_to_pixels: bool,
}

impl LayoutState {
  pub(super) fn hit_test(&self, position: Point<Pixels>) -> DocumentOffset {
    let position = match self.bounds {
      Some(bounds) => position - bounds.origin,
      None => position,
    };
    let paragraph_ix = first_paragraph_with_bottom_at_or_after(&self.paragraphs, position.y);
    if let Some(paragraph) = self.paragraphs.get(paragraph_ix) {
      if position.y < paragraph.top {
        return DocumentOffset {
          paragraph: paragraph.index,
          byte: 0,
        };
      }
      return paragraph.hit_test(position);
    }
    let Some(last) = self.paragraphs.last() else {
      return DocumentOffset::default();
    };
    DocumentOffset {
      paragraph: last.index,
      byte: last.len,
    }
  }
}

#[derive(Clone)]
pub(super) struct LaidOutParagraph {
  pub(super) index: usize,
  pub(super) cache_key: ParagraphCacheKey,
  pub(super) len: usize,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) lines: Vec<LaidOutLine>,
  pub(super) borders: Vec<RunRect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ParagraphCacheKey {
  pub(super) fingerprint: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct ParagraphHeightCacheEntry {
  pub(super) key: ParagraphCacheKey,
  pub(super) width: Pixels,
  pub(super) height: Pixels,
}

pub(super) fn paragraph_cache_key(_document: &Document, paragraph: &Paragraph) -> ParagraphCacheKey {
  let mut hasher = DefaultHasher::new();
  paragraph.style.hash(&mut hasher);
  paragraph.version.hash(&mut hasher);
  ParagraphCacheKey {
    fingerprint: hasher.finish(),
  }
}

impl LaidOutParagraph {
  pub(super) fn shift_y(&mut self, new_top: Pixels) {
    let delta = new_top - self.top;
    self.top += delta;
    self.bottom += delta;
    for line in &mut self.lines {
      line.origin.y += delta;
    }
    for border in &mut self.borders {
      border.bounds.origin.y += delta;
    }
  }

  fn hit_test(&self, position: Point<Pixels>) -> DocumentOffset {
    let line_ix = first_line_with_bottom_at_or_after(&self.lines, position.y);
    if let Some(line) = self.lines.get(line_ix) {
      return DocumentOffset {
        paragraph: self.index,
        byte: line.hit_test_x(position.x - line.origin.x),
      };
    }
    DocumentOffset {
      paragraph: self.index,
      byte: self.len,
    }
  }
}

#[derive(Clone)]
pub(super) struct LaidOutLine {
  pub(super) origin: Point<Pixels>,
  pub(super) line_height: Pixels,
  pub(super) ascent: Pixels,
  pub(super) descent: Pixels,
  pub(super) width: Pixels,
  pub(super) start_byte: usize,
  pub(super) end_byte: usize,
  pub(super) segments: Vec<LaidOutSegment>,
  pub(super) rects: Vec<RunRect>,
  pub(super) underlines: Vec<Decoration>,
}

impl LaidOutLine {
  pub(super) fn baseline_y(&self) -> Pixels {
    ((self.line_height - self.ascent - self.descent) / 2.0) + self.ascent
  }

  pub(super) fn hit_test_x(&self, x: Pixels) -> usize {
    for segment in &self.segments {
      if x <= segment.x + segment.width {
        let local_x = (x - segment.x).max(px(0.0));
        return segment.start_byte + segment.shaped.closest_index_for_x(local_x);
      }
    }
    self.end_byte
  }
}

#[derive(Clone)]
pub(super) struct LaidOutSegment {
  pub(super) shaped: ShapedLine,
  pub(super) format: EffectiveRunFormat,
  pub(super) x: Pixels,
  pub(super) width: Pixels,
  pub(super) ascent: Pixels,
  pub(super) descent: Pixels,
  pub(super) font_size: Pixels,
  pub(super) start_byte: usize,
}

#[derive(Clone)]
pub(super) struct RunRect {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) color: Hsla,
  pub(super) snap: RuleSnap,
}

#[derive(Clone, Copy)]
pub(super) enum RuleSnap {
  None,
  Horizontal,
  Vertical,
}

#[derive(Clone)]
pub(super) struct Decoration {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) color: Hsla,
}

#[derive(Clone)]
pub(super) struct EffectiveParagraphFormat {
  pub(super) font_size: Pixels,
  pub(super) font_family: SharedString,
  pub(super) bold: bool,
  pub(super) italic: bool,
  pub(super) color: Hsla,
  pub(super) align: ParagraphAlign,
  pub(super) spacing_before: Pixels,
  pub(super) spacing_after: Pixels,
  pub(super) line_spacing: f32,
  pub(super) border: Option<ParagraphBorder>,
  pub(super) underline: UnderlineKind,
}

#[derive(Clone, Copy)]
pub(super) struct ParagraphBorder {
  width: Pixels,
  space_x: Pixels,
  space_y: Pixels,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ParagraphAlign {
  Left,
  Center,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum UnderlineKind {
  None,
  Single,
  Double,
}

#[derive(Clone)]
pub(super) struct EffectiveRunFormat {
  pub(super) font_size: Pixels,
  pub(super) font_family: SharedString,
  pub(super) bold: bool,
  pub(super) italic: bool,
  pub(super) color: Hsla,
  pub(super) underline: UnderlineKind,
  pub(super) highlight: Option<Hsla>,
  pub(super) border_width: Pixels,
}

pub(super) fn paragraph_format(document: &Document, style: ParagraphStyle) -> EffectiveParagraphFormat {
  let theme = &document.theme;
  let normal = EffectiveParagraphFormat {
    font_size: theme.body_font_size,
    font_family: theme.default_font_family.clone(),
    bold: false,
    italic: false,
    color: theme.default_text_color,
    align: ParagraphAlign::Left,
    spacing_before: px(0.0),
    spacing_after: theme.paragraph_after,
    line_spacing: theme.line_spacing,
    border: None,
    underline: UnderlineKind::None,
  };

  match style {
    ParagraphStyle::Normal => normal,
    ParagraphStyle::Pocket => EffectiveParagraphFormat {
      font_size: theme.pocket_font_size,
      bold: true,
      align: ParagraphAlign::Center,
      spacing_before: theme.pocket_before,
      spacing_after: px(0.0),
      border: Some(ParagraphBorder {
        width: theme.pocket_border_width,
        space_x: theme.pocket_border_space_x,
        space_y: theme.pocket_border_space_y,
      }),
      ..normal
    },
    ParagraphStyle::Hat => EffectiveParagraphFormat {
      font_size: theme.hat_font_size,
      bold: true,
      align: ParagraphAlign::Center,
      spacing_before: theme.hat_before,
      spacing_after: px(0.0),
      underline: UnderlineKind::Double,
      ..normal
    },
    ParagraphStyle::Block => EffectiveParagraphFormat {
      font_size: theme.block_font_size,
      bold: true,
      align: ParagraphAlign::Center,
      spacing_before: theme.block_before,
      spacing_after: px(0.0),
      underline: UnderlineKind::Single,
      ..normal
    },
    ParagraphStyle::Tag => EffectiveParagraphFormat {
      font_size: theme.tag_font_size,
      bold: true,
      spacing_before: theme.tag_before,
      spacing_after: px(0.0),
      ..normal
    },
    ParagraphStyle::Analytic => EffectiveParagraphFormat {
      font_size: theme.tag_font_size,
      bold: true,
      color: theme.analytic_color,
      spacing_before: theme.tag_before,
      spacing_after: px(0.0),
      ..normal
    },
    ParagraphStyle::Undertag => EffectiveParagraphFormat {
      font_size: theme.undertag_font_size,
      font_family: theme.default_font_family.clone(),
      italic: true,
      color: theme.undertag_color,
      spacing_after: px(0.0),
      ..normal
    },
  }
}

pub(super) fn run_format(document: &Document, paragraph: EffectiveParagraphFormat, styles: RunStyles) -> EffectiveRunFormat {
  let theme = &document.theme;
  let mut format = EffectiveRunFormat {
    font_size: paragraph.font_size,
    font_family: paragraph.font_family,
    bold: paragraph.bold,
    italic: paragraph.italic,
    color: paragraph.color,
    underline: paragraph.underline,
    highlight: styles.highlight.map(|highlight| match highlight {
      HighlightStyle::Spoken => theme.highlight_spoken,
      HighlightStyle::Insert => theme.highlight_insert,
      HighlightStyle::Alternative => theme.highlight_alternative,
    }),
    border_width: px(0.0),
  };

  if styles.style_underline {
    format.font_size = theme.body_font_size;
    format.bold = false;
    format.underline = UnderlineKind::Single;
  }
  if styles.direct_underline {
    format.underline = UnderlineKind::Single;
  }
  if styles.cite {
    format.font_size = theme.cite_font_size;
    format.bold = true;
    format.underline = UnderlineKind::None;
  }
  if styles.emphasis {
    format.font_family = theme.default_font_family.clone();
    format.font_size = theme.cite_font_size;
    format.bold = true;
    format.italic = false;
    format.underline = UnderlineKind::Single;
    format.border_width = theme.emphasis_border_width;
  }

  format
}

pub(super) fn build_layout(
  document: &Document,
  width: Pixels,
  previous_layout: Option<&LayoutState>,
  window: &mut Window,
  cx: &mut App,
) -> LayoutState {
  let timing = Instant::now();
  let mut y = document.theme.pageless_inset_top;
  let mut paragraphs = Vec::with_capacity(document.paragraphs.len());
  let mut max_width = width;
  let mut shaped_count = 0;
  let mut reused_count = 0;
  let previous_layout = previous_layout.filter(|layout| layout.width == width);

  for paragraph_ix in 0..document.paragraphs.len() {
    let previous_paragraph = previous_layout.and_then(|layout| layout.paragraphs.get(paragraph_ix));
    let (paragraph, next_y, paragraph_max_width, reused) = layout_paragraph_at(document, paragraph_ix, width, y, previous_paragraph, window, cx);
    if reused {
      reused_count += 1;
    } else {
      shaped_count += 1;
    }
    max_width = max_width.max(paragraph_max_width);
    y = next_y;
    paragraphs.push(paragraph);
  }

  let layout = LayoutState {
    paragraphs,
    bounds: None,
    size: size(max_width, y + document.theme.pageless_inset_bottom),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  log_timing(
    "build layout",
    timing,
    format!("paragraphs={} shaped={shaped_count} reused={reused_count}", layout.paragraphs.len()),
  );
  layout
}

pub(super) fn build_single_paragraph_layout(
  document: &Document,
  paragraph_ix: usize,
  width: Pixels,
  previous_layout: Option<&LayoutState>,
  window: &mut Window,
  cx: &mut App,
) -> LayoutState {
  let timing = Instant::now();
  let start_y = if paragraph_ix == 0 { document.theme.pageless_inset_top } else { px(0.0) };
  let previous_paragraph = previous_layout.and_then(|layout| paragraph_layout(layout, paragraph_ix));
  let (paragraph, mut height, max_width, reused) = layout_paragraph_at(document, paragraph_ix, width, start_y, previous_paragraph, window, cx);
  if paragraph_ix + 1 == document.paragraphs.len() {
    height += document.theme.pageless_inset_bottom;
  }
  let layout = LayoutState {
    paragraphs: vec![paragraph],
    bounds: None,
    size: size(max_width.max(width), height),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  log_timing(
    "build visible paragraph",
    timing,
    format!("paragraph={paragraph_ix} shaped={} reused={}", usize::from(!reused), usize::from(reused)),
  );
  layout
}

pub(super) fn layout_paragraph_at(
  document: &Document,
  paragraph_ix: usize,
  width: Pixels,
  previous_bottom: Pixels,
  previous_paragraph: Option<&LaidOutParagraph>,
  window: &mut Window,
  cx: &mut App,
) -> (LaidOutParagraph, Pixels, Pixels, bool) {
  let paragraph = &document.paragraphs[paragraph_ix];
  let p_format = paragraph_format(document, paragraph.style);
  let y = previous_bottom + p_format.spacing_before;
  let cache_key = paragraph_cache_key(document, paragraph);

  if let Some(cached) = previous_paragraph.filter(|cached| cached.cache_key == cache_key) {
    let mut laid_out_paragraph = cached.clone();
    laid_out_paragraph.shift_y(y);
    let max_width = laid_out_paragraph
      .lines
      .iter()
      .map(|line| line.origin.x + line.width)
      .fold(width, Pixels::max);
    let next_y = laid_out_paragraph.bottom + p_format.spacing_after;
    return (laid_out_paragraph, next_y, max_width, true);
  }

  let pageless_left = document.theme.pageless_inset_x;
  let pageless_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_left = pageless_left + border_inset;
  let content_top = border.map_or(px(0.0), |border| border.width + border.space_y);
  let content_width = (pageless_width - border_inset * 2.0).max(px(1.0));
  let paragraph_text = paragraph_text(document, paragraph_ix);
  let lines = wrap_lines(document, paragraph, p_format.clone(), &paragraph_text, content_width, window, cx);

  let mut max_width = width;
  let mut laid_out_lines = Vec::with_capacity(lines.len());
  let mut line_y = y + content_top;
  for mut line in lines {
    line.origin.x = content_left
      + match p_format.align {
        ParagraphAlign::Left => px(0.0),
        ParagraphAlign::Center => (content_width - line.width).max(px(0.0)) / 2.0,
      };
    line.origin.y = line_y;
    line_y += line.line_height;
    max_width = max_width.max(line.origin.x + line.width);
    laid_out_lines.push(line);
  }

  let bottom = line_y + content_top;
  let mut borders = Vec::new();
  if let Some(border) = border {
    push_box_rules(
      &mut borders,
      Bounds::new(point(pageless_left, y), size(pageless_width, bottom - y)),
      border.width,
      document.theme.default_text_color,
    );
  }

  (
    LaidOutParagraph {
      index: paragraph_ix,
      cache_key,
      len: paragraph_text.len(),
      top: y,
      bottom,
      lines: laid_out_lines,
      borders,
    },
    bottom + p_format.spacing_after,
    max_width,
    false,
  )
}

pub(super) fn estimate_paragraph_item_height(document: &Document, paragraph_ix: usize, width: Pixels) -> Pixels {
  let paragraph = &document.paragraphs[paragraph_ix];
  let p_format = paragraph_format(document, paragraph.style);
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_top = border.map_or(px(0.0), |border| border.width + border.space_y);
  let content_width = (width - document.theme.pageless_inset_x * 2.0 - border_inset * 2.0).max(px(1.0));
  let avg_char_width = (p_format.font_size * 0.52).max(px(1.0));
  let chars_per_line = ((content_width / avg_char_width).floor() as usize).max(1);
  let text_len = paragraph_text_len(paragraph);
  let estimated_lines = (text_len / chars_per_line).saturating_add(1).max(1);
  let line_gap = p_format.font_size * document.theme.line_gap_fraction;
  let line_height = (p_format.font_size + line_gap) * p_format.line_spacing;
  let mut height = p_format.spacing_before + content_top + line_height * estimated_lines as f32 + content_top + p_format.spacing_after;
  if paragraph_ix == 0 {
    height += document.theme.pageless_inset_top;
  }
  if paragraph_ix + 1 == document.paragraphs.len() {
    height += document.theme.pageless_inset_bottom;
  }
  height.max(line_height)
}

pub(super) fn wrap_lines(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  max_width: Pixels,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutLine> {
  let mut shape_cache = FragmentShapeCache::default();
  if text.is_empty() {
    return vec![shape_line(document, paragraph, p_format, text, 0..0, &mut shape_cache, window, cx)];
  }

  let mut lines = Vec::new();
  let mut start = 0;
  let break_ends = wrap_break_ends(text);
  let mut break_cursor = 0;

  while start < text.len() {
    while break_cursor < break_ends.len() && break_ends[break_cursor] <= start {
      break_cursor += 1;
    }
    let mut last_break = None;
    let mut wrapped = false;
    let mut scan_ix = break_cursor;

    while scan_ix < break_ends.len() {
      let break_at = break_ends[scan_ix];
      let candidate_width = measure_line_width(
        document,
        paragraph,
        &p_format,
        text,
        start..break_at,
        break_at - start,
        &mut shape_cache,
        window,
      );
      if candidate_width > max_width {
        let line_end = last_break
          .filter(|break_at| *break_at > start)
          .unwrap_or_else(|| {
            first_overflow_line_end(document, paragraph, &p_format, text, start, break_at, max_width, &mut shape_cache, window)
          });
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          &mut shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        wrapped = true;
        break;
      }
      last_break = Some(break_at);
      scan_ix += 1;
    }

    if wrapped {
      continue;
    }

    let remaining_width = measure_line_width(
      document,
      paragraph,
      &p_format,
      text,
      start..text.len(),
      text.len() - start,
      &mut shape_cache,
      window,
    );
    if remaining_width <= max_width {
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..],
        start..text.len(),
        &mut shape_cache,
        window,
        cx,
      ));
      break;
    }

    let line_end = last_break
      .filter(|break_at| *break_at > start)
      .unwrap_or_else(|| {
        first_overflow_line_end(
          document,
          paragraph,
          &p_format,
          text,
          start,
          text.len(),
          max_width,
          &mut shape_cache,
          window,
        )
      });
    lines.push(shape_line(
      document,
      paragraph,
      p_format.clone(),
      text[start..line_end].trim_end(),
      start..line_end,
      &mut shape_cache,
      window,
      cx,
    ));
    start = skip_leading_whitespace(text, line_end);
  }

  lines
}

pub(super) fn wrap_break_ends(text: &str) -> Vec<usize> {
  text
    .char_indices()
    .filter_map(|(byte_ix, ch)| is_wrap_break(ch).then_some(byte_ix + ch.len_utf8()))
    .collect()
}

pub(super) fn is_wrap_break(ch: char) -> bool {
  ch.is_whitespace() || matches!(ch, '-' | '/' | ',' | ';' | ':')
}

pub(super) fn skip_leading_whitespace(text: &str, mut byte: usize) -> usize {
  while byte < text.len() && text[byte..].chars().next().is_some_and(char::is_whitespace) {
    byte += text[byte..].chars().next().unwrap().len_utf8();
  }
  byte
}

pub(super) fn first_overflow_line_end(
  document: &Document,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  text: &str,
  start: usize,
  limit: usize,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> usize {
  let chars: Vec<_> = text[start..limit]
    .char_indices()
    .map(|(relative_byte, ch)| {
      let byte_ix = start + relative_byte;
      (byte_ix, byte_ix + ch.len_utf8(), ch)
    })
    .collect();
  if chars.is_empty() {
    return limit;
  }

  let mut low = 0;
  let mut high = chars.len();
  while low < high {
    let mid = (low + high) / 2;
    let end = chars[mid].1;
    let width = measure_line_width(document, paragraph, p_format, text, start..end, end - start, shape_cache, window);
    if width > max_width {
      high = mid;
    } else {
      low = mid + 1;
    }
  }

  let Some((byte_ix, end, ch)) = chars.get(low).copied() else {
    return limit;
  };
  if is_wrap_break(ch) || byte_ix == start { end } else { byte_ix }
}

pub(super) fn measure_line_width(
  document: &Document,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  paragraph_text: &str,
  source_range: Range<usize>,
  rendered_len: usize,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> Pixels {
  let mut width = px(0.0);
  let rendered_text = &paragraph_text[source_range.start..source_range.start + rendered_len];
  for fragment in fragments_for_range(paragraph, &source_range, rendered_len) {
    let text = &rendered_text[fragment.line_range.clone()];
    if text.is_empty() {
      continue;
    }
    let format = run_format(document, p_format.clone(), fragment.styles);
    let shaped = shape_fragment_cached(window, text, format.clone(), fragment.source_start, fragment.styles, shape_cache);
    if format.border_width > px(0.0) {
      width += document.theme.box_padding_left;
    }
    width += shaped.width;
    if format.border_width > px(0.0) {
      width += document.theme.box_padding_right;
    }
  }
  width
}

pub(super) fn shape_line(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  line_text: &str,
  source_range: Range<usize>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutLine {
  let fragments = fragments_for_range(paragraph, &source_range, line_text.len());
  let mut x = px(0.0);
  let mut segments = Vec::with_capacity(fragments.len().max(1));
  let mut ascent = px(0.0);
  let mut descent = px(0.0);

  for fragment in fragments {
    let text = &line_text[fragment.line_range.clone()];
    if text.is_empty() {
      continue;
    }
    let format = run_format(document, p_format.clone(), fragment.styles);
    let shaped = shape_fragment_cached(window, text, format.clone(), fragment.source_start, fragment.styles, shape_cache);
    let width = shaped.width;
    let box_margin_left = if format.border_width > px(0.0) {
      document.theme.box_padding_left
    } else {
      px(0.0)
    };
    let box_margin_right = if format.border_width > px(0.0) {
      document.theme.box_padding_right
    } else {
      px(0.0)
    };
    let segment_ascent = shaped.ascent;
    let segment_descent = shaped.descent;
    ascent = ascent.max(segment_ascent);
    descent = descent.max(segment_descent);
    x += box_margin_left;
    segments.push(LaidOutSegment {
      shaped,
      x,
      width,
      format: format.clone(),
      ascent: segment_ascent,
      descent: segment_descent,
      font_size: format.font_size,
      start_byte: fragment.source_start,
    });
    x += width + box_margin_right;
  }

  if segments.is_empty() {
    let format = run_format(document, p_format.clone(), RunStyles::default());
    let shaped = shape_fragment(window, "", format.clone());
    ascent = shaped.ascent;
    descent = shaped.descent;
    segments.push(LaidOutSegment {
      shaped,
      format: format.clone(),
      x: px(0.0),
      width: px(0.0),
      ascent,
      descent,
      font_size: format.font_size,
      start_byte: source_range.start,
    });
  } else {
    ascent = segments
      .iter()
      .map(|segment| segment.ascent)
      .fold(px(0.0), Pixels::max);
    descent = segments
      .iter()
      .map(|segment| segment.descent)
      .fold(px(0.0), Pixels::max);
  }

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
  };
  line.rects = rects_for_line(document, &line);
  line.underlines = underlines_for_line(document, &line, cx);
  line
}

#[derive(Default)]
pub(super) struct FragmentShapeCache {
  shapes: HashMap<FragmentShapeCacheKey, ShapedLine>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct FragmentShapeCacheKey {
  source_start: usize,
  len: usize,
  styles: RunStyles,
}

pub(super) fn shape_fragment_cached(
  window: &mut Window,
  text: &str,
  format: EffectiveRunFormat,
  source_start: usize,
  styles: RunStyles,
  cache: &mut FragmentShapeCache,
) -> ShapedLine {
  let key = FragmentShapeCacheKey {
    source_start,
    len: text.len(),
    styles,
  };
  if let Some(shaped) = cache.shapes.get(&key) {
    return shaped.clone();
  }
  let shaped = shape_fragment(window, text, format);
  cache.shapes.insert(key, shaped.clone());
  shaped
}

pub(super) fn shape_fragment(window: &mut Window, text: &str, format: EffectiveRunFormat) -> ShapedLine {
  let mut run_font = font(format.font_family);
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
pub(super) struct VisualFragment {
  styles: RunStyles,
  line_range: Range<usize>,
  source_start: usize,
}

pub(super) fn fragments_for_range(paragraph: &Paragraph, range: &Range<usize>, rendered_len: usize) -> Vec<VisualFragment> {
  let mut byte_offset = 0;
  let mut line_offset = 0;
  let mut remaining = rendered_len;
  let mut fragments = Vec::with_capacity(paragraph.runs.len());
  for run in &paragraph.runs {
    let run_start = byte_offset;
    let run_end = byte_offset + run.len;
    byte_offset = run_end;
    let start = run_start.max(range.start);
    let end = run_end.min(range.end);
    if start >= end || remaining == 0 {
      continue;
    }
    let len = (end - start).min(remaining);
    fragments.push(VisualFragment {
      styles: run.styles,
      line_range: line_offset..line_offset + len,
      source_start: start,
    });
    remaining -= len;
    line_offset += len;
  }
  fragments
}

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
      document.theme.box_padding_left
    } else {
      document.theme.highlight_pad_x
    };
    let highlight_pad_right = if segment.format.border_width > px(0.0) {
      document.theme.box_padding_right
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
        point(segment.x - document.theme.box_padding_left, text_top - document.theme.box_padding_top),
        size(
          (segment.width + document.theme.box_padding_left + document.theme.box_padding_right).max(px(1.0)),
          (text_bottom - text_top + document.theme.box_padding_top + document.theme.box_padding_bottom).max(px(1.0)),
        ),
      );
      push_box_rules(
        &mut borders,
        box_bounds,
        document.theme.emphasis_border_paint_width,
        document.theme.default_text_color,
      );
    }
  }
  // Word paints fills before border rules. Keeping all run borders after all
  // run highlights prevents a following highlighted run from hiding the right
  // edge of the previous boxed run.
  backgrounds.extend(borders);
  backgrounds
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
  let mut underlines = Vec::new();
  let baseline = line.baseline_y();
  for segment in &line.segments {
    match segment.format.underline {
      UnderlineKind::None => {},
      UnderlineKind::Single => {
        let (offset, thickness) = single_underline_metrics_for_segment(segment, document, cx);
        underlines.push(Decoration {
          bounds: Bounds::new(point(segment.x, baseline + offset), size(segment.width.max(px(1.0)), thickness)),
          color: document.theme.default_text_color,
        });
      },
      UnderlineKind::Double => {
        let (offset, thickness) = double_underline_metrics_for_segment(document);
        let y = baseline + offset;
        underlines.push(Decoration {
          bounds: Bounds::new(point(segment.x, y), size(segment.width.max(px(1.0)), thickness)),
          color: document.theme.default_text_color,
        });
        underlines.push(Decoration {
          bounds: Bounds::new(
            point(segment.x, y + thickness + document.theme.double_underline_gap),
            size(segment.width.max(px(1.0)), thickness),
          ),
          color: document.theme.default_text_color,
        });
      },
    }
  }
  underlines
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

pub(super) fn first_paragraph_with_bottom_at_or_after(paragraphs: &[LaidOutParagraph], y: Pixels) -> usize {
  let mut low = 0;
  let mut high = paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraphs[mid].bottom < y {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

pub(super) fn first_paragraph_with_top_after(paragraphs: &[LaidOutParagraph], y: Pixels) -> usize {
  let mut low = 0;
  let mut high = paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraphs[mid].top <= y {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

pub(super) fn first_line_with_bottom_at_or_after(lines: &[LaidOutLine], y: Pixels) -> usize {
  let mut low = 0;
  let mut high = lines.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if lines[mid].origin.y + lines[mid].line_height < y {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

pub(super) fn caret_bounds(layout: &LayoutState, offset: DocumentOffset, origin: Point<Pixels>) -> Option<Bounds<Pixels>> {
  let paragraph = paragraph_layout(layout, offset.paragraph)?;
  let line = paragraph
    .lines
    .iter()
    .find(|line| offset.byte >= line.start_byte && offset.byte <= line.end_byte)
    .or_else(|| paragraph.lines.last())?;
  let x = x_for_byte(line, offset.byte);
  Some(Bounds::new(origin + line.origin + point(x, px(0.0)), size(px(1.0), line.line_height)))
}

pub(super) fn x_for_byte(line: &LaidOutLine, byte: usize) -> Pixels {
  for segment in &line.segments {
    let segment_end = segment.start_byte + segment.shaped.len();
    if byte <= segment_end {
      return segment.x
        + segment
          .shaped
          .x_for_index(byte.saturating_sub(segment.start_byte));
    }
  }
  line.width
}

// Locate the `LaidOutLine` containing the given offset. Returns
// `(paragraph_layout_index, line_index)`. When the byte sits exactly on a
// soft-wrap seam (== end_byte of line k and start_byte of line k+1), we bias
// to the next line — matching Word's "caret-at-start-of-next-line"
// convention. This is exactly the disambiguation called out in the plan.
pub(super) fn locate_line(layout: &LayoutState, off: DocumentOffset) -> Option<(usize, usize)> {
  let p_ix = paragraph_layout_index(layout, off.paragraph)?;
  let para = &layout.paragraphs[p_ix];
  let mut low = 0;
  let mut high = para.lines.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if para.lines[mid].end_byte < off.byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  if let Some(line) = para.lines.get(low)
    && off.byte >= line.start_byte
    && off.byte <= line.end_byte
  {
    // Bias to next line at exact wrap seam.
    if off.byte == line.end_byte && low + 1 < para.lines.len() && para.lines[low + 1].start_byte == off.byte {
      return Some((p_ix, low + 1));
    }
    return Some((p_ix, low));
  }
  // Fall back to last line of the paragraph (e.g. byte == para.len after a
  // soft-wrapped trailing whitespace strip).
  let last = para.lines.len().checked_sub(1)?;
  Some((p_ix, last))
}

pub(super) fn paragraph_layout(layout: &LayoutState, paragraph: usize) -> Option<&LaidOutParagraph> {
  let layout_ix = paragraph_layout_index(layout, paragraph)?;
  layout.paragraphs.get(layout_ix)
}

pub(super) fn paragraph_layout_index(layout: &LayoutState, paragraph: usize) -> Option<usize> {
  if layout
    .paragraphs
    .get(paragraph)
    .is_some_and(|layout_paragraph| layout_paragraph.index == paragraph)
  {
    Some(paragraph)
  } else {
    let mut low = 0;
    let mut high = layout.paragraphs.len();
    while low < high {
      let mid = low + (high - low) / 2;
      if layout.paragraphs[mid].index < paragraph {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    layout
      .paragraphs
      .get(low)
      .is_some_and(|layout_paragraph| layout_paragraph.index == paragraph)
      .then_some(low)
  }
}

// Step to the previous visual line. If we're already on the first line of a
// paragraph, jump to the last line of the previous paragraph.
pub(super) fn find_line_above(layout: &LayoutState, p_ix: usize, line_ix: usize) -> Option<(usize, usize)> {
  if line_ix > 0 {
    return Some((p_ix, line_ix - 1));
  }
  if p_ix == 0 {
    return None;
  }
  let prev = p_ix - 1;
  let last = layout.paragraphs[prev].lines.len().checked_sub(1)?;
  Some((prev, last))
}

pub(super) fn find_line_below(layout: &LayoutState, p_ix: usize, line_ix: usize) -> Option<(usize, usize)> {
  if line_ix + 1 < layout.paragraphs[p_ix].lines.len() {
    return Some((p_ix, line_ix + 1));
  }
  if p_ix + 1 < layout.paragraphs.len() {
    return Some((p_ix + 1, 0));
  }
  None
}
