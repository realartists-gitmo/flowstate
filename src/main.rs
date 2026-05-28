use std::{
  alloc::{GlobalAlloc, Layout},
  path::PathBuf,
};

use clap::Parser;

use flowstate::{run_standalone, write_demo_document};

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
  /// Optional path to the `.db8`, `.docx`, or `.fl0` document to open.
  #[arg(value_name = "PATH")]
  path: Option<PathBuf>,

  /// Write a freshly generated demo document to `data/demo.db8` and exit.
  /// Mutually exclusive with providing a `PATH`.
  #[arg(long, conflicts_with = "path")]
  write_demo_db8: bool,
}

#[hotpath::main(allocator = FlowstateAllocator)]
fn main() {
  let cli = Cli::parse();

  if cli.write_demo_db8 {
    write_demo_document().expect("failed to write data/demo.db8");
    return;
  }

  run_standalone(cli.path);
}
