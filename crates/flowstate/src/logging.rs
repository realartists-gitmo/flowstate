use std::{
  env, fs,
  path::{Path, PathBuf},
  time::{Duration, SystemTime},
};

use anyhow::{Context as _, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, Layer as _, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _};

/// Default filter: only the most serious events (errors), for every target.
/// Raise Flowstate's own verbosity with `FLOWSTATE_LOG_LEVEL`, or take full
/// control (including dependencies) with `FLOWSTATE_LOG` / `RUST_LOG`.
const DEFAULT_LOG_FILTER: &str = "error";

/// Crates whose verbosity `FLOWSTATE_LOG_LEVEL` raises. Dependencies stay at the
/// default level so the output stays focused on Flowstate's own logs.
const FLOWSTATE_TARGETS: [&str; 3] = ["flowstate", "flowstate_collab", "gpui_flowtext"];

pub struct LoggingGuard {
  _guard: WorkerGuard,
  // Kept alive so the fidelity JSONL background writer flushes on shutdown.
  _fidelity_guard: Option<WorkerGuard>,
  directory: PathBuf,
}

impl LoggingGuard {
  pub fn directory(&self) -> &Path {
    &self.directory
  }
}
pub fn init() -> Result<LoggingGuard> {
  let directory = log_directory();
  fs::create_dir_all(&directory).with_context(|| format!("creating log directory {} failed", directory.display()))?;
  prune_log_files(&directory)?;
  // One unique, sortable, human-readable stamp per run so successive runs never
  // clobber each other's logs. (The old `rolling::daily` produced a single
  // `flowstate.log.<date>` that every run on the same day appended to, which made
  // it impossible to tell one run's logs from another's.)
  let run_stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
  let file_appender = tracing_appender::rolling::never(&directory, format!("flowstate-{run_stamp}.log"));
  let (writer, guard) = tracing_appender::non_blocking(file_appender);

  // Optionally mirror logs to stdout (handy when running from a terminal).
  let stdout_layer = log_to_stdout().then(|| {
    fmt::layer()
      .with_writer(std::io::stdout)
      .with_ansi(true)
      .with_file(true)
      .with_line_number(true)
      .with_target(true)
  });

  // When fidelity tracing is on, additionally stream ONLY the fidelity events
  // (and violations/markers, matched by the `fidelity` target prefix) as
  // machine-parseable JSON lines to a dedicated file, so an automated agent gets
  // exactly one JSON object per event without scraping the human log format.
  // The fidelity JSONL sink MUST be non-blocking. Fidelity `event()`s fire from
  // the render/layout hot path (per visible paragraph, per decoration, per caret),
  // so a blocking `Mutex<File>` writer would stall the MAIN thread on file I/O
  // once per event — on a large document that is thousands of synchronous writes
  // per frame, which froze the window. A background worker (like the human log)
  // keeps the UI thread free; the returned guard flushes it on shutdown.
  let (fidelity_writer, fidelity_guard) = match trace_fidelity_enabled()
    .then(|| directory.join(format!("flowstate-fidelity-{run_stamp}.jsonl")))
    .and_then(|path| fs::File::create(&path).ok())
  {
    Some(file) => {
      let (writer, guard) = tracing_appender::non_blocking(file);
      (Some(writer), Some(guard))
    },
    None => (None, None),
  };
  let fidelity_json_layer = fidelity_writer.map(|writer| {
    fmt::layer()
      .json()
      .flatten_event(true)
      .with_current_span(false)
      .with_span_list(false)
      .with_writer(writer)
      .with_filter(EnvFilter::new("fidelity=debug"))
  });

  tracing_subscriber::registry()
    .with(env_filter())
    .with(
      fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true),
    )
    .with(stdout_layer)
    .with(fidelity_json_layer)
    .try_init()
    .context("initializing flowstate logging failed")?;

  // Tell the operator where THIS run's logs are, on stderr, so a terminal run
  // shows the exact paths without grepping.
  if trace_fidelity_enabled() {
    eprintln!(
      "flowstate: logs → {}/flowstate-{run_stamp}.log + flowstate-fidelity-{run_stamp}.jsonl (fidelity tracing ON)",
      directory.display()
    );
  } else {
    eprintln!("flowstate: logs → {}/flowstate-{run_stamp}.log", directory.display());
  }

  Ok(LoggingGuard {
    _guard: guard,
    _fidelity_guard: fidelity_guard,
    directory,
  })
}

