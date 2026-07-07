use std::{
  alloc::{GlobalAlloc, Layout},
  path::PathBuf,
};

use clap::{Parser, Subcommand};

use flowstate::{
  docx_conversion::{convert_db8_to_docx, convert_db8_to_pdf, convert_docx_to_pdf, convert_pdf_to_db8},
  logging, run_standalone, write_demo_document,
};

struct FlowstateAllocator;

impl Default for FlowstateAllocator {
  fn default() -> Self {
    Self
  }
}

// SAFETY: This allocator delegates all allocation operations directly to
// mimalloc's `GlobalAlloc` implementation without changing the pointer,
// layout, or ownership contracts.
unsafe impl GlobalAlloc for FlowstateAllocator {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    // SAFETY: The caller upholds `GlobalAlloc::alloc`'s layout contract, and
    // the layout is forwarded unchanged to mimalloc.
    unsafe { mimalloc::MiMalloc.alloc(layout) }
  }

  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    // SAFETY: The caller guarantees `ptr` and `layout` came from this
    // allocator; both are forwarded unchanged to mimalloc.
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
  /// Headless collaboration hotpath soak: load a document, type/split/import
  /// through the real write path, print latency distributions and (with
  /// `--features hotpath-cpu`) the per-stage breakdown.
  CollabHotpath {
    /// Input `.docx` or package document.
    input: PathBuf,
    /// Local typing keystrokes to measure.
    #[arg(long, default_value_t = 160)]
    keystrokes: usize,
    /// Local paragraph splits to measure.
    #[arg(long, default_value_t = 8)]
    splits: usize,
    /// Remote import chunks to measure (every 6th is structural).
    #[arg(long, default_value_t = 24)]
    imports: usize,
    /// Release audit sampling: `off`, or audit every N-th intent
    /// (debug builds always audit every commit regardless).
    #[arg(long)]
    audit: Option<String>,
  },
}

#[hotpath::main(allocator = FlowstateAllocator)]
fn main() {
  let _logging_guard = match logging::init() {
    Ok(guard) => Some(guard),
    Err(error) => {
      eprintln!("flowstate logging initialization failed: {error:#}");
      None
    },
  };

  // Enable CRDT/document fidelity diagnostics when FLOWSTATE_TRACE_FIDELITY is
  // set. Must run after logging init so violation dumps reach the subscriber.
  flowstate_fidelity::init_from_env();

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
      CliCommand::CollabHotpath {
        input,
        keystrokes,
        splits,
        imports,
        audit,
      } => {
        let audit = audit.map(|value| {
          if value.eq_ignore_ascii_case("off") {
            None
          } else {
            Some(value.parse().expect("--audit takes `off` or a positive integer"))
          }
        });
        let options = flowstate::collab_hotpath::CollabHotpathOptions {
          keystrokes,
          splits,
          imports,
          audit,
        };
        flowstate::collab_hotpath::run(&input, &options).expect("collab hotpath soak failed");
      },
    }
    return;
  }

  run_standalone(cli.path);
}
