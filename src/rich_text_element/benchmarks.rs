use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    hash::{Hash, Hasher},
    ops::Range,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{
    div, point, prelude::*, px, size, Bounds, Context, IntoElement, Pixels, Render, Window,
};
use tempfile::tempdir;

use super::*;

const DEFAULT_WIDTHS: &[f32] = &[720.0, 900.0, 1100.0, 1440.0];
const DEFAULT_ITERATIONS: usize = 3;

#[derive(Clone, Debug)]
pub struct BenchmarkOptions {
    pub paths: Vec<PathBuf>,
    pub output_path: PathBuf,
    pub iterations: usize,
    pub widths: Vec<f32>,
    pub include_paint: bool,
}

impl Default for BenchmarkOptions {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            output_path: PathBuf::from("benchmark_results.md"),
            iterations: DEFAULT_ITERATIONS,
            widths: DEFAULT_WIDTHS.to_vec(),
            include_paint: true,
        }
    }
}

pub struct BenchmarkRunner {
    options: BenchmarkOptions,
    state: BenchmarkState,
}

#[derive(Clone, Debug)]
enum BenchmarkState {
    Queued,
    Starting,
    Running,
    Complete { output_path: PathBuf },
    Failed { message: String },
}

impl BenchmarkRunner {
    pub fn new(options: BenchmarkOptions) -> Self {
        Self {
            options,
            state: BenchmarkState::Queued,
        }
    }
}

impl Render for BenchmarkRunner {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.state, BenchmarkState::Queued) {
            self.state = BenchmarkState::Starting;
            let runner = cx.entity();
            window.on_next_frame(move |window, cx| {
                runner.update(cx, |runner, cx| runner.mark_running(window, cx));
            });
        }

        div()
            .size_full()
            .bg(gpui::rgb(0xffffff))
            .text_color(gpui::rgb(0x111111))
            .p_6()
            .text_size(px(16.0))
            .font_family(".SystemUIFont")
            .child(self.status_text())
    }
}

impl BenchmarkRunner {
    fn mark_running(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state = BenchmarkState::Running;
        cx.notify();

        let runner = cx.entity();
        window.on_next_frame(move |window, cx| {
            runner.update(cx, |runner, cx| runner.run_to_completion(window, cx));
        });
    }

    fn run_to_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let report = run_benchmark_suite(&self.options, window, cx);
        match fs::write(&self.options.output_path, report.as_bytes()) {
            Ok(()) => {
                println!(
                    "benchmark report written to {}",
                    self.options.output_path.display()
                );
                println!("{report}");
                self.state = BenchmarkState::Complete {
                    output_path: self.options.output_path.clone(),
                };
            }
            Err(error) => {
                let message = format!(
                    "failed to write benchmark report to {}: {error}",
                    self.options.output_path.display()
                );
                eprintln!("{message}");
                println!("{report}");
                self.state = BenchmarkState::Failed { message };
            }
        }
        cx.notify();
        window.on_next_frame(|_, cx| cx.quit());
    }

    fn status_text(&self) -> String {
        match &self.state {
            BenchmarkState::Queued => "Benchmark mode queued.".to_string(),
            BenchmarkState::Starting => {
                "Benchmark mode starting. The suite will begin after this window paints."
                    .to_string()
            }
            BenchmarkState::Running => format!(
                "Benchmark mode running. This window may stop responding while GPUI layout and paint paths are measured.\nReport target: {}",
                self.options.output_path.display()
            ),
            BenchmarkState::Complete { output_path } => {
                format!("Benchmark complete.\nReport written to: {}", output_path.display())
            }
            BenchmarkState::Failed { message } => format!("Benchmark failed.\n{message}"),
        }
    }
}

#[derive(Clone)]
enum BenchmarkSource {
    Path(PathBuf),
    Demo,
}

struct LoadedDocument {
    label: String,
    path: Option<PathBuf>,
    file_bytes: Option<u64>,
    document: Document,
    load: DurationStats,
}

#[derive(Clone, Copy, Debug)]
struct DurationStats {
    min: Duration,
    mean: Duration,
    max: Duration,
    samples: usize,
}

impl DurationStats {
    fn from_samples(samples: &[Duration]) -> Self {
        let samples_len = samples.len().max(1);
        let min = samples.iter().copied().min().unwrap_or_default();
        let max = samples.iter().copied().max().unwrap_or_default();
        let total = samples.iter().copied().sum::<Duration>();
        let mean = div_duration(total, samples_len as u32);
        Self {
            min,
            mean,
            max,
            samples: samples.len(),
        }
    }
}

