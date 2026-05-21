use std::path::PathBuf;

use clap::Parser;

use debateprocessor::{run_standalone, write_demo_document};

/// Command line arguments for the standalone rich text processor.
///
/// `clap`'s derive API turns this struct into a parser: it generates
/// `--help`/`-h`, validates input, and fills in defaults for us. The full
/// editor can use the library directly without going through this CLI.
#[derive(Parser)]
#[command(name = "debateprocessor", about = "A rich-text editor for debate documents.")]
struct Cli {
  /// Optional path to the `.db8` document to open.
  #[arg(value_name = "PATH")]
  path: Option<PathBuf>,

  /// Write a freshly generated demo document to `data/demo.db8` and exit.
  /// Mutually exclusive with providing a `PATH`.
  #[arg(long, conflicts_with = "path")]
  write_demo_db8: bool,
}

fn main() {
  let cli = Cli::parse();

  if cli.write_demo_db8 {
    write_demo_document().expect("failed to write data/demo.db8");
    return;
  }

  run_standalone(cli.path);
}
