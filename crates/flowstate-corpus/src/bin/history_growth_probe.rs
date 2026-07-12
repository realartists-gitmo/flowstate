//! §oom-leads #1: does document history growth degrade snapshot size and
//! open time without bound? Builds the SAME final content four ways and
//! measures each provenance's snapshot bytes, export time, decode time, and
//! cold first projection:
//!
//! * `fresh`        — one bulk transaction (the docx-import shape);
//! * `typed`        — one commit per inserted char (a long editing session);
//! * `typed-styled` — as `typed`, plus a run-style mark every 64th op
//!   (realistic highlight cadence);
//! * `shallow`      — the `typed-styled` doc exported with
//!   `ExportMode::shallow_snapshot` at its final frontier (the candidate fix:
//!   history GC'd to the frontier).
//!
//! Every leg prints one `HISTORY-GROWTH {json}` line — stable keys, no
//! timestamps — so an agent can diff runs mechanically. Convergence guard:
//! every decoded doc must project to the same body text as `fresh`.
//!
//! Run: `cargo run --release -p flowstate-corpus --bin history_growth_probe [ops]`
//! (default `100_000` ops; pass a smaller count for a smoke run).

use std::time::Instant;

use flowstate_document::{MARK_HIGHLIGHT_STYLE, document_from_loro, loro_schema};
use loro::{ExportMode, LoroDoc, LoroValue};

struct Leg {
  name: &'static str,
  snapshot: Vec<u8>,
  export_ms: f64,
  build_ms: f64,
}

fn measure_decode(name: &str, snapshot: &[u8], build_ms: f64, export_ms: f64, expected_text: &str) {
  let decode_start = Instant::now();
  let doc = LoroDoc::new();
  doc.import(snapshot).expect("snapshot import");
  let decode_ms = decode_start.elapsed().as_secs_f64() * 1e3;

  let project_start = Instant::now();
  let projection = document_from_loro(&doc).expect("cold projection");
  let project_ms = project_start.elapsed().as_secs_f64() * 1e3;

  let text = loro_schema::body_text(&doc).to_string();
  assert_eq!(text, expected_text, "{name}: decoded doc diverged from the fresh content");

  println!(
    "HISTORY-GROWTH {{\"leg\":\"{name}\",\"snapshot_bytes\":{},\"build_ms\":{build_ms:.1},\"export_ms\":{export_ms:.1},\"decode_ms\":{decode_ms:.1},\"cold_project_ms\":{project_ms:.1},\"paragraphs\":{}}}",
    snapshot.len(),
    projection.paragraphs.len(),
  );
}

