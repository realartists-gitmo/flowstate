/// B-S6: invisibility mode reaches INTO cells — invisible cell paragraphs
/// drop from layout exactly like body paragraphs, while objects stay (they
/// are read-mode landmarks, not prose). Edit-mode callers pass `false`.
#[hotpath::measure]
pub(super) fn layout_table_block_with_visibility(
  document: &DocumentProjection,
  block_ix: usize,
  table: &TableBlock,
  width: Pixels,
  y: Pixels,
  invisibility_mode: bool,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutTable {
  if !invisibility_mode {
    return layout_table_block(document, block_ix, table, width, y, window, cx);
  }
  let filtered = table_with_visible_cell_content(document, table);
  layout_table_block(document, block_ix, &filtered, width, y, window, cx)
}

/// The invisibility projection of a table: each cell keeps only its VISIBLE
/// paragraphs (plus all objects); a fully-hidden cell keeps one empty
/// paragraph so the grid never collapses.
fn table_with_visible_cell_content(document: &DocumentProjection, table: &TableBlock) -> TableBlock {
  let mut filtered = table.clone();
  for row in &mut filtered.rows {
    for cell in &mut row.cells {
      cell.blocks.retain(|block| match block {
        TableCellBlock::Paragraph(paragraph) => {
          paragraph_is_visible_for_theme(&document.theme, &paragraph.paragraph)
        },
        TableCellBlock::Table(_) | TableCellBlock::Image(_) | TableCellBlock::Equation(_) => true,
      });
      if cell.blocks.is_empty() {
        cell.blocks.push(TableCellBlock::Paragraph(crate::TableCellParagraph {
          paragraph: crate::Paragraph {
            style: crate::ParagraphStyle::Normal,
            runs: Vec::new(),
            version: 0,
          },
          text: String::new(),
        }));
      }
    }
  }
  filtered
}

#[hotpath::measure]
fn layout_table_block(
  document: &DocumentProjection,
  block_ix: usize,
  table: &TableBlock,
  width: Pixels,
  y: Pixels,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutTable {
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
    let row_height = table_row_height(document, row, &column_widths, window, cx);
    let mut x = table_left;
    let mut cells = Vec::with_capacity(row.cells.len());
    let mut column_ix = 0;
    for cell in &row.cells {
      let span = cell.col_span.max(1) as usize;
      let cell_width = spanned_column_width(&column_widths, column_ix, span);
      let cell_bounds = Bounds::new(point(x, row_top), size(cell_width, row_height));
      cells.push(LaidOutTableCell {
        bounds: cell_bounds,
        blocks: layout_table_cell_blocks(document, cell, cell_bounds, window, cx),
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
    block_ix,
    top: y,
    bottom: row_top,
    bounds: Bounds::new(point(table_left, y), size(table_width, (row_top - y).max(px(1.0)))),
    rows,
    header_row: table.style.header_row,
  }
}

#[hotpath::measure]
fn table_row_height(document: &DocumentProjection, row: &TableRow, column_widths: &[Pixels], window: &mut Window, cx: &mut App) -> Pixels {
  let mut column_ix = 0;
  row
    .cells
    .iter()
    .map(|cell| {
      let span = cell.col_span.max(1) as usize;
      let width = spanned_column_width(column_widths, column_ix, span);
      column_ix += span;
      table_cell_height(document, cell, width, window, cx)
    })
    .fold(px(28.0), Pixels::max)
}

#[hotpath::measure]
fn resolved_table_column_widths(table: &TableBlock, table_width: Pixels, column_count: usize) -> Vec<Pixels> {
  let mut fixed_total = px(0.0);
  let mut fraction_total = 0u32;
  let mut auto_count = 0usize;
  for ix in 0..column_count {
    match table
      .columns
      .get(ix)
      .map(|column| &column.width)
      .unwrap_or(&TableColumnWidth::Fraction(1))
    {
      TableColumnWidth::FixedPx(width) => fixed_total += px(*width as f32),
      TableColumnWidth::Fraction(fraction) => fraction_total = fraction_total.saturating_add((*fraction).max(1)),
      TableColumnWidth::Auto => auto_count += 1,
    }
  }
  let remaining = (table_width - fixed_total).max(px(1.0));
  let denominator = fraction_total.saturating_add(auto_count as u32).max(1);
  // B-S5c: Auto columns are CONTENT-AWARE. Fractions keep exactly their
  // historical share (each Auto still counts as one fractional unit of the
  // table); the Auto POOL is then redistributed among the Auto columns
  // weighted by each column's longest cell text (clamped so one giant cell
  // can't starve its siblings).
  let auto_weight = |column_ix: usize| -> f32 {
    let longest = table
      .rows
      .iter()
      .filter_map(|row| row.cells.get(column_ix))
      .map(|cell| {
        cell
          .blocks
          .iter()
          .map(|block| match block {
            TableCellBlock::Paragraph(paragraph) => paragraph.text.chars().count(),
            TableCellBlock::Table(_) => 40,
            TableCellBlock::Image(_) | TableCellBlock::Equation(_) => 16,
          })
          .max()
          .unwrap_or(0)
      })
      .max()
      .unwrap_or(0);
    (longest as f32).clamp(6.0, 80.0)
  };
  let auto_weight_total: f32 = (0..column_count)
    .filter(|ix| {
      matches!(
        table.columns.get(*ix).map(|column| &column.width),
        Some(TableColumnWidth::Auto)
      )
    })
    .map(auto_weight)
    .sum();
  let auto_pool = remaining * (auto_count as f32 / denominator as f32);
  (0..column_count)
    .map(|ix| {
      match table
        .columns
        .get(ix)
        .map(|column| &column.width)
        .unwrap_or(&TableColumnWidth::Fraction(1))
      {
        TableColumnWidth::FixedPx(width) => px(*width as f32).max(px(8.0)),
        TableColumnWidth::Fraction(fraction) => remaining * ((*fraction).max(1) as f32 / denominator as f32),
        TableColumnWidth::Auto => {
          if auto_weight_total > 0.0 {
            (auto_pool * (auto_weight(ix) / auto_weight_total)).max(px(24.0))
          } else {
            remaining * (1.0 / denominator as f32)
          }
        },
      }
    })
    .collect()
}

#[hotpath::measure]
fn spanned_column_width(column_widths: &[Pixels], column_ix: usize, span: usize) -> Pixels {
  let end = column_ix.saturating_add(span).min(column_widths.len());
  let width = column_widths
    .get(column_ix..end)
    .unwrap_or(&[])
    .iter()
    .copied()
    .fold(px(0.0), |sum, width| sum + width);
  width.max(px(1.0))
}

#[hotpath::measure]
fn table_cell_height(document: &DocumentProjection, cell: &TableCell, width: Pixels, window: &mut Window, cx: &mut App) -> Pixels {
  let padding = table_cell_padding();
  let content_width = (width - padding * 2.0).max(px(1.0));
  let mut y = padding;
  if cell.blocks.is_empty() {
    return px(28.0);
  }
  for block in &cell.blocks {
    match block {
      TableCellBlock::Paragraph(paragraph) => {
        let laid_out = layout_table_cell_paragraph(document, paragraph, 0, content_width, padding, y, window, cx);
        y = laid_out.bottom + px(2.0);
      },
      TableCellBlock::Table(table) => {
        let laid_out = layout_table_block(document, 0, table, content_width + document.theme.pageless_inset_x * 2.0, y, window, cx);
        y = laid_out.bottom + px(2.0);
      },
      // B-S5: objects in cells occupy their intrinsic box, clamped to the
      // cell's content width.
      TableCellBlock::Image(image) => {
        y += cell_object_size(document, &TableCellBlock::Image(image.clone()), content_width).height + px(2.0);
      },
      TableCellBlock::Equation(equation) => {
        y += cell_object_size(document, &TableCellBlock::Equation(equation.clone()), content_width).height + px(2.0);
      },
    }
  }
  (y + padding).max(px(28.0))
}

/// B-S5: the intrinsic box for an object living inside a cell — image
/// dimensions from its asset (or a placeholder), equation from its render —
/// clamped to the cell's content width.
fn cell_object_size(document: &DocumentProjection, block: &TableCellBlock, content_width: Pixels) -> Size<Pixels> {
  let zoom = document.theme.zoom_factor.max(0.01);
  let (width, height) = match block {
    TableCellBlock::Image(image) => document
      .assets
      .assets
      .get(&image.asset_id)
      .and_then(|asset| asset.dimensions)
      .map_or((160.0, 120.0), |(width, height)| (width as f32, height as f32)),
    TableCellBlock::Equation(equation) => {
      crate::rich_text::editor::equation_intrinsic_size(equation).unwrap_or((120.0, 32.0))
    },
    TableCellBlock::Paragraph(_) | TableCellBlock::Table(_) => (0.0, 0.0),
  };
  let width = width * zoom;
  let height = height * zoom;
  let max_width: f32 = content_width.max(px(24.0)).into();
  let scale = if width > max_width && width > 0.0 { max_width / width } else { 1.0 };
  size(px((width * scale).max(24.0)), px((height * scale).max(20.0)))
}

#[hotpath::measure]
fn layout_table_cell_blocks(
  document: &DocumentProjection,
  cell: &TableCell,
  bounds: Bounds<Pixels>,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutBlock> {
  let padding = table_cell_padding();
  let content_width = (bounds.size.width - padding * 2.0).max(px(1.0));
  let mut y = bounds.origin.y + padding;
  let mut blocks = Vec::with_capacity(cell.blocks.len());
  for (ix, block) in cell.blocks.iter().enumerate() {
    match block {
      TableCellBlock::Paragraph(paragraph) => {
        let laid_out = layout_table_cell_paragraph(document, paragraph, ix, content_width, bounds.origin.x + padding, y, window, cx);
        y = laid_out.bottom + px(2.0);
        blocks.push(LaidOutBlock::Paragraph(laid_out));
      },
      TableCellBlock::Table(table) => {
        let laid_out = layout_table_block(document, 0, table, content_width + document.theme.pageless_inset_x * 2.0, y, window, cx);
        y = laid_out.bottom + px(2.0);
        blocks.push(LaidOutBlock::Table(laid_out));
      },
      // B-S5: objects in cells — an object box at the flow position.
      TableCellBlock::Image(_) | TableCellBlock::Equation(_) => {
        let object_size = cell_object_size(document, block, content_width);
        let object_bounds = Bounds {
          origin: point(bounds.origin.x + padding, y),
          size: object_size,
        };
        let laid_out = LaidOutObjectBlock {
          block_ix: ix,
          top: y,
          bottom: y + object_size.height,
          bounds: object_bounds,
          render_ready: true,
        };
        y = laid_out.bottom + px(2.0);
        blocks.push(match block {
          TableCellBlock::Image(_) => LaidOutBlock::Image(laid_out),
          _ => LaidOutBlock::Equation(laid_out),
        });
      },
    }
  }
  blocks
}

#[hotpath::measure]
fn layout_table_cell_paragraph(
  document: &DocumentProjection,
  cell_paragraph: &TableCellParagraph,
  index: usize,
  width: Pixels,
  x: Pixels,
  y: Pixels,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutParagraph {
  let paragraph = &cell_paragraph.paragraph;
  let p_format = paragraph_format(document, paragraph.style);
  let cache_key = paragraph_cache_key(document, paragraph);
  let lines = wrap_lines(document, paragraph, p_format.clone(), &cell_paragraph.text, width, window, cx);
  let mut laid_out_lines = Vec::with_capacity(lines.len());
  let mut line_y = y;
  for mut line in lines {
    line.origin.x = x
      + match p_format.align {
        ParagraphAlign::Left => px(0.0),
        ParagraphAlign::Center => (width - line.width).max(px(0.0)) / 2.0,
      };
    line.origin.y = line_y;
    line_y += line.line_height;
    laid_out_lines.push(line);
  }
  LaidOutParagraph {
    index,
    cache_key,
    len: cell_paragraph.text.len(),
    byte_range: 0..cell_paragraph.text.len(),
    top: y,
    bottom: line_y,
    lines: laid_out_lines,
    borders: Vec::new(),
  }
}

#[hotpath::measure]
pub(super) fn table_cell_padding() -> Pixels {
  px(5.0)
}

