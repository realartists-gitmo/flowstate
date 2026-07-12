//! Corpus import-fidelity sweep (workplace policy: validate imports over the whole
//! real debate `.docx` corpus, not just synthetic fixtures).
//!
//! Walks a corpus directory (default `$FLOWSTATE_CORPUS_DIR` or the repo-local
//! `helpers/corpus/dropbox-office-flat`),
//! DYNAMICALLY discovering every `.docx` at run time, and checks each through the real
//! import path:
//!   1. `import_docx_bytes_to_loro` — the file opens without error or panic,
//!   2. `document_from_loro_with_defects` — the projection derives with ZERO defects
//!      (a defect means the importer left canonical state a repair pass must fix),
//!   3. optional `--roundtrip` — export the projection back to `.docx`, re-import, and
//!      assert the text survives (catches export/reimport data loss).
//!
//! An incremental LEDGER (keyed by path + size + mtime) means re-runs — and `--watch`
//! (periodic rescan) — only process files that are NEW or CHANGED since last time, so
//! as files are added to the corpus, the new ones get checked
//! automatically. Failures are reported LOUDLY with their full path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const DEFAULT_CORPUS_RELATIVE_TO_WORKSPACE: &str = "helpers/corpus/dropbox-office-flat";

/// One ledger entry: the file fingerprint plus the last checked outcome.
#[derive(Clone, Serialize, Deserialize)]
struct Record {
  size: u64,
  mtime_ns: u128,
  ok: bool,
  /// Failure detail (empty when `ok`).
  detail: String,
  checked_at_unix: u64,
  /// Wall-clock milliseconds the import+project check took (perf meter).
  #[serde(default)]
  check_ms: u64,
}

/// path (as a string key) -> last result. A `BTreeMap` keeps the ledger file stable/diffable.
type Ledger = BTreeMap<String, Record>;

struct Config {
  corpus_dir: PathBuf,
  ledger_path: PathBuf,
  report_path: PathBuf,
  jobs: usize,
  roundtrip: bool,
  recheck: bool,
  watch: bool,
  watch_interval: Duration,
}

fn main() {
  // The importer is meant to be robust, but the whole point of this sweep is to find
  // the files where it ISN'T — so swallow the default panic output; per-file
  // `catch_unwind` turns a panic into a recorded failure instead of aborting the run.
  std::panic::set_hook(Box::new(|_| {}));

  let config = match parse_config() {
    Ok(config) => config,
    Err(message) => {
      eprintln!("{message}");
      std::process::exit(2);
    }
  };

  if let Err(error) = run(&config) {
    eprintln!("corpus sweep failed: {error:#}");
    std::process::exit(1);
  }
}

fn parse_config() -> Result<Config, String> {
  let mut corpus_dir: Option<PathBuf> = None;
  let mut jobs: Option<usize> = None;
  let mut roundtrip = false;
  let mut recheck = false;
  let mut watch = false;
  let mut watch_interval = Duration::from_secs(30);
  let mut ledger_path: Option<PathBuf> = None;
  let mut report_path: Option<PathBuf> = None;

  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--roundtrip" => roundtrip = true,
      "--recheck" => recheck = true,
      "--watch" => watch = true,
      "--jobs" => {
        jobs = Some(args.next().and_then(|v| v.parse().ok()).ok_or("--jobs needs a number")?);
      }
      "--interval" => {
        let secs: u64 = args.next().and_then(|v| v.parse().ok()).ok_or("--interval needs seconds")?;
        watch_interval = Duration::from_secs(secs.max(1));
      }
      "--ledger" => ledger_path = Some(PathBuf::from(args.next().ok_or("--ledger needs a path")?)),
      "--report" => report_path = Some(PathBuf::from(args.next().ok_or("--report needs a path")?)),
      "-h" | "--help" => return Err(usage()),
      other if other.starts_with('-') => return Err(format!("unknown flag {other}\n{}", usage())),
      other => corpus_dir = Some(PathBuf::from(other)),
    }
  }

  let corpus_dir = corpus_dir
    .or_else(|| std::env::var_os("FLOWSTATE_CORPUS_DIR").map(PathBuf::from))
    .unwrap_or_else(default_corpus_dir);
  if !corpus_dir.is_dir() {
    return Err(format!("corpus dir does not exist: {}", corpus_dir.display()));
  }

  let state_dir = default_state_dir();
  Ok(Config {
    corpus_dir,
    ledger_path: ledger_path.unwrap_or_else(|| state_dir.join("ledger.json")),
    report_path: report_path.unwrap_or_else(|| state_dir.join("failures.txt")),
    jobs: jobs.unwrap_or_else(default_jobs),
    roundtrip,
    recheck,
    watch,
    watch_interval,
  })
}