#[derive(Default, Clone)]
struct DocumentStats {
    text_bytes: usize,
    text_chars: usize,
    paragraphs: usize,
    blocks: usize,
    paragraph_blocks: usize,
    images: usize,
    equations: usize,
    tables: usize,
    table_rows: usize,
    table_cells: usize,
    table_cell_paragraphs: usize,
    nested_tables: usize,
    assets: usize,
    asset_bytes: usize,
    runs: usize,
    empty_paragraphs: usize,
    empty_runs: usize,
    adjacent_mergeable_runs: usize,
    soft_line_breaks: usize,
    max_paragraph_bytes: usize,
    max_runs_per_paragraph: usize,
    largest_paragraph_ix: usize,
    most_runs_paragraph_ix: usize,
    paragraph_styles: HashMap<ParagraphStyle, usize>,
    semantic_styles: HashMap<RunSemanticStyle, usize>,
    highlight_styles: HashMap<Option<HighlightStyle>, usize>,
    direct_underline_runs: usize,
    strikethrough_runs: usize,
}

#[derive(Default)]
struct FidelityReport {
    checks: usize,
    failures: Vec<String>,
    warnings: Vec<String>,
}

impl FidelityReport {
    fn check(&mut self, condition: bool, message: impl Into<String>) {
        self.checks += 1;
        if !condition {
            self.failures.push(message.into());
        }
    }

    fn warn_if(&mut self, condition: bool, message: impl Into<String>) {
        if condition {
            self.warnings.push(message.into());
        }
    }
}

#[derive(Default, Clone)]
struct LayoutSummary {
    lines: usize,
    segments: usize,
    rects: usize,
    underlines: usize,
    strikethroughs: usize,
    max_line_width: f32,
    layout_height: f32,
    fidelity_failures: usize,
}

#[derive(Clone)]
struct LayoutBenchRow {
    width: f32,
    estimate_all: DurationStats,
    visibility_visible: DurationStats,
    visibility_invisible: DurationStats,
    full_layout: DurationStats,
    reuse_layout: DurationStats,
    structural_layout: DurationStats,
    paint_plain: Option<DurationStats>,
    paint_selected: Option<DurationStats>,
    item_sizes_cold: ItemSizeBenchmarkResult,
    item_sizes_hot: ItemSizeBenchmarkResult,
    item_sizes_invisible: ItemSizeBenchmarkResult,
    estimate_mean_abs_error: f32,
    estimate_max_abs_error: f32,
    summary: LayoutSummary,
}

#[derive(Clone)]
struct ParagraphLayoutRow {
    label: String,
    paragraph_ix: usize,
    width: f32,
    normal: DurationStats,
    invisible: DurationStats,
    lines: usize,
    segments: usize,
    normal_height: f32,
    invisible_height: f32,
}

#[derive(Clone)]
struct OperationRow {
    name: String,
    duration: DurationStats,
    fidelity_failures: usize,
}

fn run_benchmark_suite(
    options: &BenchmarkOptions,
    window: &mut Window,
    cx: &mut Context<BenchmarkRunner>,
) -> String {
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
        let _ = writeln!(
            report,
            "No `.db8` files or explicit benchmark documents were found."
        );
        return report;
    }

    let mut loaded_documents = Vec::new();
    let _ = writeln!(report, "## Load Summary");
    let _ = writeln!(report);
    let _ = writeln!(
        report,
        "| document | file bytes | load min ms | load mean ms | load max ms | status |"
    );
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
            }
            Err(error) => {
                let _ = writeln!(
                    report,
                    "| {} | n/a | n/a | n/a | n/a | {} |",
                    md(&source_label(&source)),
                    md(&error)
                );
            }
        }
    }

    let _ = writeln!(report);
    let _ = writeln!(report, "## Corpus Summary");
    let _ = writeln!(report);
    let _ = writeln!(
    report,
    "| document | paragraphs | blocks | text bytes | runs | max paragraph bytes | max runs/paragraph | objects | tables/cells | assets bytes | fidelity |"
  );
    let _ = writeln!(
        report,
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"
    );

    let mut document_sections = Vec::new();
    for loaded in &loaded_documents {
        let stats = document_stats(&loaded.document);
        let fidelity = check_document_fidelity(&loaded.document);
        let status = if fidelity.failures.is_empty() {
            "pass"
        } else {
            "fail"
        };
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
    let _ = writeln!(
        out,
        "- text bytes/chars: `{}` / `{}`",
        stats.text_bytes, stats.text_chars
    );
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

    let layout_rows =
        benchmark_layout_paths(document, widths, iterations, include_paint, window, cx);
    write_layout_table(&mut out, &layout_rows);

    let paragraph_rows =
        benchmark_sample_paragraph_layouts(document, stats, widths, iterations, window, cx);
    write_paragraph_layout_table(&mut out, &paragraph_rows);

    out
}

fn benchmark_sources(options: &BenchmarkOptions) -> Vec<BenchmarkSource> {
    if !options.paths.is_empty() {
        return options
            .paths
            .iter()
            .cloned()
            .map(BenchmarkSource::Path)
            .collect();
    }

    let mut paths = fs::read_dir(".")
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "db8"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        vec![BenchmarkSource::Demo]
    } else {
        paths.into_iter().map(BenchmarkSource::Path).collect()
    }
}

