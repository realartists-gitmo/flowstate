#[hotpath::measure]
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
      byte_range: 0..paragraph_text.len(),
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

pub(super) const DEFAULT_PARAGRAPH_CHUNK_TARGET_LINES: usize = 48;

pub(super) struct ParagraphChunkBuildResult {
  pub(super) layout: LayoutState,
  pub(super) start_byte: usize,
  pub(super) next_byte: usize,
  pub(super) complete: bool,
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
pub(super) fn build_paragraph_chunk_layout_with_visibility(
  document: &Document,
  paragraph_ix: usize,
  width: Pixels,
  start_byte: usize,
  target_lines: usize,
  invisibility_mode: bool,
  paragraph_prep: Option<&ParagraphPrep>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> Option<ParagraphChunkBuildResult> {
  if let Some(prep) = paragraph_prep
    && prep.paragraph_ix == paragraph_ix
    && prep.key.invisibility_mode == invisibility_mode
  {
    if !prep.visible {
      return None;
    }
    let paragraph = Paragraph {
      style: prep.layout_style,
      byte_range: 0..prep.paragraph_text.len(),
      runs: prep.layout_runs.as_ref().to_vec(),
      version: prep.layout_version,
    };
    return layout_prepared_paragraph_chunk_at(
      document,
      &paragraph,
      paragraph_ix,
      width,
      start_byte,
      target_lines,
      paragraph_ix == 0,
      paragraph_ix + 1 == document.paragraphs.len(),
      prep.paragraph_text.as_ref(),
      Some(prep.wrap_break_ends.as_ref()),
      shape_cache,
      window,
      cx,
    );
  }

  if invisibility_mode
    && document
      .paragraphs
      .get(paragraph_ix)
      .is_some_and(|paragraph| !paragraph_is_visible(paragraph))
  {
    return None;
  }
  let projected_document = invisibility_mode
    .then(|| invisibility_projected_document(document, paragraph_ix))
    .flatten();
  let layout_document = projected_document.as_ref().unwrap_or(document);
  let layout_paragraph_ix = if projected_document.is_some() { 0 } else { paragraph_ix };
  let is_first_document_paragraph = paragraph_ix == 0;
  let is_last_document_paragraph = paragraph_ix + 1 == document.paragraphs.len();
  let mut result = layout_paragraph_chunk_at(
    layout_document,
    layout_paragraph_ix,
    paragraph_ix,
    width,
    start_byte,
    target_lines,
    is_first_document_paragraph,
    is_last_document_paragraph,
    shape_cache,
    window,
    cx,
  )?;
  if projected_document.is_some()
    && let Some(paragraph) = result.layout.paragraphs.first_mut()
  {
    paragraph.index = paragraph_ix;
  }
  Some(result)
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
fn layout_paragraph_chunk_at(
  document: &Document,
  layout_paragraph_ix: usize,
  display_paragraph_ix: usize,
  width: Pixels,
  start_byte: usize,
  target_lines: usize,
  is_first_document_paragraph: bool,
  is_last_document_paragraph: bool,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> Option<ParagraphChunkBuildResult> {
  let paragraph = document.paragraphs.get(layout_paragraph_ix)?;
  let paragraph_text = paragraph_text(document, layout_paragraph_ix);
  layout_prepared_paragraph_chunk_at(
    document,
    paragraph,
    display_paragraph_ix,
    width,
    start_byte,
    target_lines,
    is_first_document_paragraph,
    is_last_document_paragraph,
    &paragraph_text,
    None,
    shape_cache,
    window,
    cx,
  )
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure]
fn layout_prepared_paragraph_chunk_at(
  document: &Document,
  paragraph: &Paragraph,
  display_paragraph_ix: usize,
  width: Pixels,
  start_byte: usize,
  target_lines: usize,
  is_first_document_paragraph: bool,
  is_last_document_paragraph: bool,
  paragraph_text: &str,
  wrap_break_ends_override: Option<&[usize]>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> Option<ParagraphChunkBuildResult> {
  let len = paragraph_text.len();
  let start_byte = clamp_to_char_boundary(paragraph_text, start_byte.min(len));
  let p_format = paragraph_format(document, paragraph.style);
  let cache_key = paragraph_cache_key(document, paragraph);
  let pageless_left = document.theme.pageless_inset_x;
  let pageless_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_left = pageless_left + border_inset;
  let content_width = (pageless_width - border_inset * 2.0).max(px(1.0));
  let is_first_chunk = start_byte == 0;
  let chunk_target_lines = target_lines.max(1);
  let (lines, next_byte, complete) = wrap_lines_limited(
    document,
    paragraph,
    p_format.clone(),
    paragraph_text,
    start_byte,
    chunk_target_lines,
    content_width,
    wrap_break_ends_override,
    shape_cache,
    window,
    cx,
  );

  let paragraph_top = if is_first_chunk {
    let mut top = p_format.spacing_before;
    if is_first_document_paragraph {
      top += document.theme.pageless_inset_top;
    }
    top
  } else {
    px(0.0)
  };
  let content_top = if is_first_chunk {
    border.map_or(px(0.0), |border| border.width + border.space_y)
  } else {
    px(0.0)
  };
  let mut max_width = width;
  let mut laid_out_lines = Vec::with_capacity(lines.len());
  let mut line_y = paragraph_top + content_top;
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

  let tail_space = if complete {
    let mut tail = border.map_or(px(0.0), |border| border.width + border.space_y) + p_format.spacing_after;
    if is_last_document_paragraph {
      tail += document.theme.pageless_inset_bottom;
    }
    tail
  } else {
    px(0.0)
  };
  let row_bottom = line_y + tail_space;
  let byte_range_end = if complete { len } else { next_byte.min(len) };
  let mut borders = Vec::new();
  if let Some(border) = border {
    push_chunk_box_rules(
      &mut borders,
      Bounds::new(
        point(pageless_left, paragraph_top),
        size(pageless_width, (row_bottom - paragraph_top).max(px(1.0))),
      ),
      border.width,
      document.theme.default_text_color,
      is_first_chunk,
      complete,
    );
  }

  let paragraph = LaidOutParagraph {
    index: display_paragraph_ix,
    cache_key,
    len,
    byte_range: start_byte..byte_range_end,
    top: paragraph_top,
    bottom: row_bottom,
    lines: laid_out_lines,
    borders,
  };
  let layout = LayoutState {
    blocks: vec![LaidOutBlock::Paragraph(paragraph.clone())],
    paragraph_to_block: vec![0],
    block_to_paragraph: vec![Some(display_paragraph_ix)],
    paragraphs: vec![paragraph],
    bounds: None,
    size: size(max_width.max(width), row_bottom.max(px(1.0))),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  Some(ParagraphChunkBuildResult {
    layout,
    start_byte,
    next_byte: byte_range_end,
    complete,
  })
}

#[hotpath::measure]
fn clamp_to_char_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte > 0 && !text.is_char_boundary(byte) {
    byte -= 1;
  }
  byte
}

#[hotpath::measure]
fn ceil_char_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte < text.len() && !text.is_char_boundary(byte) {
    byte += 1;
  }
  byte
}

#[hotpath::measure]
fn push_chunk_box_rules(
  rects: &mut Vec<RunRect>,
  bounds: Bounds<Pixels>,
  thickness: Pixels,
  color: Hsla,
  include_top: bool,
  include_bottom: bool,
) {
  if include_top {
    rects.push(RunRect {
      bounds: Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
      color,
      snap: RuleSnap::Horizontal,
    });
  }
  if include_bottom {
    rects.push(RunRect {
      bounds: Bounds::new(
        point(bounds.origin.x, bounds.origin.y + bounds.size.height - thickness),
        size(bounds.size.width, thickness),
      ),
      color,
      snap: RuleSnap::Horizontal,
    });
  }
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

