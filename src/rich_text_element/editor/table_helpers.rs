#[hotpath::measure]
fn default_table_row(columns: usize) -> TableRow {
  TableRow {
    cells: (0..columns).map(|_| default_table_cell()).collect(),
  }
}

#[hotpath::measure]
fn default_table_cell() -> TableCell {
  TableCell {
    blocks: vec![TableCellBlock::Paragraph(default_table_cell_paragraph())],
    row_span: 1,
    col_span: 1,
  }
}

#[hotpath::measure]
fn table_column_count(table: &TableBlock) -> usize {
  table
    .rows
    .iter()
    .map(|row| {
      row
        .cells
        .iter()
        .map(|cell| cell.col_span.max(1) as usize)
        .sum::<usize>()
    })
    .max()
    .unwrap_or(0)
    .max(table.column_widths.len())
}

#[hotpath::measure]
fn fixed_table_column_widths_from_layout(table: &TableBlock, layout: &LaidOutTable) -> Vec<u32> {
  let column_count = table_column_count(table).max(1);
  let mut widths = vec![120; column_count];
  for (ix, width) in table.column_widths.iter().enumerate() {
    if ix < widths.len()
      && let TableColumnWidth::FixedPx(width) = width
    {
      widths[ix] = *width;
    }
  }
  let Some(first_layout_row) = layout.rows.first() else {
    return widths;
  };
  let Some(first_data_row) = table.rows.first() else {
    return widths;
  };
  let mut logical_column_ix = 0usize;
  for (cell_ix, cell_layout) in first_layout_row.cells.iter().enumerate() {
    let span = first_data_row
      .cells
      .get(cell_ix)
      .map(|cell| cell.col_span.max(1) as usize)
      .unwrap_or(1);
    let cell_width: f32 = cell_layout.bounds.size.width.into();
    let per_column = (cell_width / span as f32).max(32.0).round() as u32;
    for ix in logical_column_ix..logical_column_ix.saturating_add(span).min(widths.len()) {
      if !matches!(table.column_widths.get(ix), Some(TableColumnWidth::FixedPx(_))) {
        widths[ix] = per_column;
      }
    }
    logical_column_ix = logical_column_ix.saturating_add(span);
  }
  widths
}

#[hotpath::measure]
fn default_table_cell_paragraph() -> TableCellParagraph {
  TableCellParagraph {
    paragraph: Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    },
    text: String::new(),
  }
}

#[hotpath::measure]
pub(super) fn table_cell_paragraph_block_ix(cell: &TableCell, preferred: usize) -> Option<usize> {
  if matches!(cell.blocks.get(preferred), Some(TableCellBlock::Paragraph(_))) {
    return Some(preferred);
  }
  cell
    .blocks
    .iter()
    .position(|block| matches!(block, TableCellBlock::Paragraph(_)))
}

#[hotpath::measure]
fn previous_table_cell_paragraph_block_ix(cell: &TableCell, current_ix: usize) -> Option<usize> {
  cell
    .blocks
    .get(..current_ix)?
    .iter()
    .rposition(|block| matches!(block, TableCellBlock::Paragraph(_)))
}

#[hotpath::measure]
fn next_table_cell_paragraph_block_ix(cell: &TableCell, current_ix: usize) -> Option<usize> {
  cell
    .blocks
    .iter()
    .enumerate()
    .skip(current_ix.saturating_add(1))
    .find_map(|(ix, block)| matches!(block, TableCellBlock::Paragraph(_)).then_some(ix))
}

#[hotpath::measure]
pub(super) fn split_table_cell_paragraph_at(cell: &mut TableCell, paragraph_block_ix: usize, byte: usize) -> Option<usize> {
  let paragraph_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix).unwrap_or_else(|| {
    cell
      .blocks
      .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
    cell.blocks.len() - 1
  });
  let TableCellBlock::Paragraph(paragraph) = cell.blocks.get_mut(paragraph_ix)? else {
    return None;
  };
  let byte = byte.min(paragraph.text.len());
  if !paragraph.text.is_char_boundary(byte) {
    return None;
  }
  let right_text = paragraph.text[byte..].to_string();
  paragraph.text.truncate(byte);
  let (left_runs, right_runs) = split_runs_at(&paragraph.paragraph.runs, byte);
  paragraph.paragraph.runs = left_runs;
  paragraph.paragraph.byte_range = 0..paragraph.text.len();
  paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
  let new_paragraph = TableCellParagraph {
    paragraph: Paragraph {
      style: paragraph.paragraph.style,
      byte_range: 0..right_text.len(),
      runs: right_runs,
      version: paragraph.paragraph.version,
    },
    text: right_text,
  };
  cell
    .blocks
    .insert(paragraph_ix + 1, TableCellBlock::Paragraph(new_paragraph));
  Some(paragraph_ix + 1)
}