fn usage() -> String {
  "usage: flowstate-corpus [CORPUS_DIR] [--watch] [--roundtrip] [--recheck] \
   [--jobs N] [--interval SECS] [--ledger PATH] [--report PATH]\n  \
   CORPUS_DIR defaults to $FLOWSTATE_CORPUS_DIR or repo-local helpers/corpus/dropbox-office-flat"
    .to_string()
}

fn default_corpus_dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .join(DEFAULT_CORPUS_RELATIVE_TO_WORKSPACE)
}

fn default_state_dir() -> PathBuf {
  let base = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
  base.join(".flowstate-corpus")
}

fn default_jobs() -> usize {
  std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(1).max(1))
}

fn run(config: &Config) -> anyhow::Result<()> {
  if let Some(parent) = config.ledger_path.parent() {
    std::fs::create_dir_all(parent).ok();
  }
  let mut ledger = load_ledger(&config.ledger_path);

  eprintln!(
    "corpus sweep: dir={} jobs={} roundtrip={} watch={} ledger={}",
    config.corpus_dir.display(),
    config.jobs,
    config.roundtrip,
    config.watch,
    config.ledger_path.display(),
  );

  sweep_once(config, &mut ledger)?;

  if config.watch {
    eprintln!("--watch: rescanning every {}s for new/changed files (Ctrl-C to stop)", config.watch_interval.as_secs());
    loop {
      std::thread::sleep(config.watch_interval);
      sweep_once(config, &mut ledger)?;
    }
  }
  Ok(())
}

/// One incremental pass: discover, check only new/changed files, merge + persist, report.
fn sweep_once(config: &Config, ledger: &mut Ledger) -> anyhow::Result<()> {
  let discovered = discover_docx(&config.corpus_dir);
  let to_check: Vec<Discovered> = discovered
    .iter()
    .filter(|file| config.recheck || is_stale(ledger, file))
    .cloned()
    .collect();

  if to_check.is_empty() {
    report(config, ledger, &discovered, 0);
    return Ok(());
  }

  eprintln!("discovered {} .docx | checking {} new/changed", discovered.len(), to_check.len());
  // Share the ledger with the workers so results are recorded AND periodically
  // flushed to disk mid-pass — a full sweep of the large case files takes many
  // minutes, so a killed run must not lose everything (and the next run resumes).
  let shared: Mutex<Ledger> = Mutex::new(std::mem::take(ledger));
  check_all(&to_check, config, &shared);
  *ledger = shared.into_inner().expect("ledger mutex");
  save_ledger(&config.ledger_path, ledger)?;
  report(config, ledger, &discovered, to_check.len());
  Ok(())
}

#[derive(Clone)]
struct Discovered {
  path: PathBuf,
  key: String,
  size: u64,
  mtime_ns: u128,
}