fn load_document_source(
    source: &BenchmarkSource,
    iterations: usize,
) -> Result<LoadedDocument, String> {
    let iterations = iterations.max(1);
    let mut timings = Vec::with_capacity(iterations);
    let mut document = None;

    for _ in 0..iterations {
        let started = Instant::now();
        let loaded = match source {
            BenchmarkSource::Path(path) => read_db8(path).map_err(|error| error.to_string())?,
            BenchmarkSource::Demo => demo_document(),
        };
        timings.push(started.elapsed());
        document = Some(loaded);
    }

    let (path, file_bytes) = match source {
        BenchmarkSource::Path(path) => (
            Some(path.clone()),
            fs::metadata(path).ok().map(|metadata| metadata.len()),
        ),
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

fn source_label(source: &BenchmarkSource) -> String {
    match source {
        BenchmarkSource::Path(path) => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        BenchmarkSource::Demo => "demo_document".to_string(),
    }
}

fn document_stats(document: &Document) -> DocumentStats {
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

fn check_document_fidelity(document: &Document) -> FidelityReport {
    let mut report = FidelityReport::default();
    let full_text = full_document_text(document);
    report.check(
        !document.paragraphs.is_empty(),
        "document must contain at least one paragraph",
    );
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
            }
            Block::Image(image) => {
                report.check(
                    document.assets.assets.contains_key(&image.asset_id),
                    format!("image block {block_ix} must reference an existing asset"),
                );
            }
            Block::Equation(_) => {}
            Block::Table(table) => {
                check_table_fidelity(table, &mut report, &format!("table block {block_ix}"))
            }
        }
    }

    for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
        let expected_range = paragraph_byte_range(document, paragraph_ix);
        report.check(
            paragraph.byte_range == expected_range,
            format!(
                "paragraph {paragraph_ix} byte_range {:?} must match offset index {:?}",
                paragraph.byte_range, expected_range
            ),
        );
        report.check(
            full_text.is_char_boundary(expected_range.start)
                && full_text.is_char_boundary(expected_range.end),
            format!("paragraph {paragraph_ix} byte range must be on UTF-8 boundaries"),
        );
        report.check(
            paragraph_text_len(paragraph)
                == paragraph.runs.iter().map(|run| run.len).sum::<usize>(),
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

fn check_table_fidelity(table: &TableBlock, report: &mut FidelityReport, label: &str) {
    let widest_row = table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or_default();
    report.check(
        table.column_widths.is_empty() || table.column_widths.len() == widest_row,
        format!(
            "{label} column width count should match widest row when explicit widths are present"
        ),
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
                    }
                    TableCellBlock::Table(table) => check_table_fidelity(
                        table,
                        report,
                        &format!("{label} nested {row_ix}:{cell_ix}:{block_ix}"),
                    ),
                }
            }
        }
    }
}

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
    let _ = writeln!(
        out,
        "| table cell paragraphs | {} |",
        stats.table_cell_paragraphs
    );
    let _ = writeln!(out, "| nested tables | {} |", stats.nested_tables);
    let _ = writeln!(out, "| assets | {} |", stats.assets);
    let _ = writeln!(out, "| asset bytes | {} |", stats.asset_bytes);
    let _ = writeln!(out, "| empty paragraphs | {} |", stats.empty_paragraphs);
    let _ = writeln!(out, "| empty runs | {} |", stats.empty_runs);
    let _ = writeln!(
        out,
        "| adjacent mergeable runs | {} |",
        stats.adjacent_mergeable_runs
    );
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
    let _ = writeln!(
        out,
        "| direct | underline | {} |",
        stats.direct_underline_runs
    );
    let _ = writeln!(
        out,
        "| direct | strikethrough | {} |",
        stats.strikethrough_runs
    );
    let _ = writeln!(out);
}

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
        }
        None => {
            let write_stats = DurationStats::from_samples(&write_timings);
            let read_stats = DurationStats::from_samples(&read_timings);
            let _ = writeln!(
                out,
                "| phase | min ms | mean ms | max ms | fingerprint match |"
            );
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
        }
    }
    let _ = writeln!(out);
}

