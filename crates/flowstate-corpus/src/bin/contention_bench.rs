//! Gate-contention bench (G23): measure local keystroke latency SOLO vs while a second
//! thread hammers remote imports through the SAME `WriteGate` — the real "another peer is
//! typing while I type" contention the field logs blamed for ~36 ms keystrokes.
//!
//! Run: `cargo run --release --bin contention_bench -- <FILE.docx> [KEYSTROKES]`.
//! (No profiler feature — plain wall-clock; contention is a scheduling effect.)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use flowstate_collab::crdt_runtime::CrdtRuntime;
use flowstate_collab::local_write::{GateHolder, LocalDocHandle, LocalWriteConfig};
use gpui_flowtext::{InsertTextIntent, TextAnchor};

fn main() {
  let mut args = std::env::args().skip(1);
  let Some(path) = args.next().or_else(|| std::env::var("FLOWSTATE_BENCH_FIXTURE").ok()) else {
    eprintln!("usage: contention_bench <FILE.docx> [KEYSTROKES]");
    std::process::exit(2);
  };
  let keystrokes: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(200);

  let (projection, report) = flowstate_docx::convert_docx_to_document(&path).expect("convert docx");
  eprintln!("file: {path}  ({} paragraphs)  keystrokes={keystrokes}", report.paragraphs_imported);
  let core = CrdtRuntime::from_document_projection(&projection, "contention").expect("runtime");

  // Peer B (fork) will stream imports at A during the contended run.
  let doc_b = core.doc().fork();
  let doc_b2 = core.doc().fork();
  doc_b.set_peer_id(0x00B0_B0B0).expect("distinct peer id");

  let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
  let live = handle.projection().expect("projection");
  let first = *live.ids.paragraph_ids.first().expect("a paragraph");

  let keystroke = |handle: &LocalDocHandle| {
    handle
      .insert_text(InsertTextIntent { at: TextAnchor::new(first, 0), text: "z".to_string(), style_override: None })
      .expect("insert_text");
  };

  // SOLO: keystroke latency with no competing thread.
  let solo = measure_latencies(keystrokes, || keystroke(&handle));
  report_lat("SOLO keystroke", &solo);

  // CONTENDED: a background thread hammers remote imports through the same gate while we
  // measure keystroke latency on this thread. Both contend on the single write gate.
  let stop = Arc::new(AtomicBool::new(false));
  let importer = {
    let gate = Arc::clone(&gate);
    let stop = Arc::clone(&stop);
    std::thread::spawn(move || {
      let body = flowstate_document::loro_schema::body_text(&doc_b);
      let mut vv = doc_b.state_vv();
      let mut imports = 0u64;
      while !stop.load(Ordering::Relaxed) {
        body.insert(1, "r").expect("peer insert");
        doc_b.commit();
        let update = doc_b.export(loro::ExportMode::updates(&vv)).expect("export");
        vv = doc_b.state_vv();
        if let Ok(mut guard) = gate.lock(GateHolder::ImportChunk) {
          let _ = guard.import_remote_update(&update);
          imports += 1;
        }
      }
      imports
    })
  };

  let contended = measure_latencies(keystrokes, || keystroke(&handle));
  report_lat("CONTENDED keystroke", &contended);

  // PACED: realistic typing cadence (5ms between keystrokes) so the importer
  // actually streams between them — this is the field shape ("keystroke
  // waits behind an import hold"), which back-to-back keystrokes mask by
  // starving the importer.
  let paced = measure_paced_latencies(keystrokes, std::time::Duration::from_millis(5), || keystroke(&handle));
  stop.store(true, Ordering::Relaxed);
  let imports = importer.join().expect("importer thread");
  report_lat("PACED keystroke+5ms", &paced);

  // §A14.1 gate: paced typing WHILE a mass op streams in as 2048-atom
  // slices (the sender-side chunking shape). Pre-build the slice queue,
  // then type paced while a thread feeds them through the gate.
  let mass_slices: Vec<Vec<u8>> = {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
    let from_vv = guard.doc().state_vv();
    drop(guard);
    let body = flowstate_document::loro_schema::body_text(&doc_b2);
    body.insert(1, &"mass restyle payload ".repeat(3_000)).expect("mass insert");
    doc_b2.commit();
    let peer = doc_b2.peer_id();
    let start = from_vv.get(&peer).copied().unwrap_or(0);
    let end = doc_b2.oplog_vv().get(&peer).copied().unwrap_or(start);
    (start..end)
      .step_by(4096)
      .map(|w| {
        doc_b2
          .export(loro::ExportMode::updates_in_range(&[loro::IdSpan::new(peer, w, (w + 4096).min(end))]))
          .expect("slice export")
      })
      .collect()
  };
  let slice_count = mass_slices.len();
  let mass_importer = {
    let gate = Arc::clone(&gate);
    std::thread::spawn(move || {
      for slice in mass_slices {
        if let Ok(mut guard) = gate.lock(GateHolder::ImportChunk) {
          let _ = guard.import_remote_update(&slice);
        }
      }
    })
  };
  let mass_paced = measure_paced_latencies(60, std::time::Duration::from_millis(5), || keystroke(&handle));
  mass_importer.join().expect("mass importer");
  report_lat("PACED during MASS op", &mass_paced);
  let metrics = gate.metrics();
  eprintln!(
    "mass: slices={slice_count} max_hold(import)={}us max_wait(local)={}us",
    metrics.max_hold_micros_import_chunk.load(Ordering::Relaxed),
    metrics.max_wait_micros_local_intent.load(Ordering::Relaxed),
  );
  let metrics = gate.metrics();
  eprintln!(
    "gate: acquisitions={} contended={} max_hold(import)={}us max_hold(local)={}us max_wait(local)={}us",
    metrics.acquisitions.load(Ordering::Relaxed),
    metrics.contended_acquisitions.load(Ordering::Relaxed),
    metrics.max_hold_micros_import_chunk.load(Ordering::Relaxed),
    metrics.max_hold_micros_local_intent.load(Ordering::Relaxed),
    metrics.max_wait_micros_local_intent.load(Ordering::Relaxed),
  );

  eprintln!(
    "\nimports landed during contended run: {imports}\ncontention factor (median contended / median solo): {:.1}x",
    if solo.median > 0.0 { contended.median / solo.median } else { 0.0 },
  );
}

