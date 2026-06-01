#[hotpath::measure]
fn write_stats_tables(out: &mut String, stats: &DocumentStats) {
  let _ = writeln!(out, "### Model Shape");
  let _ = writeln!(out);
  let _ = writeln!(out, "| metric | value |");
  let _ = writeln!(out, "|---|---:|");
  let _ = writeln!(out, "| paragraph blocks | {} |", stats.paragraph_blocks);
  let _ = writeln!(out, "| images | {} |", stats.images);
  let _ = writeln!(out, "| equations | {} |", stats.equations);
  let _ = writeln!(out, "| tables | {} |", stats.tables);
  let _ = writeln!(out, "| table rows | {} |", stats.table_rows);
  let _ = writeln!(out, "| table cells | {} |", stats.table_cells);
  let _ = writeln!(out, "| table cell paragraphs | {} |", stats.table_cell_paragraphs);
  let _ = writeln!(out, "| nested tables | {} |", stats.nested_tables);
  let _ = writeln!(out, "| assets | {} |", stats.assets);
  let _ = writeln!(out, "| asset bytes | {} |", stats.asset_bytes);
  let _ = writeln!(out, "| empty paragraphs | {} |", stats.empty_paragraphs);
  let _ = writeln!(out, "| empty runs | {} |", stats.empty_runs);
  let _ = writeln!(out, "| adjacent mergeable runs | {} |", stats.adjacent_mergeable_runs);
  let _ = writeln!(out, "| soft line breaks | {} |", stats.soft_line_breaks);
  let _ = writeln!(out);

  let _ = writeln!(out, "### Style Distribution");
  let _ = writeln!(out);
  let _ = writeln!(out, "| family | style | count |");
  let _ = writeln!(out, "|---|---|---:|");
  for (style, name) in paragraph_style_names() {
    let _ = writeln!(
      out,
      "| paragraph | {name} | {} |",
      stats
        .paragraph_styles
        .get(&style)
        .copied()
        .unwrap_or_default()
    );
  }
  for (style, name) in semantic_style_names() {
    let _ = writeln!(
      out,
      "| run semantic | {name} | {} |",
      stats
        .semantic_styles
        .get(&style)
        .copied()
        .unwrap_or_default()
    );
  }
  for (style, name) in highlight_style_names() {
    let _ = writeln!(
      out,
      "| highlight | {name} | {} |",
      stats
        .highlight_styles
        .get(&style)
        .copied()
        .unwrap_or_default()
    );
  }
  let _ = writeln!(out, "| direct | underline | {} |", stats.direct_underline_runs);
  let _ = writeln!(out, "| direct | strikethrough | {} |", stats.strikethrough_runs);
  let _ = writeln!(out);
}

#[hotpath::measure]
fn write_fidelity_report(out: &mut String, fidelity: &FidelityReport) {
  let _ = writeln!(out, "### Fidelity Checks");
  let _ = writeln!(out);
  let _ = writeln!(out, "- checks: `{}`", fidelity.checks);
  let _ = writeln!(out, "- failures: `{}`", fidelity.failures.len());
  let _ = writeln!(out, "- warnings: `{}`", fidelity.warnings.len());
  if !fidelity.failures.is_empty() {
    let _ = writeln!(out);
    let _ = writeln!(out, "Failures:");
    for failure in fidelity.failures.iter().take(50) {
      let _ = writeln!(out, "- {}", md(failure));
    }
  }
  if !fidelity.warnings.is_empty() {
    let _ = writeln!(out);
    let _ = writeln!(out, "Warnings:");
    for warning in fidelity.warnings.iter().take(50) {
      let _ = writeln!(out, "- {}", md(warning));
    }
    if fidelity.warnings.len() > 50 {
      let _ = writeln!(out, "- ... {} more warnings", fidelity.warnings.len() - 50);
    }
  }
  let _ = writeln!(out);
}

#[hotpath::measure]
fn write_roundtrip_report(out: &mut String, loaded: &LoadedDocument, iterations: usize) {
  let mut write_timings = Vec::with_capacity(iterations);
  let mut read_timings = Vec::with_capacity(iterations);
  let mut fingerprint_matches = true;
  let mut last_error = None;
  let original = fingerprint_document(&loaded.document);

  for _ in 0..iterations {
    let result = (|| {
      let dir = tempdir().map_err(|error| error.to_string())?;
      let path = dir.path().join("roundtrip.db8");
      let write_start = Instant::now();
      write_db8(&path, &loaded.document).map_err(|error| error.to_string())?;
      write_timings.push(write_start.elapsed());
      let read_start = Instant::now();
      let roundtrip = read_db8(&path).map_err(|error| error.to_string())?;
      read_timings.push(read_start.elapsed());
      fingerprint_matches &= fingerprint_document(&roundtrip) == original;
      Ok::<_, String>(())
    })();
    if let Err(error) = result {
      last_error = Some(error);
      break;
    }
  }

  let _ = writeln!(out, "### DB8 Roundtrip");
  let _ = writeln!(out);
  match last_error {
    Some(error) => {
      let _ = writeln!(out, "- status: `failed`");
      let _ = writeln!(out, "- error: `{}`", md(&error));
    },
    None => {
      let write_stats = DurationStats::from_samples(&write_timings);
      let read_stats = DurationStats::from_samples(&read_timings);
      let _ = writeln!(out, "| phase | min ms | mean ms | max ms | fingerprint match |");
      let _ = writeln!(out, "|---|---:|---:|---:|---|");
      let _ = writeln!(
        out,
        "| write_db8 | {:.3} | {:.3} | {:.3} | n/a |",
        ms(write_stats.min),
        ms(write_stats.mean),
        ms(write_stats.max)
      );
      let _ = writeln!(
        out,
        "| read_db8 roundtrip | {:.3} | {:.3} | {:.3} | {} |",
        ms(read_stats.min),
        ms(read_stats.mean),
        ms(read_stats.max),
        fingerprint_matches
      );
    },
  }
  let _ = writeln!(out);
}