fn benchmark_index_paths(document: &Document, iterations: usize) -> Vec<OperationRow> {
    let mut rows = Vec::new();
    rows.push(operation_row(
        "paragraph_byte_range all",
        iterations,
        || {
            let mut failures = 0;
            let mut total = 0usize;
            for ix in 0..document.paragraphs.len() {
                let range = paragraph_byte_range(document, ix);
                total = total.wrapping_add(range.start).wrapping_add(range.end);
                failures += usize::from(range.start > range.end);
            }
            std::hint::black_box(total);
            failures
        },
    ));
    rows.push(operation_row(
        "global_to_document_offset all paragraph starts",
        iterations,
        || {
            let mut failures = 0;
            for ix in 0..document.paragraphs.len() {
                let range = paragraph_byte_range(document, ix);
                let offset = global_to_document_offset(document, range.start);
                failures += usize::from(offset.paragraph != ix || offset.byte != 0);
            }
            failures
        },
    ));
    rows.push(operation_row(
        "document_position_for_offset all paragraph ends",
        iterations,
        || {
            let mut failures = 0;
            for (ix, paragraph) in document.paragraphs.iter().enumerate() {
                let offset = DocumentOffset {
                    paragraph: ix,
                    byte: paragraph_text_len(paragraph),
                };
                failures += usize::from(document_position_for_offset(document, offset).is_none());
            }
            failures
        },
    ));
    rows.push(operation_row(
        "block_ix_for_paragraph scan all",
        iterations,
        || {
            let mut failures = 0;
            for ix in 0..document.paragraphs.len() {
                failures += usize::from(block_ix_for_paragraph(document, ix).is_none());
            }
            failures
        },
    ));
    rows.push(operation_row(
        "VisibilityIndex::build visible",
        iterations,
        || {
            let visibility = VisibilityIndex::build(document, false);
            let mut visible = 0usize;
            for ix in 0..document.blocks.len() {
                visible += usize::from(visibility.is_visible(ix));
            }
            std::hint::black_box(visible);
            0
        },
    ));
    rows.push(operation_row(
        "VisibilityIndex::build invisibility",
        iterations,
        || {
            let visibility = VisibilityIndex::build(document, true);
            let mut visible = 0usize;
            for ix in 0..document.blocks.len() {
                visible += usize::from(visibility.is_visible(ix));
            }
            std::hint::black_box(visible);
            0
        },
    ));
    rows
}

fn benchmark_edit_paths(
    document: &Document,
    stats: &DocumentStats,
    iterations: usize,
) -> Vec<OperationRow> {
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
    rows.push(operation_row(
        "find_text_ranges \"the\"",
        iterations,
        || {
            let ranges = find_text_ranges(document, "the");
            std::hint::black_box(ranges.len());
            0
        },
    ));
    rows.push(operation_row(
        "selected_plain_text first window",
        iterations,
        || {
            let range = first_window_range(document, 24);
            let text = selected_plain_text(document, range);
            std::hint::black_box(text.len());
            0
        },
    ));
    rows.push(operation_row(
        "selected_rich_fragment first window",
        iterations,
        || {
            let range = first_window_range(document, 24);
            let fragment = selected_rich_fragment(document, range);
            std::hint::black_box(fragment.paragraphs.len());
            0
        },
    ));
    rows.push(operation_row(
        "merge_adjacent_runs all runs",
        iterations,
        || {
            let runs = document
                .paragraphs
                .iter()
                .flat_map(|paragraph| paragraph.runs.iter().cloned())
                .collect::<Vec<_>>();
            let merged = merge_adjacent_runs(runs);
            std::hint::black_box(merged.len());
            0
        },
    ));
    rows.push(operation_row(
        "insert_text_at largest paragraph midpoint",
        iterations,
        || {
            let mut clone = document.clone();
            insert_text_at(&mut clone, largest, largest_mid, "x", RunStyles::default());
            check_document_fidelity(&clone).failures.len()
        },
    ));
    rows.push(operation_row(
        "delete_range_in_paragraph first char",
        iterations,
        || {
            let mut clone = document.clone();
            if let Some(range) = largest_first_char.clone() {
                delete_range_in_paragraph(&mut clone, largest, range);
            }
            check_document_fidelity(&clone).failures.len()
        },
    ));
    rows.push(operation_row(
        "apply_style_to_paragraph_range fragmented paragraph",
        iterations,
        || {
            let mut clone = document.clone();
            let end = paragraph_text_len(&clone.paragraphs[fragmented])
                .min(safe_mid_byte(&clone, fragmented).max(1));
            if end > 0 {
                apply_style_to_paragraph_range(
                    &mut clone,
                    fragmented,
                    0..end,
                    RunStyle::HighlightSpoken,
                );
            }
            check_document_fidelity(&clone).failures.len()
        },
    ));
    rows.push(operation_row(
        "split_paragraph_at largest midpoint",
        iterations,
        || {
            let mut clone = document.clone();
            if paragraph_text_len(&clone.paragraphs[largest]) > 0 {
                split_paragraph_at(&mut clone, largest, largest_mid);
            }
            check_document_fidelity(&clone).failures.len()
        },
    ));
    rows.push(operation_row(
        "delete_cross_paragraph_range first window",
        iterations,
        || {
            let mut clone = document.clone();
            if clone.paragraphs.len() > 1 {
                let end_paragraph = (clone.paragraphs.len() - 1).min(10);
                let end_byte = paragraph_text_len(&clone.paragraphs[end_paragraph])
                    .min(safe_mid_byte(&clone, end_paragraph).max(1));
                delete_cross_paragraph_range(
                    &mut clone,
                    DocumentOffset {
                        paragraph: 0,
                        byte: 0,
                    }..DocumentOffset {
                        paragraph: end_paragraph,
                        byte: end_byte,
                    },
                );
            }
            check_document_fidelity(&clone).failures.len()
        },
    ));
    rows.push(operation_row(
        "insert_rich_fragment_at first window",
        iterations,
        || {
            let fragment = selected_rich_fragment(document, first_window_range(document, 8));
            let mut clone = document.clone();
            insert_rich_fragment_at(&mut clone, DocumentOffset::default(), &fragment);
            check_document_fidelity(&clone).failures.len()
        },
    ));

    rows
}

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