struct Lat {
  median: f64,
  p95: f64,
  max: f64,
}

/// Like `measure_latencies`, but sleeps `gap` between ops OUTSIDE the timed
/// region — the importer streams during the gaps (the field shape).
fn measure_paced_latencies(count: usize, gap: std::time::Duration, mut op: impl FnMut()) -> Lat {
  let mut samples = Vec::with_capacity(count);
  for _ in 0..count {
    let t = Instant::now();
    op();
    samples.push(t.elapsed().as_secs_f64() * 1000.0);
    std::thread::sleep(gap);
  }
  samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
  Lat {
    median: samples[samples.len() / 2],
    p95: samples[samples.len() * 95 / 100],
    max: samples[samples.len() - 1],
  }
}

fn measure_latencies(count: usize, mut op: impl FnMut()) -> Lat {
  let mut samples = Vec::with_capacity(count);
  for _ in 0..count {
    let t = Instant::now();
    op();
    samples.push(t.elapsed().as_secs_f64() * 1000.0);
  }
  samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
  Lat {
    median: samples[samples.len() / 2],
    p95: samples[samples.len() * 95 / 100],
    max: samples[samples.len() - 1],
  }
}

fn report_lat(name: &str, lat: &Lat) {
  println!("{name:<24} median {:6.2} ms   p95 {:6.2} ms   max {:6.2} ms", lat.median, lat.p95, lat.max);
}