fn discover_docx(dir: &Path) -> Vec<Discovered> {
  let mut out = Vec::new();
  let walker = ignore::WalkBuilder::new(dir)
    .standard_filters(false) // do not skip hidden/gitignored — sweep everything...
    .hidden(false)
    .build();
  for entry in walker.flatten() {
    let path = entry.path();
    // ...except Dropbox's own bookkeeping dirs and macOS AppleDouble sidecar dirs.
    if path.components().any(|component| {
      let name = component.as_os_str().to_string_lossy();
      name == ".dropbox" || name == ".dropbox.cache" || name == "__MACOSX"
    }) {
      continue;
    }
    let is_docx = path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("docx"));
    if !is_docx {
      continue;
    }
    // Skip non-documents that only *look* like `.docx`: Word lock/temp files
    // (`~$name.docx`) and macOS AppleDouble resource-fork sidecars (`._name.docx`,
    // which are not zip archives). These are expected import failures, not bugs.
    if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("~$") || n.starts_with("._")) {
      continue;
    }
    let Ok(meta) = entry.metadata() else { continue };
    if !meta.is_file() {
      continue;
    }
    // Zero-byte files are empty shells or not-yet-synced placeholders — not real
    // documents. Skipping keeps them out of the failure count; once a placeholder
    // syncs real bytes its size changes and it is discovered and checked normally.
    if meta.len() == 0 {
      continue;
    }
    let mtime_ns = meta
      .modified()
      .ok()
      .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
      .map_or(0, |d| d.as_nanos());
    out.push(Discovered {
      key: path.to_string_lossy().into_owned(),
      path: path.to_path_buf(),
      size: meta.len(),
      mtime_ns,
    });
  }
  out
}

/// A file needs checking if we've never seen it, or its size/mtime changed since.
fn is_stale(ledger: &Ledger, file: &Discovered) -> bool {
  ledger.get(&file.key).is_none_or(|record| record.size != file.size || record.mtime_ns != file.mtime_ns)
}

/// §perf-heaven T8.20: a memory-BUDGET gate so `jobs` workers can't each hold a
/// large-doc import + full Dst state at once and OOM (the systemd cap killed
/// the full `--roundtrip` / `RICHTEXT_VERIFY` sweeps at jobs=8/15). Each file
/// acquires permits ≈ its estimated peak memory (docx bytes × a multiplier), so
/// large files serialize against the budget while small files still flow at full
/// `jobs`. A file bigger than the whole budget takes it all (runs alone) rather
/// than deadlocking. This lets the definitive full-corpus sweep run instead of
/// the `--jobs 3-4` workaround.
struct MemGate {
  available: Mutex<u64>,
  cond: Condvar,
  total: u64,
}

impl MemGate {
  fn new(total: u64) -> Self {
    Self { available: Mutex::new(total), cond: Condvar::new(), total }
  }

  /// Block until `want` (clamped to the whole budget) permits are free, take them.
  fn acquire(&self, want: u64) -> u64 {
    let want = want.min(self.total).max(1);
    let mut available = self.available.lock().expect("mem-gate mutex");
    while *available < want {
      available = self.cond.wait(available).expect("mem-gate condvar");
    }
    *available -= want;
    want
  }

  fn release(&self, amount: u64) {
    *self.available.lock().expect("mem-gate mutex") += amount;
    self.cond.notify_all();
  }
}

/// The in-flight memory budget: `FLOWSTATE_CORPUS_MEM_BUDGET_MB`, else ~55% of
/// system RAM (from /proc/meminfo), else a conservative 6 GiB fallback.
fn mem_budget_bytes() -> u64 {
  if let Some(mb) = std::env::var("FLOWSTATE_CORPUS_MEM_BUDGET_MB").ok().and_then(|value| value.parse::<u64>().ok()) {
    return mb.saturating_mul(1024 * 1024).max(256 * 1024 * 1024);
  }
  let system = std::fs::read_to_string("/proc/meminfo")
    .ok()
    .and_then(|meminfo| {
      meminfo.lines().find_map(|line| {
        line.strip_prefix("MemTotal:").and_then(|rest| rest.trim().strip_suffix(" kB")).and_then(|kb| kb.trim().parse::<u64>().ok())
      })
    })
    .map(|kb| kb.saturating_mul(1024));
  system.map_or(6 * 1024 * 1024 * 1024, |bytes| (bytes / 100).saturating_mul(55))
}

