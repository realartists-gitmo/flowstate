use std::{
  env, fs,
  path::{Path, PathBuf},
  time::{Duration, SystemTime},
};

use anyhow::{Context as _, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::app_settings::flowstate_data_dir;

/// Default filter: only the most serious events (errors), for every target.
/// Raise Flowstate's own verbosity with `FLOWSTATE_LOG_LEVEL`, or take full
/// control (including dependencies) with `FLOWSTATE_LOG` / `RUST_LOG`.
const DEFAULT_LOG_FILTER: &str = "error";

/// Crates whose verbosity `FLOWSTATE_LOG_LEVEL` raises. Dependencies stay at the
/// default level so the output stays focused on Flowstate's own logs.
const FLOWSTATE_TARGETS: [&str; 3] = ["flowstate", "flowstate_collab", "gpui_flowtext"];

pub struct LoggingGuard {
  _guard: WorkerGuard,
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
  let file_appender = tracing_appender::rolling::daily(&directory, "flowstate.log");
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
    .try_init()
    .context("initializing flowstate logging failed")?;

  Ok(LoggingGuard { _guard: guard, directory })
}

/// Resolves the log filter from the environment, in precedence order:
/// 1. `FLOWSTATE_LOG` — full [`EnvFilter`] directive (advanced; also controls
///    dependency crates), e.g. `flowstate_collab=trace,flowstate::collab=debug`.
/// 2. `RUST_LOG` — the standard [`EnvFilter`] directive.
/// 3. `FLOWSTATE_LOG_LEVEL` — a single level (`error|warn|info|debug|trace`)
///    applied to Flowstate's own crates, leaving dependencies at `error`.
/// 4. Default — `error` only.
fn env_filter() -> EnvFilter {
  let Some(directive) = directive_from_env() else {
    return EnvFilter::new(DEFAULT_LOG_FILTER);
  };
  EnvFilter::try_new(&directive).unwrap_or_else(|error| {
    eprintln!("invalid log filter {directive:?}: {error}; falling back to {DEFAULT_LOG_FILTER:?}");
    EnvFilter::new(DEFAULT_LOG_FILTER)
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
  env::var_os("FLOWSTATE_LOG_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| flowstate_data_dir().join("logs"))
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
    if !name.starts_with("flowstate.log.") {
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
}
