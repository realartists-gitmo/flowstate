#[hotpath::measure]
fn run_benchmark_suite(options: &BenchmarkOptions, window: &mut Window, cx: &mut Context<BenchmarkRunner>) -> String {
  let mut report = String::new();
  let started = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| duration.as_secs())
    .unwrap_or_default();
  let iterations = options.iterations.max(1);
  let widths = if options.widths.is_empty() {
    DEFAULT_WIDTHS.to_vec()
  } else {
    options.widths.clone()
  };

  let _ = writeln!(report, "# Rich Text Element Benchmark Report");
  let _ = writeln!(report);
  let _ = writeln!(report, "- unix_time: `{started}`");
  let _ = writeln!(report, "- build_profile: `{}`", build_profile());
  let _ = writeln!(report, "- iterations_per_microbenchmark: `{iterations}`");
  let _ = writeln!(
    report,
    "- widths_px: `{}`",
    widths
      .iter()
      .map(|width| format!("{width:.0}"))
      .collect::<Vec<_>>()
      .join(", ")
  );
  let _ = writeln!(report, "- paint_benchmarks: `{}`", options.include_paint);
  let _ = writeln!(report);

  let sources = benchmark_sources(options);
  if sources.is_empty() {
    let _ = writeln!(report, "No `.gptx` files or explicit benchmark documents were found.");
    return report;
  }

  let mut loaded_documents = Vec::new();
  let _ = writeln!(report, "## Load Summary");
  let _ = writeln!(report);
  let _ = writeln!(report, "| document | file bytes | load min ms | load mean ms | load max ms | status |");
  let _ = writeln!(report, "|---|---:|---:|---:|---:|---|");

  for source in sources {
    match load_document_source(&source, iterations) {
      Ok(loaded) => {
        let file_bytes = loaded
          .file_bytes
          .map(|bytes| bytes.to_string())
          .unwrap_or_else(|| "n/a".to_string());
        let _ = writeln!(
          report,
          "| {} | {} | {:.3} | {:.3} | {:.3} | ok |",
          md(&loaded.label),
          file_bytes,
          ms(loaded.load.min),
          ms(loaded.load.mean),
          ms(loaded.load.max)
        );
        loaded_documents.push(loaded);
      },
      Err(error) => {
        let _ = writeln!(report, "| {} | n/a | n/a | n/a | n/a | {} |", md(&source_label(&source)), md(&error));
      },
    }
  }

  let _ = writeln!(report);
  let _ = writeln!(report, "## Corpus Summary");
  let _ = writeln!(report);
  let _ = writeln!(
    report,
    "| document | paragraphs | blocks | text bytes | runs | max paragraph bytes | max runs/paragraph | objects | tables/cells | assets bytes | fidelity |"
  );
  let _ = writeln!(report, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|");

  let mut document_sections = Vec::new();
  for loaded in &loaded_documents {
    let stats = document_stats(&loaded.document);
    let fidelity = check_document_fidelity(&loaded.document);
    let status = if fidelity.failures.is_empty() { "pass" } else { "fail" };
    let _ = writeln!(
      report,
      "| {} | {} | {} | {} | {} | {} | {} | {} | {}/{} | {} | {} |",
      md(&loaded.label),
      stats.paragraphs,
      stats.blocks,
      stats.text_bytes,
      stats.runs,
      stats.max_paragraph_bytes,
      stats.max_runs_per_paragraph,
      stats.images + stats.equations + stats.tables,
      stats.tables,
      stats.table_cells,
      stats.asset_bytes,
      status
    );

    document_sections.push(benchmark_document(
      loaded,
      &stats,
      fidelity,
      &widths,
      iterations,
      options.include_paint,
      window,
      cx,
    ));
  }

  for section in document_sections {
    report.push_str(&section);
  }

  report
}

#[hotpath::measure]
fn benchmark_document(
  loaded: &LoadedDocument,
  stats: &DocumentStats,
  fidelity: FidelityReport,
  widths: &[f32],
  iterations: usize,
  include_paint: bool,
  window: &mut Window,
  cx: &mut Context<BenchmarkRunner>,
) -> String {
  let mut out = String::new();
  let document = &loaded.document;
  let fingerprint = fingerprint_document(document);

  let _ = writeln!(out);
  let _ = writeln!(out, "## {}", md(&loaded.label));
  let _ = writeln!(out);
  if let Some(path) = &loaded.path {
    let _ = writeln!(out, "- source: `{}`", path.display());
  }
  let _ = writeln!(out, "- fingerprint: `{fingerprint:016x}`");
  let _ = writeln!(out, "- text bytes/chars: `{}` / `{}`", stats.text_bytes, stats.text_chars);
  let _ = writeln!(
    out,
    "- paragraphs/blocks/runs: `{}` / `{}` / `{}`",
    stats.paragraphs, stats.blocks, stats.runs
  );
  let _ = writeln!(
    out,
    "- largest paragraph: `#{}` with `{}` bytes",
    stats.largest_paragraph_ix, stats.max_paragraph_bytes
  );
  let _ = writeln!(
    out,
    "- most fragmented paragraph: `#{}` with `{}` runs",
    stats.most_runs_paragraph_ix, stats.max_runs_per_paragraph
  );
  let _ = writeln!(out);

  write_stats_tables(&mut out, stats);
  write_fidelity_report(&mut out, &fidelity);
  write_roundtrip_report(&mut out, loaded, iterations);

  let index_rows = benchmark_index_paths(document, iterations);
  write_operation_table(&mut out, "Index And Mapping Benchmarks", &index_rows);

  let edit_rows = benchmark_edit_paths(document, stats, iterations);
  write_operation_table(&mut out, "Edit And Clipboard Benchmarks", &edit_rows);

  let style_rows = benchmark_styles(document, stats, iterations);
  write_operation_table(&mut out, "Style-Heavy Benchmarks", &style_rows);

  let layout_rows = benchmark_layout_paths(document, widths, iterations, include_paint, window, cx);
  write_layout_table(&mut out, &layout_rows);

  let paragraph_rows = benchmark_sample_paragraph_layouts(document, stats, widths, iterations, window, cx);
  write_paragraph_layout_table(&mut out, &paragraph_rows);

  out
}