/// Estimated peak resident memory for importing a docx of `size` bytes. The
/// import + full-state build expands the compressed docx ~50–100×; use a
/// conservative multiplier so the gate errs toward under-subscription.
fn estimated_peak_bytes(size: u64) -> u64 {
  size.saturating_mul(90).max(64 * 1024 * 1024)
}

/// Check every file across `jobs` workers, recording each result into `shared`
/// (the ledger) and flushing to disk every `FLUSH_EVERY` files so progress survives.
fn check_all(files: &[Discovered], config: &Config, shared: &Mutex<Ledger>) {
  const FLUSH_EVERY: usize = 500;
  let next = AtomicUsize::new(0);
  let done = AtomicUsize::new(0);
  let total = files.len();
  let started = Instant::now();
  let gate = MemGate::new(mem_budget_bytes());

  std::thread::scope(|scope| {
    for _ in 0..config.jobs.max(1) {
      scope.spawn(|| loop {
        let index = next.fetch_add(1, Ordering::Relaxed);
        if index >= total {
          break;
        }
        let file = &files[index];
        let file_started = Instant::now();
        // Reserve this file's estimated peak memory before importing; release it
        // as soon as the check returns (RAII-free: explicit release below).
        let held = gate.acquire(estimated_peak_bytes(file.size));
        let outcome = check_one(&file.path, config.roundtrip);
        gate.release(held);
        let elapsed = file_started.elapsed();
        // Surface pathologically slow files (near-hangs) so they are diagnosable.
        if elapsed > Duration::from_secs(10) {
          eprintln!("  SLOW {:.1}s: {}", elapsed.as_secs_f64(), file.path.display());
        }
        let record = Record {
          size: file.size,
          mtime_ns: file.mtime_ns,
          ok: outcome.is_ok(),
          detail: outcome.err().unwrap_or_default(),
          checked_at_unix: unix_now(),
          #[allow(clippy::cast_possible_truncation, reason = "a single-file import is far under u64::MAX milliseconds")]
          check_ms: elapsed.as_millis() as u64,
        };
        // Insert under the lock (guard drops at the `;`), then bump the counter —
        // the atomic does not need the ledger lock held.
        shared.lock().expect("ledger mutex").insert(file.key.clone(), record);
        let count = done.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(250) {
          let rate = count as f64 / started.elapsed().as_secs_f64().max(0.001);
          eprintln!("  ... {count}/{total} ({rate:.0}/s)");
        }
        if count.is_multiple_of(FLUSH_EVERY) {
          // Serialize+write under the lock (brief, ~ms); resumability over speed.
          let ledger = shared.lock().expect("ledger mutex");
          save_ledger(&config.ledger_path, &ledger).ok();
        }
      });
    }
  });
}

/// Run the fidelity check on one file, catching panics from the importer.
fn check_one(path: &Path, roundtrip: bool) -> Result<(), String> {
  let bytes = std::fs::read(path).map_err(|error| format!("read: {error}"))?;
  match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_bytes(&bytes, roundtrip))) {
    Ok(result) => result,
    Err(payload) => Err(format!("PANIC: {}", panic_message(payload.as_ref()))),
  }
}

