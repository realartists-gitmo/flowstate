use std::{env, path::{Path, PathBuf}};

use anyhow::{Context as _, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::app_settings::flowstate_data_dir;

const DEFAULT_LOG_FILTER: &str = "flowstate::collab=trace,flowstate::workspace::workspace=trace,flowstate_collab=trace";

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
  std::fs::create_dir_all(&directory)
    .with_context(|| format!("creating log directory {} failed", directory.display()))?;
  let file_appender = tracing_appender::rolling::daily(&directory, "flowstate.log");
  let (writer, guard) = tracing_appender::non_blocking(file_appender);

  tracing_subscriber::registry()
    .with(env_filter())
    .with(
      tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true),
    )
    .try_init()
    .context("initializing flowstate logging failed")?;

  Ok(LoggingGuard {
    _guard: guard,
    directory,
  })
}

fn env_filter() -> EnvFilter {
  let filter = env::var("FLOWSTATE_LOG")
    .or_else(|_| env::var("RUST_LOG"))
    .unwrap_or_else(|_| DEFAULT_LOG_FILTER.to_string());

  EnvFilter::try_new(&filter).unwrap_or_else(|error| {
    eprintln!("invalid FLOWSTATE_LOG/RUST_LOG filter {filter:?}: {error}; falling back to {DEFAULT_LOG_FILTER:?}");
    EnvFilter::new(DEFAULT_LOG_FILTER)
  })
}

fn log_directory() -> PathBuf {
  env::var_os("FLOWSTATE_LOG_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| flowstate_data_dir().join("logs"))
}
