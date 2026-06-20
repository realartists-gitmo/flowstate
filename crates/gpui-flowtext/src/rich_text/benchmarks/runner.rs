use std::{
  collections::HashMap,
  fmt::Write as _,
  fs,
  hash::{Hash, Hasher},
  ops::Range,
  path::PathBuf,
  time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{Bounds, Context, IntoElement, Pixels, Render, Window, div, point, prelude::*, px, size};
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

#[hotpath::measure_all]
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

#[hotpath::measure_all]
impl BenchmarkRunner {
  pub fn new(options: BenchmarkOptions) -> Self {
    Self {
      options,
      state: BenchmarkState::Queued,
    }
  }
}

#[hotpath::measure_all]
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

#[hotpath::measure_all]
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
        println!("benchmark report written to {}", self.options.output_path.display());
        println!("{report}");
        self.state = BenchmarkState::Complete {
          output_path: self.options.output_path.clone(),
        };
      },
      Err(error) => {
        let message = format!("failed to write benchmark report to {}: {error}", self.options.output_path.display());
        eprintln!("{message}");
        println!("{report}");
        self.state = BenchmarkState::Failed { message };
      },
    }
    cx.notify();
    window.on_next_frame(|_, cx| cx.quit());
  }

  fn status_text(&self) -> String {
    match &self.state {
      BenchmarkState::Queued => "Benchmark mode queued.".to_string(),
      BenchmarkState::Starting => "Benchmark mode starting. The suite will begin after this window paints.".to_string(),
      BenchmarkState::Running => format!(
        "Benchmark mode running. This window may stop responding while GPUI layout and paint paths are measured.\nReport target: {}",
        self.options.output_path.display()
      ),
      BenchmarkState::Complete { output_path } => {
        format!("Benchmark complete.\nReport written to: {}", output_path.display())
      },
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
  document: DocumentProjection,
  load: DurationStats,
}

#[derive(Clone, Copy, Debug)]
struct DurationStats {
  min: Duration,
  mean: Duration,
  max: Duration,
  samples: usize,
}

#[hotpath::measure_all]
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

#[hotpath::measure_all]
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