fn check_bytes(bytes: &[u8], roundtrip: bool) -> Result<(), String> {
  let (imported, report) =
    flowstate_docx::import_docx_bytes_to_loro(bytes, "corpus-sweep").map_err(|error| format!("import: {error}"))?;
  let (projection, defects) =
    flowstate_document::document_from_loro_with_defects(&imported.doc).map_err(|error| format!("project: {error}"))?;
  if !defects.is_empty() {
    return Err(format!(
      "{} projection defect(s) (paragraphs_imported={}); sample: {:?}",
      defects.len(),
      report.paragraphs_imported,
      defects.iter().take(3).collect::<Vec<_>>(),
    ));
  }
  // Total content loss: the importer reported paragraphs but the projection is
  // ENTIRELY blank — no text AND no blocks. (An image/table-only doc has empty text
  // but non-empty blocks, so it is NOT flagged — that was the earlier false positive.)
  if report.paragraphs_imported > 0 && projection.text.to_string().trim().is_empty() && projection.blocks.is_empty() {
    return Err(format!(
      "content loss: imported {} paragraph(s) but the projection has no text and no blocks",
      report.paragraphs_imported,
    ));
  }
  // §perf-heaven T1 corpus validation: project a SNAPSHOT-REIMPORTED (cold,
  // `LazyLoad::Src`) state so `get_richtext_value` takes the T1 Src fast path
  // (the normal import above leaves the state warm/`Dst`, which never exercises
  // it). With `FLOWSTATE_RICHTEXT_VERIFY` on, that path also builds the full
  // state and asserts bit-identical — so a T1 divergence panics HERE and is
  // recorded as this document's failure (per-file catch_unwind).
  if std::env::var_os("FLOWSTATE_SNAPSHOT_VERIFY").is_some() {
    let snapshot = imported
      .doc
      .export(loro::ExportMode::Snapshot)
      .map_err(|error| format!("snapshot export: {error}"))?;
    let cold = loro::LoroDoc::new();
    cold.import(&snapshot).map_err(|error| format!("snapshot reimport: {error}"))?;
    let (reprojection, snap_defects) =
      flowstate_document::document_from_loro_with_defects(&cold).map_err(|error| format!("cold project: {error}"))?;
    // §perf-heaven T7.26: the cold (`LazyLoad::Src`) reprojection must equal the
    // warm one. The warm `projection` already passed the zero-defect gate above,
    // so the Src path — `get_richtext_value`, and now the Src-safe `char_at`
    // object-anchor validation — MUST also produce zero defects and BYTE-identical
    // text. A `char_at` Src bug that misplaces/quarantines an object shows up as a
    // non-empty `snap_defects` or a text divergence RIGHT HERE, giving the
    // object-anchor Src path the equivalence net `get_richtext_value` already has
    // via `FLOWSTATE_RICHTEXT_VERIFY`. (Earlier this was ignored as "the doc's own
    // repair concern"; that is wrong once the warm side is known defect-free.)
    if !snap_defects.is_empty() {
      return Err(format!(
        "T7.26 Src-path projection produced {} defect(s) the warm path did not; sample: {:?}",
        snap_defects.len(),
        snap_defects.iter().take(3).collect::<Vec<_>>(),
      ));
    }
    if reprojection.text != projection.text {
      return Err("T7.26 Src-path projection text diverged from the warm projection".to_string());
    }
  }

  if roundtrip {
    // Export from the ASSET-BEARING projection (built from the docx during
    // import), not the Loro-only re-projection above: Loro stores asset metadata
    // (hash/length) but NOT the bytes (they live out-of-band in the package), so
    // `document_from_loro`'s projection has no image bytes and the exporter falls
    // back to `[alt text]` — a harness artifact the real app never hits (it loads
    // asset bytes from the package). `imported.projection` matches the live app.
    //
    // §act-eleven A11.8: the COMPARISON baseline is the Loro MATERIALIZATION of
    // the first import (`projection`), matching the reimport side like-for-like
    // — the live app materializes both sides, so the old docx-direct baseline
    // asserted a state the user never sees and mis-flagged the materializer's
    // object-adjacency folding (the last standing corpus "residual") as loss.
    return check_roundtrip(&imported.projection, &projection);
  }
  Ok(())
}