fn benchmark_layout_paths(
    document: &Document,
    widths: &[f32],
    iterations: usize,
    include_paint: bool,
    window: &mut Window,
    cx: &mut Context<BenchmarkRunner>,
) -> Vec<LayoutBenchRow> {
    let mut rows = Vec::new();
    for width in widths {
        let width_px = px(*width);
        let estimate_all = repeated(iterations, || {
            let mut total = 0.0f32;
            for paragraph_ix in 0..document.paragraphs.len() {
                let height: f32 =
                    estimate_paragraph_item_height(document, paragraph_ix, width_px).into();
                total += height;
            }
            std::hint::black_box(total);
        });
        let visibility_visible = repeated(iterations, || {
            std::hint::black_box(VisibilityIndex::build(document, false));
        });
        let visibility_invisible = repeated(iterations, || {
            std::hint::black_box(VisibilityIndex::build(document, true));
        });

        let mut full_layout = None;
        let full_layout_duration = repeated(iterations, || {
            full_layout = Some(build_layout(document, width_px, None, window, cx));
        });
        let layout = full_layout.expect("full layout benchmark should produce a layout");
        let reuse_layout_duration = repeated(iterations, || {
            std::hint::black_box(build_layout(document, width_px, Some(&layout), window, cx));
        });
        let structural_layout = repeated(iterations, || {
            std::hint::black_box(build_structural_block_layout(
                document, width_px, None, window, cx,
            ));
        });

        let mut paint_layout_state = layout.clone();
        paint_layout_state.bounds = Some(Bounds::new(
            point(px(0.0), px(0.0)),
            size(width_px, layout.size.height),
        ));
        let paint_plain = include_paint.then(|| {
            repeated(iterations, || {
                paint_layout(&paint_layout_state, None, None, false, px(1.0), window, cx);
            })
        });
        let selection = top_selection(document);
        let paint_selected = include_paint.then(|| {
            repeated(iterations, || {
                paint_layout(
                    &paint_layout_state,
                    selection.as_ref(),
                    None,
                    false,
                    px(1.0),
                    window,
                    cx,
                );
            })
        });

        let editor = cx.new(|cx| RichTextEditor::new_with_path(document.clone(), None, cx));
        let item_sizes_cold = editor.update(cx, |editor, cx| {
            editor.benchmark_invalidate_document_layout_caches();
            editor.benchmark_paragraph_item_sizes(width_px, window, cx)
        });
        let item_sizes_hot = editor.update(cx, |editor, cx| {
            editor.benchmark_paragraph_item_sizes(width_px, window, cx)
        });
        let item_sizes_invisible = editor.update(cx, |editor, cx| {
            editor.set_invisibility_mode(true, cx);
            editor.benchmark_paragraph_item_sizes(width_px, window, cx)
        });

        let (estimate_mean_abs_error, estimate_max_abs_error) =
            estimate_error(document, &layout, width_px);
        let summary = summarize_layout(document, &layout);
        rows.push(LayoutBenchRow {
            width: *width,
            estimate_all,
            visibility_visible,
            visibility_invisible,
            full_layout: full_layout_duration,
            reuse_layout: reuse_layout_duration,
            structural_layout,
            paint_plain,
            paint_selected,
            item_sizes_cold,
            item_sizes_hot,
            item_sizes_invisible,
            estimate_mean_abs_error,
            estimate_max_abs_error,
            summary,
        });
    }
    rows
}

fn benchmark_sample_paragraph_layouts(
    document: &Document,
    stats: &DocumentStats,
    widths: &[f32],
    iterations: usize,
    window: &mut Window,
    cx: &mut Context<BenchmarkRunner>,
) -> Vec<ParagraphLayoutRow> {
    let mut rows = Vec::new();
    let mut samples = vec![
        ("first".to_string(), 0usize),
        ("middle".to_string(), document.paragraphs.len() / 2),
        (
            "last".to_string(),
            document.paragraphs.len().saturating_sub(1),
        ),
        ("largest".to_string(), stats.largest_paragraph_ix),
        ("most_runs".to_string(), stats.most_runs_paragraph_ix),
    ];
    samples.sort_by_key(|(_, ix)| *ix);
    samples.dedup_by_key(|(_, ix)| *ix);

    for width in widths {
        let width_px = px(*width);
        for (label, paragraph_ix) in &samples {
            let paragraph_ix = (*paragraph_ix).min(document.paragraphs.len().saturating_sub(1));
            let mut normal_layout = None;
            let normal = repeated(iterations, || {
                normal_layout = Some(build_single_paragraph_layout_with_visibility(
                    document,
                    paragraph_ix,
                    width_px,
                    None,
                    false,
                    window,
                    cx,
                ));
            });
            let mut invisible_layout = None;
            let invisible = repeated(iterations, || {
                invisible_layout = Some(build_single_paragraph_layout_with_visibility(
                    document,
                    paragraph_ix,
                    width_px,
                    None,
                    true,
                    window,
                    cx,
                ));
            });
            let normal_layout =
                normal_layout.expect("single paragraph layout benchmark should produce a layout");
            let invisible_layout = invisible_layout
                .expect("single paragraph invisible layout benchmark should produce a layout");
            let summary = summarize_layout(document, &normal_layout);
            rows.push(ParagraphLayoutRow {
                label: label.clone(),
                paragraph_ix,
                width: *width,
                normal,
                invisible,
                lines: summary.lines,
                segments: summary.segments,
                normal_height: px_to_f32(normal_layout.size.height),
                invisible_height: px_to_f32(invisible_layout.size.height),
            });
        }
    }

    rows
}

