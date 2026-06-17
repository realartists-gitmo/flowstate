use std::{
  env,
  path::{Path, PathBuf},
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
  std::fs::create_dir_all(&directory).with_context(|| format!("creating log directory {} failed", directory.display()))?;
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