/// Resolves the log filter from the environment, in precedence order:
/// 1. `FLOWSTATE_LOG` — full [`EnvFilter`] directive (advanced; also controls
///    dependency crates), e.g. `flowstate_collab=trace,flowstate::collab=debug`.
/// 2. `RUST_LOG` — the standard [`EnvFilter`] directive.
/// 3. `FLOWSTATE_LOG_LEVEL` — a single level (`error|warn|info|debug|trace`)
///    applied to Flowstate's own crates, leaving dependencies at `error`.
/// 4. Default — `error` only.
fn env_filter() -> EnvFilter {
  let mut filter = base_env_filter();
  if trace_fidelity_enabled() {
    // A single `fidelity=debug` directive surfaces the whole diagnostics
    // stream: EnvFilter matches targets by prefix, so this also covers the
    // `fidelity.violation` target (emitted at error), regardless of the
    // ambient log level. One env var thus yields both firehose and violations.
    filter = filter.add_directive(
      "fidelity=debug"
        .parse()
        .expect("static `fidelity=debug` directive is always valid"),
    );
  }
  filter
}

fn base_env_filter() -> EnvFilter {
  let Some(directive) = directive_from_env() else {
    return EnvFilter::new(DEFAULT_LOG_FILTER);
  };
  EnvFilter::try_new(&directive).unwrap_or_else(|error| {
    eprintln!("invalid log filter {directive:?}: {error}; falling back to {DEFAULT_LOG_FILTER:?}");
    EnvFilter::new(DEFAULT_LOG_FILTER)
  })
}

/// Mirrors `flowstate_fidelity`'s env parsing: any non-empty, non-`0` value of
/// `FLOWSTATE_TRACE_FIDELITY` turns diagnostics on. Kept local so logging setup
/// has no cross-crate ordering dependency.
fn trace_fidelity_enabled() -> bool {
  env::var("FLOWSTATE_TRACE_FIDELITY").is_ok_and(|value| {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed != "0"
  })
}

fn directive_from_env() -> Option<String> {
  for var in ["FLOWSTATE_LOG", "RUST_LOG"] {
    if let Ok(filter) = env::var(var)
      && !filter.trim().is_empty()
    {
      return Some(filter);
    }
  }

  let level = level_from_env()?;
  let scoped = FLOWSTATE_TARGETS
    .iter()
    .map(|target| format!("{target}={level}"))
    .collect::<Vec<_>>()
    .join(",");
  Some(format!("{DEFAULT_LOG_FILTER},{scoped}"))
}

fn level_from_env() -> Option<&'static str> {
  let value = env::var("FLOWSTATE_LOG_LEVEL").ok()?;
  match value.trim().to_ascii_lowercase().as_str() {
    "" => None,
    "error" => Some("error"),
    "warn" | "warning" => Some("warn"),
    "info" => Some("info"),
    "debug" => Some("debug"),
    "trace" => Some("trace"),
    other => {
      eprintln!("invalid FLOWSTATE_LOG_LEVEL {other:?}; expected error|warn|info|debug|trace");
      None
    },
  }
}

fn log_to_stdout() -> bool {
  env::var("FLOWSTATE_LOG_STDOUT").is_ok_and(|value| is_truthy(&value))
}

fn is_truthy(value: &str) -> bool {
  matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn log_directory() -> PathBuf {
  // `FLOWSTATE_LOG_DIR` still wins when set. Otherwise default to a
  // `flowstate-logs/` directory under the current working directory — for the
  // usual `cargo run` from the repo root that puts logs right in the repo (and
  // it's git-ignored) instead of a hidden platform data dir under `/tmp`-style
  // paths, which is far easier to find during development.
  env::var_os("FLOWSTATE_LOG_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| {
      env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("flowstate-logs")
    })
}