fn repeated(iterations: usize, mut run: impl FnMut()) -> DurationStats {
    let mut timings = Vec::with_capacity(iterations.max(1));
    for _ in 0..iterations.max(1) {
        let started = Instant::now();
        run();
        timings.push(started.elapsed());
    }
    DurationStats::from_samples(&timings)
}

fn estimate_error(document: &Document, layout: &LayoutState, width: Pixels) -> (f32, f32) {
    let mut total = 0.0f32;
    let mut max = 0.0f32;
    let mut count = 0usize;
    for (layout_ix, paragraph) in layout.paragraphs.iter().enumerate() {
        let exact = if let Some(next) = layout.paragraphs.get(layout_ix + 1) {
            px_to_f32(next.top - paragraph.top)
        } else {
            px_to_f32(layout.size.height - paragraph.top)
        };
        let estimate = px_to_f32(estimate_paragraph_item_height(
            document,
            paragraph.index,
            width,
        ));
        let error = (estimate - exact).abs();
        total += error;
        max = max.max(error);
        count += 1;
    }
    if count == 0 {
        (0.0, 0.0)
    } else {
        (total / count as f32, max)
    }
}

fn summarize_layout(document: &Document, layout: &LayoutState) -> LayoutSummary {
    let mut summary = LayoutSummary {
        layout_height: px_to_f32(layout.size.height),
        ..Default::default()
    };
    for paragraph in &layout.paragraphs {
        if paragraph.index >= document.paragraphs.len() {
            summary.fidelity_failures += 1;
        }
        let mut previous_bottom = px(-1.0);
        for line in &paragraph.lines {
            summary.lines += 1;
            summary.segments += line.segments.len();
            summary.rects += line.rects.len();
            summary.underlines += line.underlines.len();
            summary.strikethroughs += line.strikethroughs.len();
            summary.max_line_width = summary.max_line_width.max(px_to_f32(line.width));
            if line.start_byte > line.end_byte
                || line.end_byte > paragraph.len
                || line.origin.y < previous_bottom
            {
                summary.fidelity_failures += 1;
            }
            previous_bottom = line.origin.y + line.line_height;
            for segment in &line.segments {
                if segment.start_byte > line.end_byte {
                    summary.fidelity_failures += 1;
                }
            }
        }
    }
    summary
}

fn write_operation_table(out: &mut String, title: &str, rows: &[OperationRow]) {
    let _ = writeln!(out, "### {title}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| benchmark | min ms | mean ms | max ms | samples | fidelity failures |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|");
    for row in rows {
        let _ = writeln!(
            out,
            "| {} | {:.3} | {:.3} | {:.3} | {} | {} |",
            md(&row.name),
            ms(row.duration.min),
            ms(row.duration.mean),
            ms(row.duration.max),
            row.duration.samples,
            row.fidelity_failures
        );
    }
    let _ = writeln!(out);
}