#[hotpath::measure]
pub(super) fn insert_table_cell_paragraphs_at(
  cell: &mut TableCell,
  paragraph_block_ix: usize,
  byte: usize,
  paragraphs: &[InputParagraph],
) -> Option<(usize, usize)> {
  if paragraphs.is_empty() {
    return Some((paragraph_block_ix, byte));
  }
  let paragraph_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix).unwrap_or_else(|| {
    cell
      .blocks
      .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
    cell.blocks.len() - 1
  });
  let TableCellBlock::Paragraph(paragraph) = cell.blocks.get_mut(paragraph_ix)? else {
    return None;
  };
  let byte = byte.min(paragraph.text.len());
  if !paragraph.text.is_char_boundary(byte) {
    return None;
  }

  let right_text = paragraph.text[byte..].to_string();
  paragraph.text.truncate(byte);
  let (left_runs, right_runs) = split_runs_at(&paragraph.paragraph.runs, byte);
  paragraph.paragraph.runs = left_runs;
  paragraph.paragraph.byte_range = 0..paragraph.text.len();
  paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);

  let mut inserted = paragraphs
    .iter()
    .map(table_cell_paragraph_from_input_paragraph)
    .collect::<Vec<_>>();
  let first = inserted.remove(0);
  let first_insert_start = paragraph.text.len();
  paragraph.text.push_str(&first.text);
  let mut paragraph_runs = std::mem::take(&mut paragraph.paragraph.runs);
  paragraph_runs.extend(first.paragraph.runs);
  paragraph.paragraph.runs = merge_adjacent_runs(paragraph_runs);
  paragraph.paragraph.byte_range = 0..paragraph.text.len();
  paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
  let mut caret_block_ix = paragraph_ix;
  let mut caret_byte = first_insert_start + first.text.len();

  for (insert_ix, inserted_paragraph) in (paragraph_ix + 1..).zip(inserted) {
    caret_block_ix = insert_ix;
    caret_byte = inserted_paragraph.text.len();
    cell
      .blocks
      .insert(insert_ix, TableCellBlock::Paragraph(inserted_paragraph));
  }

  let TableCellBlock::Paragraph(caret_paragraph) = cell.blocks.get_mut(caret_block_ix)? else {
    return None;
  };
  caret_paragraph.text.push_str(&right_text);
  let mut caret_runs = std::mem::take(&mut caret_paragraph.paragraph.runs);
  caret_runs.extend(right_runs);
  caret_paragraph.paragraph.runs = merge_adjacent_runs(caret_runs);
  caret_paragraph.paragraph.byte_range = 0..caret_paragraph.text.len();
  caret_paragraph.paragraph.version = caret_paragraph.paragraph.version.wrapping_add(1);
  Some((caret_block_ix, caret_byte))
}

#[hotpath::measure]
pub(super) fn merge_table_cell_paragraph_with_previous(cell: &mut TableCell, paragraph_block_ix: usize) -> Option<(usize, usize)> {
  let current_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix)?;
  let previous_ix = previous_table_cell_paragraph_block_ix(cell, current_ix)?;
  let TableCellBlock::Paragraph(current) = cell.blocks.remove(current_ix) else {
    return None;
  };
  let TableCellBlock::Paragraph(previous) = cell.blocks.get_mut(previous_ix)? else {
    return None;
  };
  let caret = previous.text.len();
  previous.text.push_str(&current.text);
  let mut runs = std::mem::take(&mut previous.paragraph.runs);
  runs.extend(current.paragraph.runs);
  previous.paragraph.runs = merge_adjacent_runs(runs);
  previous.paragraph.byte_range = 0..previous.text.len();
  previous.paragraph.version = previous.paragraph.version.wrapping_add(1);
  Some((previous_ix, caret))
}

#[hotpath::measure]
fn merge_table_cell_paragraph_with_next(cell: &mut TableCell, paragraph_block_ix: usize) -> Option<(usize, usize)> {
  let current_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix)?;
  let next_ix = next_table_cell_paragraph_block_ix(cell, current_ix)?;
  let TableCellBlock::Paragraph(next) = cell.blocks.remove(next_ix) else {
    return None;
  };
  let TableCellBlock::Paragraph(current) = cell.blocks.get_mut(current_ix)? else {
    return None;
  };
  let caret = current.text.len();
  current.text.push_str(&next.text);
  let mut runs = std::mem::take(&mut current.paragraph.runs);
  runs.extend(next.paragraph.runs);
  current.paragraph.runs = merge_adjacent_runs(runs);
  current.paragraph.byte_range = 0..current.text.len();
  current.paragraph.version = current.paragraph.version.wrapping_add(1);
  Some((current_ix, caret))
}

