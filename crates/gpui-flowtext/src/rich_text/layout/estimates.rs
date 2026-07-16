/// §perf-heaven T5: fraction of a line's raw char capacity actually filled once
/// word-wrap (break at word boundaries, not mid-word) is accounted for. Tuned
/// against the estimate-accuracy net so the estimate approaches the exact height
/// without over-shooting.
const WORD_WRAP_FILL: f32 = 0.82;

#[hotpath::measure]
pub(super) fn estimate_paragraph_item_height(document: &DocumentProjection, paragraph_ix: usize, width: Pixels) -> Pixels {
  estimate_paragraph_item_height_with_visibility(document, paragraph_ix, width, false)
}

#[hotpath::measure]
pub(super) fn estimate_paragraph_item_height_with_visibility(
  document: &DocumentProjection,
  paragraph_ix: usize,
  width: Pixels,
  invisibility_mode: bool,
) -> Pixels {
  if invisibility_mode {
    if document
      .paragraphs
      .get(paragraph_ix)
      .is_some_and(|paragraph| !paragraph_is_visible(document, paragraph))
    {
      return px(0.0);
    }
    // §act-ten A10.9: estimate the projected paragraph from its visible
    // text/runs DIRECTLY. The old path assembled a full throwaway
    // `DocumentProjection` (rope + paragraph/block trees + assets clone +
    // sections rebuild) per paragraph per estimate — 30k calls / 320 MB in
    // hotpath1. Same inputs, same numbers: the projected paragraph is always
    // `Normal`-styled and its text length equals the sum of projected run
    // lengths.
    if document
      .paragraphs
      .get(paragraph_ix)
      .is_some_and(|paragraph| matches!(paragraph.style, ParagraphStyle::Normal))
      && let Some((text, _runs)) = projected_visible_paragraph_text_and_runs(document, paragraph_ix)
    {
      return estimate_item_height_from_parts(
        document,
        paragraph_ix,
        ParagraphStyle::Normal,
        text.len(),
        text.matches(SOFT_LINE_BREAK).count(),
        width,
      );
    }
    // No projection (non-Normal visible style, or no non-empty visible runs):
    // fall through to the full-paragraph estimate, as before.
  }
  let paragraph = &document.paragraphs[paragraph_ix];
  estimate_item_height_from_parts(
    document,
    paragraph_ix,
    paragraph.style,
    paragraph_text_len(paragraph),
    paragraph_char_count(document, paragraph_ix, SOFT_LINE_BREAK),
    width,
  )
}