/// Build a user-initiated diagnostics bundle without telemetry or network I/O.
/// Invite bearers and the user's home-directory prefix are removed before the
/// selected local log files are copied into one readable report.
pub fn export_redacted_diagnostics(destination: &Path) -> Result<()> {
  let directory = log_directory();
  let mut logs = fs::read_dir(&directory)
    .with_context(|| format!("reading log directory {} failed", directory.display()))?
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path())
    .filter(|path| matches!(path.extension().and_then(|extension| extension.to_str()), Some("log" | "jsonl")))
    .collect::<Vec<_>>();
  logs.sort();
  let home = dirs::home_dir().and_then(|path| path.to_str().map(str::to_owned));
  let mut report = String::from("Flowstate diagnostics (local, redacted)\n\n");
  for path in logs.into_iter().rev().take(4).rev() {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    use std::fmt::Write as _;
    let _ = writeln!(report, "--- {name} ---");
    let contents = fs::read_to_string(&path).unwrap_or_else(|error| format!("[unreadable log: {error}]"));
    for line in contents.lines() {
      report.push_str(&redact_diagnostic_line(line, home.as_deref()));
      report.push('\n');
    }
  }
  atomicwrites::AtomicFile::new(destination, atomicwrites::OverwriteBehavior::AllowOverwrite)
    .write(|file| std::io::Write::write_all(file, report.as_bytes()))
    .with_context(|| format!("writing diagnostics export {} failed", destination.display()))
}

fn redact_diagnostic_line(line: &str, home: Option<&str>) -> String {
  let mut redacted = line
    .split_whitespace()
    .map(|token| {
      if token.contains(flowstate_collab::ticket::INVITE_URL_PREFIX) || token.contains(flowstate_collab::ticket::TICKET_KIND) {
        "[REDACTED_INVITE]"
      } else {
        token
      }
    })
    .collect::<Vec<_>>()
    .join(" ");
  if let Some(home) = home.filter(|home| !home.is_empty()) {
    redacted = redacted.replace(home, "~");
  }
  redacted
}
const MAX_LOG_FILES: usize = 14;
const MAX_LOG_AGE: Duration = Duration::from_hours(336);

fn prune_log_files(directory: &Path) -> Result<()> {
  let mut candidates = Vec::new();
  let now = SystemTime::now();
  for entry in fs::read_dir(directory).with_context(|| format!("reading log directory {} failed", directory.display()))? {
    let entry = entry?;
    let path = entry.path();
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    // Prune this run's per-run files (`flowstate-<stamp>.log`,
    // `flowstate-fidelity-<stamp>.jsonl`) and any legacy `flowstate.log.<date>`.
    let extension = path.extension().and_then(|extension| extension.to_str());
    let is_run_log = name.starts_with("flowstate-") && matches!(extension, Some("log" | "jsonl"));
    if !is_run_log && !name.starts_with("flowstate.log.") {
      continue;
    }
    let metadata = entry.metadata()?;
    let modified = metadata.modified().ok();
    let age = modified.and_then(|modified| now.duration_since(modified).ok());
    if age.is_some_and(|age| age > MAX_LOG_AGE) {
      let _ = fs::remove_file(&path);
      continue;
    }
    candidates.push((path, modified.unwrap_or(SystemTime::UNIX_EPOCH)));
  }

  if candidates.len() > MAX_LOG_FILES {
    candidates.sort_by_key(|(_, modified)| *modified);
    for (path, _) in candidates.drain(..candidates.len() - MAX_LOG_FILES) {
      let _ = fs::remove_file(path);
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn prunes_excess_log_files() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..20 {
      let path = dir.path().join(format!("flowstate.log.{index}"));
      fs::write(&path, index.to_string()).unwrap();
    }

    prune_log_files(dir.path()).unwrap();

    let retained = fs::read_dir(dir.path())
      .unwrap()
      .filter_map(|entry| entry.ok())
      .map(|entry| entry.file_name())
      .filter(|name| name.to_string_lossy().starts_with("flowstate.log."))
      .count();
    assert!(retained <= MAX_LOG_FILES);
  }

  #[test]
  fn diagnostic_redaction_removes_invites_and_home_paths() {
    let line = "join flowstate://join#fscollab-secret from /home/alex/Documents/brief.db8";
    let redacted = redact_diagnostic_line(line, Some("/home/alex"));
    assert!(!redacted.contains("fscollab-secret"));
    assert!(redacted.contains("[REDACTED_INVITE]"));
    assert!(redacted.contains("~/Documents/brief.db8"));
  }
}
