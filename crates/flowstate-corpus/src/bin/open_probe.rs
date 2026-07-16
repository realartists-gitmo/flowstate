//! §act-twelve A12.1.0: the `.db8` cold-open stage probe. Converts a docx
//! once, checkpoints it as a package, then times a fresh `open_package`
//! (set `FLOWSTATE_OPEN_PROBE=1` for the in-runtime stage prints).
//!
//! Usage: `FLOWSTATE_OPEN_PROBE=1 open_probe <FILE.docx> [N_OPENS]`

use flowstate_collab::crdt_runtime::CrdtRuntime;

fn main() {
  let mut args = std::env::args().skip(1);
  let Some(path) = args
    .next()
    .or_else(|| std::env::var("FLOWSTATE_BENCH_FIXTURE").ok())
  else {
    eprintln!("usage: open_probe <FILE.docx> [N_OPENS]  (or set FLOWSTATE_BENCH_FIXTURE)");
    std::process::exit(2);
  };
  let opens: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(3);
  let dir = tempfile::tempdir().expect("tempdir");
  let db8_path = match std::env::var_os("FLOWSTATE_KEEP_DB8") {
    Some(p) => std::path::PathBuf::from(p),
    None => dir.path().join("open-probe.db8"),
  };

  let t0 = std::time::Instant::now();
  let (projection, _report) = flowstate_docx::convert_docx_to_document(&path).expect("convert docx");
  eprintln!("[open-probe] docx convert: {:?}", t0.elapsed());
  let t1 = std::time::Instant::now();
  let mut runtime = CrdtRuntime::from_document_projection(&projection, "open-probe").expect("runtime");
  eprintln!("[open-probe] CRDT import: {:?}", t1.elapsed());
  let t2 = std::time::Instant::now();
  runtime
    .checkpoint_package("open-probe", Some(db8_path.clone()), &flowstate_document::RevisionStamp::session())
    .expect("checkpoint");
  eprintln!("[open-probe] checkpoint (.db8 write): {:?}", t2.elapsed());
  drop(runtime);

  let mut last = None;
  for i in 0..opens {
    let t = std::time::Instant::now();
    let reopened = CrdtRuntime::open_package(&db8_path).expect("open package");
    let open_time = t.elapsed();
    let paragraphs = reopened
      .projection_snapshot()
      .expect("projection")
      .paragraphs
      .len();
    eprintln!("[open-probe] OPEN #{i}: {open_time:?} ({paragraphs} paragraphs)");
    last = Some(reopened);
  }

  // §A13.4.0: time a ROUTINE checkpoint (the split off-thread job path) on
  // the reopened runtime — the shape autosave/explicit-save actually run.
  if let Some(mut runtime) = last {
    let t = std::time::Instant::now();
    let (job, _events) = runtime
      .begin_checkpoint("open-probe-routine", Some(db8_path.clone()), &flowstate_document::RevisionStamp::auto())
      .expect("begin checkpoint")
      .expect("package present");
    let begin_time = t.elapsed();
    let t = std::time::Instant::now();
    let (package, wrote) = job.run();
    wrote.expect("checkpoint job");
    runtime.finish_checkpoint(package, true);
    eprintln!("[open-probe] routine checkpoint: begin(gate)={begin_time:?} job={:?}", t.elapsed());
  }
}
