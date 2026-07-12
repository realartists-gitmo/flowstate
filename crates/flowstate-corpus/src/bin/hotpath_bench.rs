//! Headless profiler bench — the same hotpath timing/alloc/cpu tables `cargo survey`
//! dumps for the app, but driven deterministically over a real `.docx` with no GUI.
//!
//! It reproduces the field "reopen / receiving-peer" condition (the body is UNDECODED
//! at projection time) via the snapshot round-trip trick, then cold-projects N times.
//! The `#[hotpath::measure]` annotations already in `flowstate-document` /
//! `flowstate-docx` (`object_blocks_for_flow`, `projector_body_to_delta`,
//! `import_docx_to_loro`, `paragraph_ids_by_boundary`, …) populate the tables; the
//! `hotpath::main` macro dumps them on exit.
//!
//! Run (release, pick one profiler axis) — e.g.
//! `cargo run --release --features hotpath-alloc --bin hotpath_bench -- <FILE.docx> [ITERS]`:
//!   - `hotpath` → timing table,
//!   - `hotpath-cpu` → + cpu attribution,
//!   - `hotpath-alloc` → + alloc-bytes per fn.
//!
//! Without any feature it is a plain timed run (no tables) — still useful.

use std::alloc::{GlobalAlloc, Layout};

struct FlowstateAllocator;

impl Default for FlowstateAllocator {
  fn default() -> Self {
    Self
  }
}

// SAFETY: every call forwards its arguments unchanged to mimalloc, which upholds the
// `GlobalAlloc` contract; this wrapper adds no behavior of its own.
unsafe impl GlobalAlloc for FlowstateAllocator {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    // SAFETY: the caller upholds `GlobalAlloc::alloc`'s layout contract; the layout
    // is forwarded to mimalloc unchanged.
    unsafe { mimalloc::MiMalloc.alloc(layout) }
  }
  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    // SAFETY: the caller guarantees `ptr`/`layout` came from this allocator; both
    // are forwarded to mimalloc unchanged.
    unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
  }
}

// When alloc-profiling is on, hotpath installs its own tracking allocator (wrapping
// this one); otherwise this is the process global allocator, matching the app.
#[cfg(not(feature = "hotpath-alloc"))]
#[global_allocator]
static GLOBAL: FlowstateAllocator = FlowstateAllocator;

#[hotpath::main(allocator = FlowstateAllocator)]
fn main() {
  let mut args = std::env::args().skip(1);
  let Some(path) = args.next().or_else(|| std::env::var("FLOWSTATE_BENCH_FIXTURE").ok()) else {
    eprintln!(
      "usage: hotpath_bench <FILE.docx | synthetic:<kind>:<paragraphs>> [ITERS]   (or set FLOWSTATE_BENCH_FIXTURE)\n  \
       synthetic kinds: equations (default), mixed, text — e.g. synthetic:equations:9000"
    );
    std::process::exit(2);
  };
  let iters: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(3);

  eprintln!("hotpath_bench: {path}  (cold-project x{iters})");

  // IMPORT: source -> Loro doc. A `synthetic:*` selector builds an equation/object
  // fixture (§perf-heaven T7 — no equation docs in the corpus); otherwise docx bytes
  // through the interpreter (whose #[hotpath::measure] fns fire).
  let doc = if let Some((projection, count)) = flowstate_corpus::fixtures::from_selector(&path) {
    eprintln!("  synthetic fixture: {count} paragraphs, {} blocks", projection.blocks.len());
    flowstate_collab::crdt_runtime::CrdtRuntime::from_document_projection(&projection, "hotpath-bench")
      .expect("runtime")
      .doc()
      .clone()
  } else {
    let (imported, report) = flowstate_docx::import_docx_to_loro(&path, "hotpath-bench").expect("import docx");
    eprintln!("  imported {} paragraphs", report.paragraphs_imported);
    imported.doc
  };
  let snapshot = doc.export(loro::ExportMode::Snapshot).expect("snapshot");

  // WARM baseline: project the already-decoded doc once (cheap; for contrast).
  let _ = flowstate_document::document_from_loro(&doc).expect("warm project");

  // COLD: each iteration re-imports the snapshot into a FRESH (lazy/undecoded) doc
  // and projects it — the field reopen condition where the body decode + the
  // object_blocks / body-to-delta scan family dominate. This is what the tables sample.
  for iteration in 0..iters.max(1) {
    let doc = loro::LoroDoc::new();
    doc.import(&snapshot).expect("reimport");
    let projection = flowstate_document::document_from_loro(&doc).expect("cold project");
    if iteration == 0 {
      eprintln!("  projection: {} paragraphs, {} blocks", projection.paragraphs.len(), projection.blocks.len());
    }
  }
  eprintln!("hotpath_bench: done — tables below.");
}
