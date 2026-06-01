#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
pub(super) fn caret_bounds(layout: &LayoutState, offset: DocumentOffset, origin: Point<Pixels>) -> Option<Bounds<Pixels>> {
  // Use locate_line so the caret is drawn on the same visual line that
  // Up/Down/Home/End navigate from — in particular the wrap-seam bias
  // (byte at end of line k == start of line k+1 → paint on k+1) must be
  // identical in both paths, otherwise the caret appears at the wrong
  // position after the cursor reaches a soft-wrap boundary.
  let (p_ix, l_ix) = locate_line(layout, offset)?;
  let line = layout.paragraphs[p_ix].lines.get(l_ix)?;
  let x = x_for_byte(line, offset.byte);
  Some(Bounds::new(origin + line.origin + point(x, px(0.0)), size(px(1.0), line.line_height)))
}

#[hotpath::measure]
pub(super) fn caret_bounds_in_paragraph(paragraph: &LaidOutParagraph, byte: usize, origin: Point<Pixels>) -> Option<Bounds<Pixels>> {
  let line_ix = line_ix_for_byte(paragraph, byte)?;
  let line = paragraph.lines.get(line_ix)?;
  let x = x_for_byte(line, byte);
  Some(Bounds::new(origin + line.origin + point(x, px(0.0)), size(px(1.0), line.line_height)))
}

#[hotpath::measure]
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

#[hotpath::measure]
fn line_ix_for_byte(paragraph: &LaidOutParagraph, byte: usize) -> Option<usize> {
  let mut low = 0;
  let mut high = paragraph.lines.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraph.lines[mid].end_byte < byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  if let Some(line) = paragraph.lines.get(low)
    && byte >= line.start_byte
    && byte <= line.end_byte
  {
    if byte == line.end_byte && low + 1 < paragraph.lines.len() && paragraph.lines[low + 1].start_byte == byte {
      return Some(low + 1);
    }
    return Some(low);
  }
  paragraph.lines.len().checked_sub(1)
}

// Locate the `LaidOutLine` containing the given offset. Returns
// `(paragraph_layout_index, line_index)`. When the byte sits exactly on a
// soft-wrap seam (== end_byte of line k and start_byte of line k+1), we bias
// to the next line — matching Word's "caret-at-start-of-next-line"
// convention. This is exactly the disambiguation called out in the plan.
#[hotpath::measure]
pub(super) fn locate_line(layout: &LayoutState, off: DocumentOffset) -> Option<(usize, usize)> {
  let p_ix = paragraph_layout_index_for_offset(layout, off)?;
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

#[hotpath::measure]
pub(super) fn paragraph_layout(layout: &LayoutState, paragraph: usize) -> Option<&LaidOutParagraph> {
  let layout_ix = paragraph_layout_index(layout, paragraph)?;
  layout.paragraphs.get(layout_ix)
}

#[hotpath::measure]
pub(super) fn paragraph_layout_index_for_offset(layout: &LayoutState, offset: DocumentOffset) -> Option<usize> {
  layout
    .paragraphs
    .iter()
    .enumerate()
    .find(|(_, paragraph)| paragraph.index == offset.paragraph && paragraph.contains_byte(offset.byte))
    .map(|(ix, _)| ix)
    .or_else(|| paragraph_layout_index(layout, offset.paragraph))
}

#[hotpath::measure]
pub(super) fn paragraph_layout_index(layout: &LayoutState, paragraph: usize) -> Option<usize> {
  let _ = layout.paragraph_block_ix(paragraph);
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
#[hotpath::measure]
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

#[hotpath::measure]
pub(super) fn find_line_below(layout: &LayoutState, p_ix: usize, line_ix: usize) -> Option<(usize, usize)> {
  if line_ix + 1 < layout.paragraphs[p_ix].lines.len() {
    return Some((p_ix, line_ix + 1));
  }
  if p_ix + 1 < layout.paragraphs.len() {
    return Some((p_ix + 1, 0));
  }
  None
}
