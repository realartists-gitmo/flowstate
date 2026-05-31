use std::{
  alloc::{GlobalAlloc, Layout},
  path::PathBuf,
};

use clap::{Parser, Subcommand};

use flowstate::{
  docx_conversion::{convert_db8_to_docx, convert_db8_to_pdf, convert_docx_to_pdf, convert_pdf_to_db8},
  run_standalone, write_demo_document,
};

struct FlowstateAllocator;

impl Default for FlowstateAllocator {
  fn default() -> Self {
    Self
  }
}

unsafe impl GlobalAlloc for FlowstateAllocator {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    unsafe { mimalloc::MiMalloc.alloc(layout) }
  }

  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
  }
}

#[cfg(not(feature = "hotpath-alloc"))]
#[global_allocator]
static GLOBAL: FlowstateAllocator = FlowstateAllocator;

/// Command line arguments for the standalone rich text processor.
///
/// `clap`'s derive API turns this struct into a parser: it generates
/// `--help`/`-h`, validates input, and fills in defaults for us. The full
/// editor can use the library directly without going through this CLI.
#[derive(Parser)]
#[command(name = "Flowstate", about = "A rich-text editor for debate documents.")]
struct Cli {
  #[command(subcommand)]
  command: Option<CliCommand>,

  /// Optional path to the `.db8`, `.docx`, `.pdf`, or `.fl0` document to open.
  #[arg(value_name = "PATH")]
  path: Option<PathBuf>,

  /// Write a freshly generated demo document to `data/demo.db8` and exit.
  /// Mutually exclusive with providing a `PATH`.
  #[arg(long, conflicts_with = "path")]
  write_demo_db8: bool,
}

#[derive(Subcommand)]
enum CliCommand {
  /// Convert a DB8 document to DOCX and exit.
  Db8ToDocx {
    /// Input `.db8` document.
    input: PathBuf,
    /// Output `.docx` path.
    output: PathBuf,
  },
  /// Convert a DOCX document to PDF and exit.
  DocxToPdf {
    /// Input `.docx` document.
    input: PathBuf,
    /// Output `.pdf` path.
    output: PathBuf,
  },
  /// Convert a DB8 document to PDF, embedding the DB8 for lossless recovery.
  Db8ToPdf {
    /// Input `.db8` document.
    input: PathBuf,
    /// Output `.pdf` path.
    output: PathBuf,
  },
  /// Extract an embedded Flowstate DB8 payload from a PDF.
  PdfToDb8 {
    /// Input `.pdf` document.
    input: PathBuf,
    /// Output `.db8` path.
    output: PathBuf,
  },
}

#[hotpath::main(allocator = FlowstateAllocator)]
fn main() {
  let cli = Cli::parse();

  if cli.write_demo_db8 {
    write_demo_document().expect("failed to write data/demo.db8");
    return;
  }

  if let Some(command) = cli.command {
    match command {
      CliCommand::Db8ToDocx { input, output } => {
        convert_db8_to_docx(input, output).expect("failed to export DOCX");
      },
      CliCommand::DocxToPdf { input, output } => {
        convert_docx_to_pdf(input, output).expect("failed to export PDF");
      },
      CliCommand::Db8ToPdf { input, output } => {
        convert_db8_to_pdf(input, output).expect("failed to export PDF");
      },
      CliCommand::PdfToDb8 { input, output } => {
        convert_pdf_to_db8(input, output).expect("failed to extract DB8");
      },
    }
    return;
  }

  run_standalone(cli.path);
}