/// Shared core of the §perf-heaven T5 line-count height heuristic, over
/// pre-extracted paragraph facts (style, text byte length, forced-break count).
fn estimate_item_height_from_parts(
  document: &DocumentProjection,
  paragraph_ix: usize,
  style: ParagraphStyle,
  text_len: usize,
  forced_line_count: usize,
  width: Pixels,
) -> Pixels {
  let p_format = paragraph_format(document, style);
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_top = border.map_or(px(0.0), |border| border.width + border.space_y);
  let content_width = (width - document.theme.pageless_inset_x * 2.0 - border_inset * 2.0).max(px(1.0));
  let avg_char_width = (p_format.font_size * 0.52).max(px(1.0));
  // §perf-heaven T5: word-wrap allowance — see the prep variant.
  let chars_per_line = ((content_width / avg_char_width * WORD_WRAP_FILL).floor() as usize).max(1);
  let estimated_lines = (text_len / chars_per_line)
    .saturating_add(1)
    .saturating_add(forced_line_count)
    .max(1);
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

#[hotpath::measure]
// §act-eleven A11.7: takes the CURRENT paragraph index — the prep's stored
// `paragraph_ix` is its build-time position, which a structural shift above
// invalidated positionally but not contentually (the whole point of dropping
// the positional validity check is that the shifted tail keeps its prep).
pub(super) fn estimate_paragraph_prep_item_height(
  document: &DocumentProjection,
  prep: &ParagraphPrep,
  paragraph_ix: usize,
  width: Pixels,
) -> Pixels {
  if !prep.visible {
    return px(0.0);
  }
  let p_format = paragraph_format(document, prep.layout_style);
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_top = border.map_or(px(0.0), |border| border.width + border.space_y);
  let content_width = (width - document.theme.pageless_inset_x * 2.0 - border_inset * 2.0).max(px(1.0));
  let avg_char_width = (p_format.font_size * 0.52).max(px(1.0));
  // §perf-heaven T5: word-wrap breaks at word boundaries, so a line holds fewer
  // chars than its raw char capacity (the last word spills to the next line).
  // Applying `WORD_WRAP_FILL` lifts the estimated line count toward the exact
  // one, cutting the estimate's height under-shoot (guarded by the
  // estimate-accuracy net — it must not tip into over-shoot).
  let chars_per_line = ((content_width / avg_char_width * WORD_WRAP_FILL).floor() as usize).max(1);
  // §perf-heaven T7.17: `chars_per_line` is a CHARACTER capacity, so measure the
  // text in characters, not bytes. `prep.paragraph_text` is already materialized
  // here, so the char count is cheap; using the byte length made a multibyte
  // (e.g. CJK) paragraph estimate up to 3–4× too many lines, which over-shoots
  // the exact height (guarded by the estimate-accuracy net, now including a
  // non-ASCII fixture).
  let text_len = prep.paragraph_text.chars().count();
  let forced_line_count = prep.paragraph_text.matches(SOFT_LINE_BREAK).count();
  let estimated_lines = (text_len / chars_per_line)
    .saturating_add(1)
    .saturating_add(forced_line_count)
    .max(1);
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

#[hotpath::measure]
pub(super) fn estimate_structural_block_item_height(document: &DocumentProjection, block_ix: usize, width: Pixels) -> Pixels {
  let Some(block) = document.blocks.get(block_ix) else {
    return px(1.0);
  };
  match block {
    Block::Paragraph(_) => {
      let paragraph_ix = document
        .blocks
        .iter()
        .take(block_ix + 1)
        .filter(|block| matches!(block, Block::Paragraph(_)))
        .count()
        .saturating_sub(1);
      estimate_paragraph_item_height(document, paragraph_ix, width)
    },
    Block::Image(image) => image_placeholder_height(document, image, width) + document.theme.paragraph_after,
    Block::Equation(equation) => equation_placeholder_height(document, equation) + document.theme.paragraph_after,
    Block::Table(table) => table_placeholder_height(document, table, width) + document.theme.paragraph_after,
  }
}

#[hotpath::measure]
fn table_placeholder_height(document: &DocumentProjection, table: &TableBlock, width: Pixels) -> Pixels {
  let line_height = (document.theme.body_font_size * document.theme.zoom_factor.max(0.01) * document.theme.line_spacing).max(px(16.0));
  let column_count = table
    .columns
    .len()
    .max(
      table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1),
    )
    .max(1);
  let content_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let column_widths = resolved_table_column_widths(table, content_width, column_count);
  let height = table
    .rows
    .iter()
    .map(|row| {
      let mut column_ix = 0;
      row
        .cells
        .iter()
        .map(|cell| {
          let span = cell.col_span.max(1) as usize;
          let _column_width = spanned_column_width(&column_widths, column_ix, span);
          column_ix += span;
          let paragraph_count = cell
            .blocks
            .iter()
            .filter(|block| matches!(block, TableCellBlock::Paragraph(_)))
            .count()
            .max(1);
          line_height * paragraph_count as f32 + table_cell_padding() * 2.0
        })
        .fold(px(28.0), Pixels::max)
    })
    .fold(px(0.0), |height, row_height| height + row_height);
  if height > px(0.0) {
    return height;
  }
  let laid_out = layout_table_block_without_text(document, table, width, px(0.0));
  if laid_out.rows.is_empty() {
    return (document.theme.body_font_size * document.theme.zoom_factor.max(0.01) * document.theme.line_spacing).max(px(24.0));
  }
  laid_out.bottom - laid_out.top
}

#[hotpath::measure]
fn layout_table_block_without_text(document: &DocumentProjection, table: &TableBlock, width: Pixels, y: Pixels) -> LaidOutTable {
  let table_left = document.theme.pageless_inset_x;
  let table_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let column_count = table
    .columns
    .len()
    .max(
      table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1),
    )
    .max(1);
  let column_widths = resolved_table_column_widths(table, table_width, column_count);
  let mut row_top = y;
  let mut rows = Vec::with_capacity(table.rows.len());
  for row in &table.rows {
    let row_height = px(28.0);
    let mut x = table_left;
    let mut cells = Vec::with_capacity(row.cells.len());
    let mut column_ix = 0;
    for cell in &row.cells {
      let span = cell.col_span.max(1) as usize;
      let cell_width = spanned_column_width(&column_widths, column_ix, span);
      cells.push(LaidOutTableCell {
        bounds: Bounds::new(point(x, row_top), size(cell_width, row_height)),
        blocks: Vec::new(),
      });
      x += cell_width;
      column_ix += span;
    }
    rows.push(LaidOutTableRow {
      top: row_top,
      bottom: row_top + row_height,
      cells,
    });
    row_top += row_height;
  }
  LaidOutTable {
    block_ix: 0,
    top: y,
    bottom: row_top,
    bounds: Bounds::new(point(table_left, y), size(table_width, (row_top - y).max(px(1.0)))),
    rows,
    header_row: table.style.header_row,
  }
}
