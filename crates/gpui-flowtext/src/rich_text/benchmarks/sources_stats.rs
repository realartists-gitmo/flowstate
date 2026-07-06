#[hotpath::measure]
fn benchmark_sources(options: &BenchmarkOptions) -> Vec<BenchmarkSource> {
  if !options.paths.is_empty() {
    return options
      .paths
      .iter()
      .cloned()
      .map(BenchmarkSource::Path)
      .collect();
  }

  vec![BenchmarkSource::Demo]
}

#[hotpath::measure]
fn load_document_source(source: &BenchmarkSource, iterations: usize) -> Result<LoadedDocument, String> {
  let iterations = iterations.max(1);
  let mut timings = Vec::with_capacity(iterations);
  let mut document = None;

  for _ in 0..iterations {
    let started = Instant::now();
    let loaded = match source {
      BenchmarkSource::Path(path) => {
        return Err(format!(
          "loading {} requires a host Loro package adapter",
          path.display()
        ));
      },
      BenchmarkSource::Demo => demo_document(),
    };
    timings.push(started.elapsed());
    document = Some(loaded);
  }

  let (path, file_bytes) = match source {
    BenchmarkSource::Path(path) => (Some(path.clone()), fs::metadata(path).ok().map(|metadata| metadata.len())),
    BenchmarkSource::Demo => (None, None),
  };

  Ok(LoadedDocument {
    label: source_label(source),
    path,
    file_bytes,
    document: document.expect("at least one benchmark load iteration"),
    load: DurationStats::from_samples(&timings),
  })
}

#[hotpath::measure]
fn source_label(source: &BenchmarkSource) -> String {
  match source {
    BenchmarkSource::Path(path) => path
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| path.display().to_string()),
    BenchmarkSource::Demo => "demo_document".to_string(),
  }
}

#[hotpath::measure]
fn document_stats(document: &DocumentProjection) -> DocumentStats {
  let mut stats = DocumentStats {
    text_bytes: document.text.byte_len(),
    text_chars: full_document_text(document).chars().count(),
    paragraphs: document.paragraphs.len(),
    blocks: document.blocks.len(),
    assets: document.assets.assets.len(),
    asset_bytes: document
      .assets
      .assets
      .values()
      .map(|asset| asset.bytes.len())
      .sum(),
    ..Default::default()
  };

  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    *stats.paragraph_styles.entry(paragraph.style).or_default() += 1;
    let paragraph_len = paragraph_text_len(paragraph);
    if paragraph_len == 0 {
      stats.empty_paragraphs += 1;
    }
    if paragraph_len > stats.max_paragraph_bytes {
      stats.max_paragraph_bytes = paragraph_len;
      stats.largest_paragraph_ix = paragraph_ix;
    }
    if paragraph.runs.len() > stats.max_runs_per_paragraph {
      stats.max_runs_per_paragraph = paragraph.runs.len();
      stats.most_runs_paragraph_ix = paragraph_ix;
    }
    stats.runs += paragraph.runs.len();
    for (run_ix, run) in paragraph.runs.iter().enumerate() {
      if run.len == 0 {
        stats.empty_runs += 1;
      }
      if run_ix > 0 && paragraph.runs[run_ix - 1].styles == run.styles {
        stats.adjacent_mergeable_runs += 1;
      }
      *stats
        .semantic_styles
        .entry(run.styles.semantic)
        .or_default() += 1;
      *stats
        .highlight_styles
        .entry(run.styles.highlight)
        .or_default() += 1;
      stats.direct_underline_runs += usize::from(run.styles.direct_underline);
      stats.strikethrough_runs += usize::from(run.styles.strikethrough);
    }
    stats.soft_line_breaks += paragraph_text(document, paragraph_ix)
      .matches(SOFT_LINE_BREAK)
      .count();
  }

  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(_) => stats.paragraph_blocks += 1,
      Block::Image(_) => stats.images += 1,
      Block::Equation(_) => stats.equations += 1,
      Block::Table(table) => accumulate_table_stats(table, &mut stats, false),
    }
  }

  stats
}