/// Export the projection back to `.docx`, re-import it, and assert the text
/// survives. `export_source` is the asset-bearing docx-built projection (what
/// the app persists/exports); `baseline` is the Loro materialization of the
/// SAME import (what the app displays) — the reimport is compared against
/// `baseline`, Loro-vs-Loro.
fn check_roundtrip(
  export_source: &flowstate_document::DocumentProjection,
  baseline: &flowstate_document::DocumentProjection,
) -> Result<(), String> {
  let unique = format!("fscorpus-{}-{:?}.docx", std::process::id(), std::thread::current().id());
  let tmp = std::env::temp_dir().join(unique);
  flowstate_docx::write_docx(&tmp, export_source).map_err(|error| format!("write_docx: {error}"))?;
  let bytes = std::fs::read(&tmp).map_err(|error| format!("reread: {error}"));
  // Diagnostic: keep the exported bytes for inspection instead of deleting.
  if let Some(dir) = std::env::var_os("FLOWSTATE_ROUNDTRIP_DUMP") {
    let _ = std::fs::create_dir_all(&dir);
    let name = tmp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "export.docx".into());
    if let Ok(bytes) = &bytes {
      let _ = std::fs::write(std::path::Path::new(&dir).join(name), bytes);
    }
  }
  let _ = std::fs::remove_file(&tmp);
  let bytes = bytes?;
  let (imported, _report) =
    flowstate_docx::import_docx_bytes_to_loro(&bytes, "corpus-sweep-rt").map_err(|error| format!("reimport: {error}"))?;
  let (reprojection, defects) =
    flowstate_document::document_from_loro_with_defects(&imported.doc).map_err(|error| format!("reproject: {error}"))?;
  if !defects.is_empty() {
    return Err(format!("round-trip produced {} projection defect(s)", defects.len()));
  }
  // §act-eleven C5: STYLE census (report-only). The roundtrip gate asserts
  // TEXT equality; run/paragraph styles and block kinds can silently degrade
  // across the corpus with no signal. Report divergences without failing so
  // the pre-existing count can be measured before this is promoted to a gate.
  if std::env::var_os("FLOWSTATE_ROUNDTRIP_STYLE_CENSUS").is_some() {
    let mut style_divergences = 0usize;
    let mut run_divergences = 0usize;
    let paragraphs = baseline.paragraphs.len().min(reprojection.paragraphs.len());
    for ix in 0..paragraphs {
      if baseline.paragraphs[ix].style != reprojection.paragraphs[ix].style {
        style_divergences += 1;
      }
      if baseline.paragraphs[ix].runs != reprojection.paragraphs[ix].runs {
        run_divergences += 1;
      }
    }
    let block_kind = |block: &flowstate_document::Block| match block {
      flowstate_document::Block::Paragraph(_) => 0u8,
      flowstate_document::Block::Image(_) => 1,
      flowstate_document::Block::Equation(_) => 2,
      flowstate_document::Block::Table(_) => 3,
    };
    let kinds_diverge = baseline.blocks.len() != reprojection.blocks.len()
      || baseline
        .blocks
        .iter()
        .zip(reprojection.blocks.iter())
        .any(|(before, after)| block_kind(before) != block_kind(after));
    if style_divergences > 0 || run_divergences > 0 || kinds_diverge {
      eprintln!(
        "STYLE-CENSUS paragraphs={paragraphs} style_divergences={style_divergences} run_divergences={run_divergences} block_kinds_diverge={kinds_diverge}"
      );
    }
  }
  if baseline.text != reprojection.text {
    if std::env::var_os("FLOWSTATE_ROUNDTRIP_DIFF").is_some() {
      let before = baseline.text.to_string();
      let after = reprojection.text.to_string();
      if let Some(dir) = std::env::var_os("FLOWSTATE_ROUNDTRIP_TEXTDUMP") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::Path::new(&dir).join("before.txt"), &before);
        let _ = std::fs::write(std::path::Path::new(&dir).join("after.txt"), &after);
      }
      let first = before
        .char_indices()
        .zip(after.chars())
        .find(|((_, b), a)| b != a)
        .map(|((i, _), _)| i)
        .unwrap_or(before.len().min(after.len()));
      let ctx = |s: &str, at: usize| {
        let start = at.saturating_sub(160);
        let end = (at + 160).min(s.len());
        s.get(start..end).unwrap_or("").replace('\n', "\\n").replace('\u{2028}', "\\u2028")
      };
      eprintln!(
        "ROUNDTRIP-DIFF len {} -> {} (first diff @ char {}):\n  BEFORE: …{}…\n  AFTER:  …{}…",
        before.chars().count(),
        after.chars().count(),
        first,
        ctx(&before, first),
        ctx(&after, first),
      );
    }
    // §perf-heaven T7.27: the mismatch is frequently data ADDED (a spurious
    // U+2028 / `\n` / `[alt]`), not lost — report the actual direction from the
    // char-count delta instead of always blaming loss.
    let before_len = baseline.text.to_string().chars().count();
    let after_len = reprojection.text.to_string().chars().count();
    let direction = match after_len.cmp(&before_len) {
      std::cmp::Ordering::Greater => format!("{} char(s) ADDED", after_len - before_len),
      std::cmp::Ordering::Less => format!("{} char(s) LOST", before_len - after_len),
      std::cmp::Ordering::Equal => "same length, content changed".to_string(),
    };
    return Err(format!(
      "round-trip text mismatch on export->reimport ({direction}); set FLOWSTATE_ROUNDTRIP_DIFF for the diff"
    ));
  }
  Ok(())
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(message) = payload.downcast_ref::<&str>() {
    (*message).to_string()
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message.clone()
  } else {
    "unknown panic".to_string()
  }
}