#[hotpath::measure]
fn table_cell_styles_at(cell_paragraph: &TableCellParagraph, byte: usize) -> RunStyles {
  let (run_ix, _) = run_containing(&cell_paragraph.paragraph, byte.min(cell_paragraph.text.len()));
  cell_paragraph
    .paragraph
    .runs
    .get(run_ix)
    .map(|run| run.styles)
    .unwrap_or_default()
}

#[hotpath::measure]
fn insert_text_in_table_cell_paragraph(cell_paragraph: &mut TableCellParagraph, byte: usize, text: &str, styles: RunStyles) {
  if text.is_empty() {
    return;
  }
  let byte = byte.min(cell_paragraph.text.len());
  if !cell_paragraph.text.is_char_boundary(byte) {
    return;
  }
  let insert_len = text.len();
  cell_paragraph.text.insert_str(byte, text);
  let (mut left, right) = split_runs_at(&cell_paragraph.paragraph.runs, byte);
  left.push(TextRun { len: insert_len, styles });
  left.extend(right);
  cell_paragraph.paragraph.runs = merge_adjacent_runs(left);
  cell_paragraph.paragraph.byte_range = 0..cell_paragraph.text.len();
  cell_paragraph.paragraph.version = cell_paragraph.paragraph.version.wrapping_add(1);
}

#[hotpath::measure]
fn delete_range_in_table_cell_paragraph(cell_paragraph: &mut TableCellParagraph, range: Range<usize>) {
  let start = range.start.min(cell_paragraph.text.len());
  let end = range.end.min(cell_paragraph.text.len()).max(start);
  if start == end || !cell_paragraph.text.is_char_boundary(start) || !cell_paragraph.text.is_char_boundary(end) {
    return;
  }
  cell_paragraph.text.replace_range(start..end, "");
  let mut output = Vec::new();
  let mut offset = 0;
  for run in std::mem::take(&mut cell_paragraph.paragraph.runs) {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= start || run_start >= end {
      output.push(run);
      continue;
    }
    if run_start < start {
      output.push(TextRun {
        len: start - run_start,
        styles: run.styles,
      });
    }
    if run_end > end {
      output.push(TextRun {
        len: run_end - end,
        styles: run.styles,
      });
    }
  }
  cell_paragraph.paragraph.runs = merge_adjacent_runs(output);
  cell_paragraph.paragraph.byte_range = 0..cell_paragraph.text.len();
  cell_paragraph.paragraph.version = cell_paragraph.paragraph.version.wrapping_add(1);
}

#[hotpath::measure]
fn table_cell_range_all_run_styles(cell_paragraph: &TableCellParagraph, range: Range<usize>, predicate: impl Fn(RunStyles) -> bool) -> bool {
  if range.start >= range.end {
    return false;
  }
  let mut offset = 0;
  let mut saw_run = false;
  for run in &cell_paragraph.paragraph.runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= range.start || run_start >= range.end {
      continue;
    }
    saw_run = true;
    if !predicate(run.styles) {
      return false;
    }
  }
  saw_run
}

#[hotpath::measure]
pub(super) fn mutate_table_cell_runs_in_range(
  cell_paragraph: &mut TableCellParagraph,
  range: Range<usize>,
  mut mutate: impl FnMut(&mut RunStyles),
) {
  let start = range.start.min(cell_paragraph.text.len());
  let end = range.end.min(cell_paragraph.text.len());
  if start >= end {
    return;
  }
  let mut new_runs = Vec::with_capacity(cell_paragraph.paragraph.runs.len() + 2);
  let mut offset = 0;
  let old_runs = std::mem::take(&mut cell_paragraph.paragraph.runs);
  for run in &old_runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= start || run_start >= end {
      new_runs.push(run.clone());
      continue;
    }
    if run_start < start {
      new_runs.push(TextRun {
        len: start - run_start,
        styles: run.styles,
      });
    }
    let selected_start = run_start.max(start);
    let selected_end = run_end.min(end);
    let mut selected_styles = run.styles;
    mutate(&mut selected_styles);
    new_runs.push(TextRun {
      len: selected_end - selected_start,
      styles: selected_styles,
    });
    if run_end > end {
      new_runs.push(TextRun {
        len: run_end - end,
        styles: run.styles,
      });
    }
  }
  cell_paragraph.paragraph.runs = merge_adjacent_runs(new_runs);
  cell_paragraph.paragraph.version = cell_paragraph.paragraph.version.wrapping_add(1);
}