#[hotpath::measure]
fn accumulate_table_stats(table: &TableBlock, stats: &mut DocumentStats, nested: bool) {
  stats.tables += 1;
  stats.nested_tables += usize::from(nested);
  stats.table_rows += table.rows.len();
  for row in &table.rows {
    stats.table_cells += row.cells.len();
    for cell in &row.cells {
      for block in &cell.blocks {
        match block {
          TableCellBlock::Paragraph(_) => stats.table_cell_paragraphs += 1,
          TableCellBlock::Table(table) => accumulate_table_stats(table, stats, true),
        }
      }
    }
  }
}

#[hotpath::measure]
fn check_document_fidelity(document: &DocumentProjection) -> FidelityReport {
  let mut report = FidelityReport::default();
  let full_text = full_document_text(document);
  report.check(!document.paragraphs.is_empty(), "document must contain at least one paragraph");
  report.check(
    document
      .blocks
      .iter()
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count()
      == document.paragraphs.len(),
    "paragraph block count must match paragraph projection length",
  );

  let mut block_paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(paragraph) => {
        if let Some(projected) = document.paragraphs.get(block_paragraph_ix) {
          report.warn_if(
            paragraph.style != projected.style || paragraph.runs != projected.runs || paragraph.version != projected.version,
            format!("block {block_ix} paragraph payload differs from paragraph projection {block_paragraph_ix}"),
          );
        }
        block_paragraph_ix += 1;
      },
      Block::Image(image) => {
        report.check(
          document.assets.assets.contains_key(&image.asset_id),
          format!("image block {block_ix} must reference an existing asset"),
        );
      },
      Block::Equation(_) => {},
      Block::Table(table) => check_table_fidelity(table, &mut report, &format!("table block {block_ix}")),
    }
  }

  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let expected_range = paragraph_byte_range(document, paragraph_ix);
    report.check(
      full_text.is_char_boundary(expected_range.start) && full_text.is_char_boundary(expected_range.end),
      format!("paragraph {paragraph_ix} byte range must be on UTF-8 boundaries"),
    );
    report.check(
      paragraph_text_len(paragraph) == paragraph.runs.iter().map(|run| run.len).sum::<usize>(),
      format!("paragraph {paragraph_ix} run lengths must sum to paragraph text length"),
    );
    report.warn_if(
      paragraph
        .runs
        .windows(2)
        .any(|runs| runs[0].styles == runs[1].styles),
      format!("paragraph {paragraph_ix} has adjacent runs that could be merged"),
    );
  }

  report.warn_if(
    document
      .paragraphs
      .iter()
      .any(|paragraph| paragraph.runs.iter().any(|run| run.len == 0)),
    "document contains zero-length runs",
  );
  report
}

#[hotpath::measure]
fn check_table_fidelity(table: &TableBlock, report: &mut FidelityReport, label: &str) {
  let widest_row = table
    .rows
    .iter()
    .map(|row| row.cells.len())
    .max()
    .unwrap_or_default();
  report.check(
    table.columns.is_empty() || table.columns.len() == widest_row,
    format!("{label} column width count should match widest row when explicit widths are present"),
  );
  for (row_ix, row) in table.rows.iter().enumerate() {
    for (cell_ix, cell) in row.cells.iter().enumerate() {
      report.check(
        cell.row_span > 0 && cell.col_span > 0,
        format!("{label} cell {row_ix}:{cell_ix} spans must be positive"),
      );
      for (block_ix, block) in cell.blocks.iter().enumerate() {
        match block {
          TableCellBlock::Paragraph(paragraph) => {
            report.check(
              paragraph.paragraph.byte_range.len() == paragraph.text.len(),
              format!("{label} cell {row_ix}:{cell_ix} paragraph {block_ix} byte range must match cell text"),
            );
          },
          TableCellBlock::Table(table) => check_table_fidelity(table, report, &format!("{label} nested {row_ix}:{cell_ix}:{block_ix}")),
        }
      }
    }
  }
}