fn main() {
  let ops: usize = std::env::args()
    .nth(1)
    .and_then(|value| value.parse().ok())
    .unwrap_or(100_000);

  // The content everything converges to: `ops` chars, a paragraph break every
  // 80 chars (realistic paragraph density).
  let char_at = |op_ix: usize| -> char {
    if op_ix % 80 == 79 { '\n' } else { (b'a' + (op_ix % 26) as u8) as char }
  };

  // --- fresh: one bulk insert ------------------------------------------------
  let fresh_start = Instant::now();
  let fresh = loro_schema::new_loro_document("history-growth-fresh").expect("fresh doc");
  let body = loro_schema::body_text(&fresh);
  let content: String = (0..ops).map(char_at).collect();
  let base_len = body.len_unicode();
  body.insert(base_len, &content).expect("bulk insert");
  fresh.commit();
  let fresh_build = fresh_start.elapsed().as_secs_f64() * 1e3;
  let expected_text = loro_schema::body_text(&fresh).to_string();

  let export_start = Instant::now();
  let fresh_snapshot = fresh.export(ExportMode::Snapshot).expect("fresh export");
  let fresh_export = export_start.elapsed().as_secs_f64() * 1e3;

  // --- typed / typed-styled: one commit per char -----------------------------
  let mut legs: Vec<Leg> = vec![Leg {
    name: "fresh",
    snapshot: fresh_snapshot,
    export_ms: fresh_export,
    build_ms: fresh_build,
  }];
  let mut styled_final_frontiers = None;
  for styled in [false, true] {
    let build_start = Instant::now();
    let doc = loro_schema::new_loro_document(if styled { "history-growth-styled" } else { "history-growth-typed" })
      .expect("typed doc");
    let text = loro_schema::body_text(&doc);
    let base = text.len_unicode();
    for op_ix in 0..ops {
      text
        .insert(base + op_ix, &char_at(op_ix).to_string())
        .expect("typed insert");
      if styled && op_ix % 64 == 63 {
        let start = base + op_ix.saturating_sub(40);
        text
          .mark(start..base + op_ix, MARK_HIGHLIGHT_STYLE, LoroValue::I64(1))
          .expect("mark");
      }
      doc.commit();
    }
    let build_ms = build_start.elapsed().as_secs_f64() * 1e3;
    let export_start = Instant::now();
    let snapshot = doc.export(ExportMode::Snapshot).expect("typed export");
    let export_ms = export_start.elapsed().as_secs_f64() * 1e3;
    if styled {
      styled_final_frontiers = Some((doc.state_frontiers(), doc));
      legs.push(Leg { name: "typed-styled", snapshot, export_ms, build_ms });
    } else {
      legs.push(Leg { name: "typed", snapshot, export_ms, build_ms });
    }
  }

  // --- churn: steady-state length, content REWRITTEN 5x (tombstone mass) ----
  // The realistic long-lived-doc shape: blocks get replaced, not appended.
  // Total inserted chars = 5 * ops, 80% deleted along the way; final visible
  // content differs from `expected_text` so it gets its own convergence check.
  churn_legs(ops, &char_at);
  // --- shallow: the typed-styled doc, history GC'd to the final frontier -----
  let (frontiers, styled_doc) = styled_final_frontiers.expect("styled leg ran");
  let export_start = Instant::now();
  let shallow_snapshot = styled_doc
    .export(ExportMode::shallow_snapshot(&frontiers))
    .expect("shallow export");
  let shallow_export = export_start.elapsed().as_secs_f64() * 1e3;
  legs.push(Leg {
    name: "shallow",
    snapshot: shallow_snapshot,
    export_ms: shallow_export,
    build_ms: 0.0,
  });

  // NOTE: `typed`/`typed-styled` body text includes whatever sentinel the
  // schema seeds — identical across legs by construction. The `fresh` text is
  // the reference; styled legs carry marks (same characters).
  for leg in &legs {
    measure_decode(leg.name, &leg.snapshot, leg.build_ms, leg.export_ms, &expected_text);
  }
}

fn churn_legs(ops: usize, char_at: &dyn Fn(usize) -> char) {
    let build_start = Instant::now();
    let doc = loro_schema::new_loro_document("history-growth-churn").expect("churn doc");
    let text = loro_schema::body_text(&doc);
    let base = text.len_unicode();
    let mut churn_ops = 0usize;
    while churn_ops < ops * 5 {
      // Type an 80-char paragraph...
      let cursor = text.len_unicode();
      for op_ix in 0..80 {
        text
          .insert(cursor + op_ix, &char_at(churn_ops + op_ix).to_string())
          .expect("churn insert");
        doc.commit();
      }
      churn_ops += 80;
      // ...then delete 64 of its chars 4 paragraphs back (rewrite churn),
      // keeping the doc length roughly steady at ~5x80 visible chars.
      let len = text.len_unicode();
      if len > base + 400 {
        let del_start = base + ((churn_ops / 80) * 13) % (len - base - 80);
        text.delete(del_start, 64).expect("churn delete");
        doc.commit();
        churn_ops += 1;
      }
    }
    let build_ms = build_start.elapsed().as_secs_f64() * 1e3;
    let export_start = Instant::now();
    let snapshot = doc.export(ExportMode::Snapshot).expect("churn export");
    let export_ms = export_start.elapsed().as_secs_f64() * 1e3;
    let churn_text = loro_schema::body_text(&doc).to_string();
    let frontiers = doc.state_frontiers();
    let shallow_start = Instant::now();
    let churn_shallow = doc.export(ExportMode::shallow_snapshot(&frontiers)).expect("churn shallow export");
    let churn_shallow_export = shallow_start.elapsed().as_secs_f64() * 1e3;
    measure_decode("churn", &snapshot, build_ms, export_ms, &churn_text);
    measure_decode("churn-shallow", &churn_shallow, 0.0, churn_shallow_export, &churn_text);
}
