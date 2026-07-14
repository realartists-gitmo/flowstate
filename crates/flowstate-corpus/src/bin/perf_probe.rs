//! Perf probe: isolate WHERE the time goes when opening a large real `.docx`.
//!
//! Usage: `flowstate-perf-probe <FILE.docx> [ITERS]` (default 5 iters).
//!
//! Phases, each timed:
//!   IMPORT       — `.docx` bytes -> Loro doc (the interpreter).
//!   PROJECT-warm — `document_from_loro` on the just-imported (state already decoded) doc.
//!   SNAPSHOT     — export a Loro snapshot (what a `.db8`/reopen carries).
//!   PROJECT-cold — import the snapshot into a FRESH doc (lazy/undecoded richtext, the
//!                  real reopen/receiving-peer condition) and project it. This is the
//!                  hypothesis-A path where `query_text_id_positions` decodes the whole
//!                  body per projection.
//!   COLD->WARM   — on ONE fresh import, project twice: does the decode PERSIST (2nd
//!                  cheap) or re-decode every projection (2nd still expensive)?

use std::time::Instant;

fn main() {
  let mut args = std::env::args().skip(1);
  let Some(path) = args
    .next()
    .or_else(|| std::env::var("FLOWSTATE_BENCH_FIXTURE").ok())
  else {
    eprintln!("usage: flowstate-perf-probe <FILE.docx> [ITERS]   (or set FLOWSTATE_BENCH_FIXTURE)");
    std::process::exit(2);
  };
  let iters: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(5);

  let bytes = match std::fs::read(&path) {
    Ok(bytes) => bytes,
    Err(error) => {
      eprintln!("read {path}: {error}");
      std::process::exit(1);
    },
  };
  println!("file: {path}\nsize: {:.2} MB   iters: {iters}\n", bytes.len() as f64 / 1_048_576.0);

  // IMPORT (once; keep the decoded doc for the warm + snapshot phases).
  let t = Instant::now();
  let (imported, report) = match flowstate_docx::import_docx_to_loro(&path, "perf-probe") {
    Ok(pair) => pair,
    Err(error) => {
      eprintln!("import failed: {error}");
      std::process::exit(1);
    },
  };
  let import_ms = t.elapsed().as_secs_f64() * 1000.0;
  println!(
    "IMPORT          {import_ms:8.1} ms   (paragraphs_imported={})",
    report.paragraphs_imported
  );

  // PROJECT-warm: the imported doc's richtext state is already materialized.
  let warm = time_median(iters, || {
    let _ = flowstate_document::document_from_loro(&imported.doc).expect("project");
  });
  report_phase("PROJECT-warm", &warm);

  // SNAPSHOT
  let t = Instant::now();
  let snapshot = imported
    .doc
    .export(loro::ExportMode::Snapshot)
    .expect("snapshot");
  println!(
    "SNAPSHOT        {:8.1} ms   ({:.2} MB)",
    t.elapsed().as_secs_f64() * 1000.0,
    snapshot.len() as f64 / 1_048_576.0
  );

  // PROJECT-cold: fresh (undecoded) import each iter — the reopen / receiving-peer path.
  let cold = time_median(iters, || {
    let doc = loro::LoroDoc::new();
    doc.import(&snapshot).expect("reimport");
    let _ = flowstate_document::document_from_loro(&doc).expect("project cold");
  });
  report_phase("PROJECT-cold", &cold);

  // COLD->WARM on a single fresh instance: does the first projection's decode persist?
  let cold_warm_doc = loro::LoroDoc::new();
  cold_warm_doc.import(&snapshot).expect("reimport");
  let t = Instant::now();
  let _ = flowstate_document::document_from_loro(&cold_warm_doc).expect("project");
  let first = t.elapsed().as_secs_f64() * 1000.0;
  let t = Instant::now();
  let _ = flowstate_document::document_from_loro(&cold_warm_doc).expect("project");
  let second = t.elapsed().as_secs_f64() * 1000.0;
  println!("COLD->WARM      first={first:.1} ms  second={second:.1} ms   (second<<first => decode persists; else re-decodes every projection)");

  println!(
    "\nsummary: import {import_ms:.0}ms | cold-project {:.0}ms | warm-project {:.0}ms | cold/warm ratio {:.1}x",
    cold.median,
    warm.median,
    if warm.median > 0.0 { cold.median / warm.median } else { 0.0 },
  );
}

struct Stats {
  min: f64,
  median: f64,
  max: f64,
}

fn time_median(iters: usize, mut run: impl FnMut()) -> Stats {
  let mut samples: Vec<f64> = Vec::with_capacity(iters.max(1));
  for _ in 0..iters.max(1) {
    let t = Instant::now();
    run();
    samples.push(t.elapsed().as_secs_f64() * 1000.0);
  }
  samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
  Stats {
    min: samples[0],
    median: samples[samples.len() / 2],
    max: samples[samples.len() - 1],
  }
}

fn report_phase(name: &str, stats: &Stats) {
  println!("{name:<15} {:8.1} ms   (min {:.1}, max {:.1})", stats.median, stats.min, stats.max);
}
