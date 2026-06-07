#[hotpath::measure]
fn benchmark_index_paths(document: &Document, iterations: usize) -> Vec<OperationRow> {
  let mut rows = Vec::new();
  rows.push(operation_row("paragraph_byte_range all", iterations, || {
    let mut failures = 0;
    let mut total = 0usize;
    for ix in 0..document.paragraphs.len() {
      let range = paragraph_byte_range(document, ix);
      total = total.wrapping_add(range.start).wrapping_add(range.end);
      failures += usize::from(range.start > range.end);
    }
    std::hint::black_box(total);
    failures
  }));
  rows.push(operation_row("global_to_document_offset all paragraph starts", iterations, || {
    let mut failures = 0;
    for ix in 0..document.paragraphs.len() {
      let range = paragraph_byte_range(document, ix);
      let offset = global_to_document_offset(document, range.start);
      failures += usize::from(offset.paragraph != ix || offset.byte != 0);
    }
    failures
  }));
  rows.push(operation_row("document_position_for_offset all paragraph ends", iterations, || {
    let mut failures = 0;
    for (ix, paragraph) in document.paragraphs.iter().enumerate() {
      let offset = DocumentOffset {
        paragraph: ix,
        byte: paragraph_text_len(paragraph),
      };
      failures += usize::from(document_position_for_offset(document, offset).is_none());
    }
    failures
  }));
  rows.push(operation_row("block_ix_for_paragraph scan all", iterations, || {
    let mut failures = 0;
    for ix in 0..document.paragraphs.len() {
      failures += usize::from(block_ix_for_paragraph(document, ix).is_none());
    }
    failures
  }));
  rows.push(operation_row("VisibilityIndex::build visible", iterations, || {
    let visibility = VisibilityIndex::build(document, false);
    let mut visible = 0usize;
    for ix in 0..document.blocks.len() {
      visible += usize::from(visibility.is_visible(ix));
    }
    std::hint::black_box(visible);
    0
  }));
  rows.push(operation_row("VisibilityIndex::build invisibility", iterations, || {
    let visibility = VisibilityIndex::build(document, true);
    let mut visible = 0usize;
    for ix in 0..document.blocks.len() {
      visible += usize::from(visibility.is_visible(ix));
    }
    std::hint::black_box(visible);
    0
  }));
  rows
}

#[hotpath::measure]
fn benchmark_edit_paths(document: &Document, stats: &DocumentStats, iterations: usize) -> Vec<OperationRow> {
  let mut rows = Vec::new();
  let largest = stats
    .largest_paragraph_ix
    .min(document.paragraphs.len().saturating_sub(1));
  let fragmented = stats
    .most_runs_paragraph_ix
    .min(document.paragraphs.len().saturating_sub(1));
  let largest_mid = safe_mid_byte(document, largest);
  let largest_first_char = first_char_range(document, largest);

  rows.push(operation_row("full_document_text", iterations, || {
    let text = full_document_text(document);
    std::hint::black_box(text.len());
    0
  }));
  rows.push(operation_row("find_text_ranges \"the\"", iterations, || {
    let ranges = find_text_ranges(document, "the");
    std::hint::black_box(ranges.len());
    0
  }));
  rows.push(operation_row("selected_plain_text first window", iterations, || {
    let range = first_window_range(document, 24);
    let text = selected_plain_text(document, range);
    std::hint::black_box(text.len());
    0
  }));
  rows.push(operation_row("selected_rich_fragment first window", iterations, || {
    let range = first_window_range(document, 24);
    let fragment = selected_rich_fragment(document, range);
    std::hint::black_box(fragment.paragraphs.len());
    0
  }));
  rows.push(operation_row("merge_adjacent_runs all runs", iterations, || {
    let runs = document
      .paragraphs
      .iter()
      .flat_map(|paragraph| paragraph.runs.iter().cloned())
      .collect::<Vec<_>>();
    let merged = merge_adjacent_runs(runs);
    std::hint::black_box(merged.len());
    0
  }));
  rows.push(operation_row("insert_text_at largest paragraph midpoint", iterations, || {
    let mut clone = document.clone();
    insert_text_at(&mut clone, largest, largest_mid, "x", RunStyles::default());
    check_document_fidelity(&clone).failures.len()
  }));
  rows.push(operation_row("delete_range_in_paragraph first char", iterations, || {
    let mut clone = document.clone();
    if let Some(range) = largest_first_char.clone() {
      delete_range_in_paragraph(&mut clone, largest, range);
    }
    check_document_fidelity(&clone).failures.len()
  }));
  rows.push(operation_row("apply_style_to_paragraph_range fragmented paragraph", iterations, || {
    let mut clone = document.clone();
    let end = paragraph_text_len(&clone.paragraphs[fragmented]).min(safe_mid_byte(&clone, fragmented).max(1));
    if end > 0 {
      apply_style_to_paragraph_range(&mut clone, fragmented, 0..end, RunStyle::Highlight(1));
    }
    check_document_fidelity(&clone).failures.len()
  }));
  rows.push(operation_row("split_paragraph_at largest midpoint", iterations, || {
    let mut clone = document.clone();
    if paragraph_text_len(&clone.paragraphs[largest]) > 0 {
      let _ = split_paragraph_at(&mut clone, largest, largest_mid);
    }
    check_document_fidelity(&clone).failures.len()
  }));
  rows.push(operation_row("delete_cross_paragraph_range first window", iterations, || {
    let mut clone = document.clone();
    if clone.paragraphs.len() > 1 {
      let end_paragraph = (clone.paragraphs.len() - 1).min(10);
      let end_byte = paragraph_text_len(&clone.paragraphs[end_paragraph]).min(safe_mid_byte(&clone, end_paragraph).max(1));
      delete_cross_paragraph_range(
        &mut clone,
        DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
          paragraph: end_paragraph,
          byte: end_byte,
        },
      );
    }
    check_document_fidelity(&clone).failures.len()
  }));
  rows.push(operation_row("insert_rich_fragment_at first window", iterations, || {
    let fragment = selected_rich_fragment(document, first_window_range(document, 8));
    let mut clone = document.clone();
    insert_rich_fragment_at(&mut clone, DocumentOffset::default(), &fragment);
    check_document_fidelity(&clone).failures.len()
  }));

  rows
}

#[hotpath::measure]
fn operation_row(name: &str, iterations: usize, mut run: impl FnMut() -> usize) -> OperationRow {
  let mut timings = Vec::with_capacity(iterations);
  let mut failures = 0usize;
  for _ in 0..iterations.max(1) {
    let started = Instant::now();
    failures += run();
    timings.push(started.elapsed());
  }
  OperationRow {
    name: name.to_string(),
    duration: DurationStats::from_samples(&timings),
    fidelity_failures: failures,
  }
}