fn write_layout_table(out: &mut String, rows: &[LayoutBenchRow]) {
    let _ = writeln!(out, "### Layout, Paint, And Virtual List Benchmarks");
    let _ = writeln!(out);
    let _ = writeln!(
    out,
    "| width | estimate all mean ms | full layout mean ms | reused layout mean ms | structural mean ms | paint mean ms | selected paint mean ms | item sizes cold/hot/invis ms | lines | segments | height | estimate mean/max abs error px | fidelity failures |"
  );
    let _ = writeln!(
        out,
        "|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|"
    );
    for row in rows {
        let paint = row
            .paint_plain
            .map(|stats| format!("{:.3}", ms(stats.mean)))
            .unwrap_or_else(|| "n/a".to_string());
        let selected_paint = row
            .paint_selected
            .map(|stats| format!("{:.3}", ms(stats.mean)))
            .unwrap_or_else(|| "n/a".to_string());
        let _ = writeln!(
      out,
      "| {:.0} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} | {:.3}/{:.3}/{:.3} | {} | {} | {:.1} | {:.1}/{:.1} | {} |",
      row.width,
      ms(row.estimate_all.mean),
      ms(row.full_layout.mean),
      ms(row.reuse_layout.mean),
      ms(row.structural_layout.mean),
      paint,
      selected_paint,
      ms(row.item_sizes_cold.elapsed),
      ms(row.item_sizes_hot.elapsed),
      ms(row.item_sizes_invisible.elapsed),
      row.summary.lines,
      row.summary.segments,
      row.summary.layout_height,
      row.estimate_mean_abs_error,
      row.estimate_max_abs_error,
      row.summary.fidelity_failures
    );
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Item size cache detail:");
    let _ = writeln!(out);
    let _ = writeln!(
    out,
    "| width | cold hit | hot hit | invis hit | items | exact heights cold/hot/invis | total height cold/hot/invis | visibility visible/invisible mean ms |"
  );
    let _ = writeln!(out, "|---:|---|---|---|---:|---|---|---:|");
    for row in rows {
        let _ = writeln!(
            out,
            "| {:.0} | {} | {} | {} | {} | {}/{}/{} | {:.1}/{:.1}/{:.1} | {:.3}/{:.3} |",
            row.width,
            row.item_sizes_cold.cache_hit,
            row.item_sizes_hot.cache_hit,
            row.item_sizes_invisible.cache_hit,
            row.item_sizes_cold.item_count,
            row.item_sizes_cold.exact_height_count,
            row.item_sizes_hot.exact_height_count,
            row.item_sizes_invisible.exact_height_count,
            row.item_sizes_cold.total_height,
            row.item_sizes_hot.total_height,
            row.item_sizes_invisible.total_height,
            ms(row.visibility_visible.mean),
            ms(row.visibility_invisible.mean)
        );
    }
    let _ = writeln!(out);
}

fn write_paragraph_layout_table(out: &mut String, rows: &[ParagraphLayoutRow]) {
    let _ = writeln!(out, "### Sample Paragraph Layout Benchmarks");
    let _ = writeln!(out);
    let _ = writeln!(
    out,
    "| sample | paragraph | width | normal mean ms | invisibility mean ms | normal height | invisibility height | lines | segments |"
  );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for row in rows {
        let _ = writeln!(
            out,
            "| {} | {} | {:.0} | {:.3} | {:.3} | {:.1} | {:.1} | {} | {} |",
            md(&row.label),
            row.paragraph_ix,
            row.width,
            ms(row.normal.mean),
            ms(row.invisible.mean),
            row.normal_height,
            row.invisible_height,
            row.lines,
            row.segments
        );
    }
    let _ = writeln!(out);
}

fn first_window_range(document: &Document, paragraph_count: usize) -> Range<DocumentOffset> {
    let end_paragraph = document
        .paragraphs
        .len()
        .saturating_sub(1)
        .min(paragraph_count.saturating_sub(1));
    DocumentOffset {
        paragraph: 0,
        byte: 0,
    }..DocumentOffset {
        paragraph: end_paragraph,
        byte: paragraph_text_len(&document.paragraphs[end_paragraph]),
    }
}

fn top_selection(document: &Document) -> Option<EditorSelection> {
    if document.paragraphs.is_empty() {
        return None;
    }
    Some(EditorSelection {
        anchor: DocumentOffset {
            paragraph: 0,
            byte: 0,
        },
        head: first_window_range(document, 3).end,
    })
}

fn first_char_range(document: &Document, paragraph_ix: usize) -> Option<Range<usize>> {
    let text = paragraph_text(document, paragraph_ix);
    let ch = text.chars().next()?;
    Some(0..ch.len_utf8())
}

fn safe_mid_byte(document: &Document, paragraph_ix: usize) -> usize {
    let text = paragraph_text(document, paragraph_ix);
    if text.is_empty() {
        return 0;
    }
    let target = text.len() / 2;
    text.char_indices()
        .map(|(ix, _)| ix)
        .take_while(|ix| *ix <= target)
        .last()
        .unwrap_or(0)
}

fn fingerprint_document(document: &Document) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for chunk in document.text.chunks() {
        chunk.hash(&mut hasher);
    }
    document.paragraphs.len().hash(&mut hasher);
    for paragraph in document.paragraphs.iter() {
        paragraph.style.hash(&mut hasher);
        paragraph.byte_range.hash(&mut hasher);
        paragraph.runs.hash(&mut hasher);
    }
    document.blocks.len().hash(&mut hasher);
    for block in document.blocks.iter() {
        hash_block(block, &mut hasher);
    }
    let mut assets = document.assets.assets.values().collect::<Vec<_>>();
    assets.sort_by_key(|asset| asset.id.0);
    for asset in assets {
        asset.id.hash(&mut hasher);
        asset.mime_type.as_ref().hash(&mut hasher);
        asset
            .original_name
            .as_ref()
            .map(|name| name.as_ref())
            .hash(&mut hasher);
        asset.content_hash.hash(&mut hasher);
        asset.bytes.len().hash(&mut hasher);
        asset.bytes.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_block(block: &Block, hasher: &mut impl Hasher) {
    match block {
        Block::Paragraph(paragraph) => {
            0u8.hash(hasher);
            hash_paragraph(paragraph, hasher);
        }
        Block::Image(image) => {
            1u8.hash(hasher);
            image.asset_id.hash(hasher);
            image.alt_text.as_ref().hash(hasher);
            hash_optional_paragraph(image.caption.as_ref(), hasher);
            hash_image_sizing(&image.sizing, hasher);
            hash_block_alignment(image.alignment, hasher);
            image.version.hash(hasher);
        }
        Block::Equation(equation) => {
            2u8.hash(hasher);
            equation.source.as_ref().hash(hasher);
            hash_equation_syntax(equation.syntax, hasher);
            hash_equation_display(equation.display, hasher);
            equation.version.hash(hasher);
        }
        Block::Table(table) => {
            3u8.hash(hasher);
            hash_table(table, hasher);
        }
    }
}

fn hash_optional_paragraph(paragraph: Option<&Paragraph>, hasher: &mut impl Hasher) {
    match paragraph {
        Some(paragraph) => {
            true.hash(hasher);
            hash_paragraph(paragraph, hasher);
        }
        None => false.hash(hasher),
    }
}

fn hash_paragraph(paragraph: &Paragraph, hasher: &mut impl Hasher) {
    paragraph.style.hash(hasher);
    paragraph.byte_range.hash(hasher);
    paragraph.runs.hash(hasher);
    paragraph.version.hash(hasher);
}

fn hash_table(table: &TableBlock, hasher: &mut impl Hasher) {
    table.version.hash(hasher);
    table.style.header_row.hash(hasher);
    table.column_widths.len().hash(hasher);
    for width in &table.column_widths {
        match width {
            TableColumnWidth::Auto => 0u8.hash(hasher),
            TableColumnWidth::FixedPx(value) => {
                1u8.hash(hasher);
                value.hash(hasher);
            }
            TableColumnWidth::Fraction(value) => {
                2u8.hash(hasher);
                value.hash(hasher);
            }
        }
    }
    table.rows.len().hash(hasher);
    for row in &table.rows {
        row.cells.len().hash(hasher);
        for cell in &row.cells {
            cell.row_span.hash(hasher);
            cell.col_span.hash(hasher);
            for block in &cell.blocks {
                match block {
                    TableCellBlock::Paragraph(paragraph) => {
                        0u8.hash(hasher);
                        hash_paragraph(&paragraph.paragraph, hasher);
                        paragraph.text.hash(hasher);
                    }
                    TableCellBlock::Table(table) => {
                        1u8.hash(hasher);
                        hash_table(table, hasher);
                    }
                }
            }
        }
    }
}

fn hash_image_sizing(sizing: &ImageSizing, hasher: &mut impl Hasher) {
    match sizing {
        ImageSizing::Intrinsic => 0u8.hash(hasher),
        ImageSizing::FitWidth => 1u8.hash(hasher),
        ImageSizing::Fixed {
            width_px,
            height_px,
        } => {
            2u8.hash(hasher);
            width_px.hash(hasher);
            height_px.hash(hasher);
        }
    }
}

fn hash_block_alignment(alignment: BlockAlignment, hasher: &mut impl Hasher) {
    match alignment {
        BlockAlignment::Left => 0u8.hash(hasher),
        BlockAlignment::Center => 1u8.hash(hasher),
        BlockAlignment::Right => 2u8.hash(hasher),
    }
}

fn hash_equation_syntax(syntax: EquationSyntax, hasher: &mut impl Hasher) {
    match syntax {
        EquationSyntax::Latex => 0u8.hash(hasher),
    }
}

fn hash_equation_display(display: EquationDisplay, hasher: &mut impl Hasher) {
    match display {
        EquationDisplay::Display => 0u8.hash(hasher),
        EquationDisplay::InlineLikeParagraph => 1u8.hash(hasher),
    }
}

fn paragraph_style_names() -> [(ParagraphStyle, &'static str); 7] {
    [
        (ParagraphStyle::Pocket, "Pocket"),
        (ParagraphStyle::Hat, "Hat"),
        (ParagraphStyle::Block, "Block"),
        (ParagraphStyle::Tag, "Tag"),
        (ParagraphStyle::Analytic, "Analytic"),
        (ParagraphStyle::Normal, "Normal"),
        (ParagraphStyle::Undertag, "Undertag"),
    ]
}

fn semantic_style_names() -> [(RunSemanticStyle, &'static str); 6] {
    [
        (RunSemanticStyle::Plain, "Plain"),
        (RunSemanticStyle::Cite, "Cite"),
        (RunSemanticStyle::Emphasis, "Emphasis"),
        (RunSemanticStyle::Underline, "Underline"),
        (RunSemanticStyle::Condensed, "Condensed"),
        (RunSemanticStyle::Ultracondensed, "Ultracondensed"),
    ]
}

fn highlight_style_names() -> [(Option<HighlightStyle>, &'static str); 4] {
    [
        (None, "None"),
        (Some(HighlightStyle::Spoken), "Spoken"),
        (Some(HighlightStyle::Insert), "Insert"),
        (Some(HighlightStyle::Alternative), "Alternative"),
    ]
}

fn div_duration(duration: Duration, divisor: u32) -> Duration {
    if divisor == 0 {
        Duration::default()
    } else {
        Duration::from_secs_f64(duration.as_secs_f64() / divisor as f64)
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn px_to_f32(pixels: Pixels) -> f32 {
    let value: f32 = pixels.into();
    value
}

fn md(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
