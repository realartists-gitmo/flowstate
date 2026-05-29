#[hotpath::measure]
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
  if text.contains(SOFT_LINE_BREAK) {
    let mut lines = Vec::new();
    let mut segment_start = 0;
    for (break_ix, ch) in text.char_indices().filter(|(_, ch)| *ch == SOFT_LINE_BREAK) {
      push_wrapped_soft_segment(
        &mut lines,
        document,
        paragraph,
        p_format.clone(),
        text,
        segment_start..break_ix,
        max_width,
        &mut shape_cache,
        window,
        cx,
      );
      segment_start = break_ix + ch.len_utf8();
    }
    push_wrapped_soft_segment(
      &mut lines,
      document,
      paragraph,
      p_format,
      text,
      segment_start..text.len(),
      max_width,
      &mut shape_cache,
      window,
      cx,
    );
    return lines;
  }

  wrap_text_segment(
    document,
    paragraph,
    p_format,
    text,
    0..text.len(),
    max_width,
    &mut shape_cache,
    window,
    cx,
  )
}

#[hotpath::measure]
fn wrap_lines_limited(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  start_byte: usize,
  max_lines: usize,
  max_width: Pixels,
  wrap_break_ends_override: Option<&[usize]>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> (Vec<LaidOutLine>, usize, bool) {
  let max_lines = max_lines.max(1);
  let start_byte = clamp_to_char_boundary(text, start_byte.min(text.len()));
  if text.is_empty() {
    return (
      vec![shape_line(document, paragraph, p_format, text, 0..0, shape_cache, window, cx)],
      0,
      true,
    );
  }
  if start_byte >= text.len() {
    return (Vec::new(), text.len(), true);
  }

  let mut lines = Vec::new();
  let mut segment_start = start_byte;
  while segment_start < text.len() && lines.len() < max_lines {
    let soft_break = text[segment_start..]
      .char_indices()
      .find_map(|(offset, ch)| (ch == SOFT_LINE_BREAK).then_some((segment_start + offset, ch.len_utf8())));
    let (segment_end, break_len, has_break) = soft_break
      .map(|(byte, len)| (byte, len, true))
      .unwrap_or((text.len(), 0, false));
    let remaining = max_lines - lines.len();
    if segment_start == segment_end {
      lines.push(shape_line(
        document,
        paragraph,
        p_format.clone(),
        "",
        segment_start..segment_start,
        shape_cache,
        window,
        cx,
      ));
      segment_start = segment_end + break_len;
      if lines.len() >= max_lines {
        return (lines, segment_start.min(text.len()), segment_start >= text.len());
      }
      continue;
    }

    let (mut segment_lines, next_byte, segment_complete) = wrap_text_segment_limited(
      document,
      paragraph,
      p_format.clone(),
      text,
      segment_start..segment_end,
      max_width,
      remaining,
      wrap_break_ends_override,
      shape_cache,
      window,
      cx,
    );
    lines.append(&mut segment_lines);
    if !segment_complete {
      return (lines, next_byte, false);
    }

    segment_start = if has_break { segment_end + break_len } else { segment_end };
    if !has_break {
      return (lines, text.len(), true);
    }
  }

  (lines, segment_start.min(text.len()), segment_start >= text.len())
}

#[hotpath::measure]
fn wrap_text_segment_limited(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  segment: Range<usize>,
  max_width: Pixels,
  max_lines: usize,
  wrap_break_ends_override: Option<&[usize]>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> (Vec<LaidOutLine>, usize, bool) {
  if segment.is_empty() {
    return (
      vec![shape_line(document, paragraph, p_format, "", segment.clone(), shape_cache, window, cx)],
      segment.end,
      true,
    );
  }

  let max_lines = max_lines.max(1);
  let mut lines = Vec::new();
  let mut start = segment.start;
  let computed_break_ends;
  let break_ends = if let Some(break_ends) = wrap_break_ends_override {
    break_ends
  } else {
    computed_break_ends = wrap_break_ends(&text[segment.clone()])
      .into_iter()
      .map(|byte| segment.start + byte)
      .collect::<Vec<_>>();
    computed_break_ends.as_slice()
  };

  while start < segment.end {
    let break_cursor = first_break_after(break_ends, start);
    let break_limit = first_break_after(break_ends, segment.end);
    let last_break = if break_cursor < break_limit {
      if let Some(over_ix) = first_break_over_width(
        document,
        paragraph,
        &p_format,
        text,
        start,
        break_ends,
        break_cursor..break_limit,
        max_width,
        shape_cache,
        window,
      ) {
        let line_end = if over_ix > break_cursor {
          break_ends[over_ix - 1]
        } else {
          first_overflow_line_end(
            document,
            paragraph,
            &p_format,
            text,
            start,
            break_ends[over_ix],
            max_width,
            shape_cache,
            window,
          )
        };
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        if lines.len() >= max_lines {
          return (lines, start.min(segment.end), start >= segment.end);
        }
        continue;
      }
      break_ends.get(break_limit - 1).copied()
    } else {
      None
    };

    if break_cursor == break_limit {
      let line_end = first_overflow_line_end(document, paragraph, &p_format, text, start, segment.end, max_width, shape_cache, window);
      if line_end < segment.end {
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        if lines.len() >= max_lines {
          return (lines, start.min(segment.end), start >= segment.end);
        }
        continue;
      }
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      return (lines, segment.end, true);
    }

    let Some(last_break) = last_break else {
      continue;
    };

    let remaining_width = measure_line_width(
      document,
      paragraph,
      &p_format,
      text,
      start..segment.end,
      segment.end - start,
      shape_cache,
      window,
    );
    if remaining_width <= max_width {
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      return (lines, segment.end, true);
    }

    let line_end = last_break;
    lines.push(shape_line(
      document,
      paragraph,
      p_format.clone(),
      text[start..line_end].trim_end(),
      start..line_end,
      shape_cache,
      window,
      cx,
    ));
    start = skip_leading_whitespace(text, line_end);
    if lines.len() >= max_lines {
      return (lines, start.min(segment.end), start >= segment.end);
    }
  }

  (lines, segment.end, true)
}

#[hotpath::measure]
fn push_wrapped_soft_segment(
  lines: &mut Vec<LaidOutLine>,
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  segment: Range<usize>,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) {
  if segment.is_empty() {
    lines.push(shape_line(document, paragraph, p_format, "", segment, shape_cache, window, cx));
  } else {
    lines.extend(wrap_text_segment(
      document,
      paragraph,
      p_format,
      text,
      segment,
      max_width,
      shape_cache,
      window,
      cx,
    ));
  }
}

#[hotpath::measure]
fn wrap_text_segment(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  segment: Range<usize>,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutLine> {
  if segment.is_empty() {
    return vec![shape_line(document, paragraph, p_format, "", segment, shape_cache, window, cx)];
  }

  let mut lines = Vec::new();
  let mut start = segment.start;
  let break_ends = wrap_break_ends(&text[segment.clone()])
    .into_iter()
    .map(|byte| segment.start + byte)
    .collect::<Vec<_>>();

  while start < segment.end {
    let break_cursor = first_break_after(&break_ends, start);
    let break_limit = first_break_after(&break_ends, segment.end);
    let last_break = if break_cursor < break_limit {
      if let Some(over_ix) = first_break_over_width(
        document,
        paragraph,
        &p_format,
        text,
        start,
        &break_ends,
        break_cursor..break_limit,
        max_width,
        shape_cache,
        window,
      ) {
        let line_end = if over_ix > break_cursor {
          break_ends[over_ix - 1]
        } else {
          first_overflow_line_end(
            document,
            paragraph,
            &p_format,
            text,
            start,
            break_ends[over_ix],
            max_width,
            shape_cache,
            window,
          )
        };
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        continue;
      }
      break_ends.get(break_limit - 1).copied()
    } else {
      None
    };

    if break_cursor == break_limit {
      let line_end = first_overflow_line_end(document, paragraph, &p_format, text, start, segment.end, max_width, shape_cache, window);
      if line_end < segment.end {
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        continue;
      }
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      break;
    }

    let Some(last_break) = last_break else {
      continue;
    };

    let remaining_width = measure_line_width(
      document,
      paragraph,
      &p_format,
      text,
      start..segment.end,
      segment.end - start,
      shape_cache,
      window,
    );
    if remaining_width <= max_width {
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      break;
    }

    let line_end = last_break;
    lines.push(shape_line(
      document,
      paragraph,
      p_format.clone(),
      text[start..line_end].trim_end(),
      start..line_end,
      shape_cache,
      window,
      cx,
    ));
    start = skip_leading_whitespace(text, line_end);
  }

  lines
}

#[hotpath::measure]
fn first_break_after(break_ends: &[usize], byte: usize) -> usize {
  let mut low = 0usize;
  let mut high = break_ends.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if break_ends[mid] <= byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
fn first_break_over_width(
  document: &Document,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  text: &str,
  start: usize,
  break_ends: &[usize],
  range: Range<usize>,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> Option<usize> {
  if range.start >= range.end {
    return None;
  }

  let mut lower = range.start;
  let mut step = 1usize;
  let probe = loop {
    let probe = range
      .start
      .saturating_add(step)
      .saturating_sub(1)
      .min(range.end - 1);
    let break_at = break_ends[probe];
    let candidate_width = measure_line_width(
      document,
      paragraph,
      p_format,
      text,
      start..break_at,
      break_at - start,
      shape_cache,
      window,
    );
    if candidate_width > max_width {
      break probe;
    }
    if probe + 1 >= range.end {
      return None;
    }
    lower = probe + 1;
    step = step.saturating_mul(2);
  };

  let mut low = lower;
  let mut high = probe;
  while low < high {
    let mid = low + (high - low) / 2;
    let break_at = break_ends[mid];
    let candidate_width = measure_line_width(
      document,
      paragraph,
      p_format,
      text,
      start..break_at,
      break_at - start,
      shape_cache,
      window,
    );
    if candidate_width > max_width {
      high = mid;
    } else {
      low = mid + 1;
    }
  }
  (low < range.end).then_some(low)
}

#[hotpath::measure]
pub(super) fn wrap_break_ends(text: &str) -> Vec<usize> {
  let mut breaks = Vec::with_capacity((text.len() / 8).min(4096));
  for (byte_ix, ch) in text.char_indices() {
    if is_wrap_break(ch) {
      breaks.push(byte_ix + ch.len_utf8());
    }
  }
  breaks
}

#[hotpath::measure]
pub(super) fn is_wrap_break(ch: char) -> bool {
  ch.is_whitespace() || matches!(ch, '-' | '/' | ',' | ';' | ':')
}

#[hotpath::measure]
pub(super) fn skip_leading_whitespace(text: &str, mut byte: usize) -> usize {
  while byte < text.len() && text[byte..].chars().next().is_some_and(char::is_whitespace) {
    byte += text[byte..].chars().next().unwrap().len_utf8();
  }
  byte
}

#[hotpath::measure]
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
  let mut chars = text[start..limit]
    .char_indices()
    .map(|(relative_byte, ch)| {
      let byte_ix = start + relative_byte;
      (byte_ix, byte_ix + ch.len_utf8(), ch)
    });
  let Some(first_char) = chars.next() else {
    return limit;
  };
  let char_count = chars.count() + 1;

  let mut low = 0;
  let mut high = char_count;
  while low < high {
    let mid = (low + high) / 2;
    let end = nth_char_boundary_after(text, start, mid).unwrap_or(limit);
    let width = measure_line_width(document, paragraph, p_format, text, start..end, end - start, shape_cache, window);
    if width > max_width {
      high = mid;
    } else {
      low = mid + 1;
    }
  }

  let (byte_ix, end, ch) = if low == 0 {
    first_char
  } else {
    nth_char_after(text, start, low).unwrap_or((limit, limit, '\0'))
  };
  if is_wrap_break(ch) || byte_ix == start { end } else { byte_ix }
}

#[hotpath::measure]
fn nth_char_boundary_after(text: &str, start: usize, n: usize) -> Option<usize> {
  if n == 0 {
    return text[start..].chars().next().map(|ch| start + ch.len_utf8());
  }
  text[start..]
    .char_indices()
    .nth(n)
    .map(|(relative_byte, ch)| start + relative_byte + ch.len_utf8())
}

#[hotpath::measure]
fn nth_char_after(text: &str, start: usize, n: usize) -> Option<(usize, usize, char)> {
  text[start..]
    .char_indices()
    .nth(n)
    .map(|(relative_byte, ch)| {
      let byte_ix = start + relative_byte;
      (byte_ix, byte_ix + ch.len_utf8(), ch)
    })
}