fn report(config: &Config, ledger: &Ledger, discovered: &[Discovered], checked_now: usize) {
  // Report over CURRENTLY-discovered files only (a file deleted from the corpus is not
  // a failure), reading each one's latest ledger outcome.
  let mut failures: Vec<(&str, &str)> = Vec::new();
  let mut ok = 0usize;
  for file in discovered {
    match ledger.get(&file.key) {
      Some(record) if record.ok => ok += 1,
      Some(record) => failures.push((file.key.as_str(), record.detail.as_str())),
      None => {}
    }
  }
  failures.sort_unstable();

  eprintln!(
    "---\ncorpus: {} files | ok {} | FAILED {} | checked this pass {}",
    discovered.len(),
    ok,
    failures.len(),
    checked_now,
  );
  for (path, detail) in failures.iter().take(40) {
    eprintln!("  FAIL {path}\n       {detail}");
  }
  if failures.len() > 40 {
    eprintln!("  ... and {} more (see {})", failures.len() - 40, config.report_path.display());
  }

  // Persist the full failure list for offline triage.
  if let Some(parent) = config.report_path.parent() {
    std::fs::create_dir_all(parent).ok();
  }
  let mut body = String::new();
  for (path, detail) in &failures {
    use std::fmt::Write as _;
    let _ = writeln!(body, "{path}\t{detail}");
  }
  if let Err(error) = std::fs::write(&config.report_path, body) {
    eprintln!("  (could not write report {}: {error})", config.report_path.display());
  }
}

fn load_ledger(path: &Path) -> Ledger {
  std::fs::read(path)
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default()
}

fn save_ledger(path: &Path, ledger: &Ledger) -> anyhow::Result<()> {
  let bytes = serde_json::to_vec_pretty(ledger)?;
  // Write-then-rename so a crash mid-write never corrupts the ledger.
  let tmp = path.with_extension("json.tmp");
  std::fs::write(&tmp, bytes)?;
  std::fs::rename(&tmp, path)?;
  Ok(())
}

fn unix_now() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
