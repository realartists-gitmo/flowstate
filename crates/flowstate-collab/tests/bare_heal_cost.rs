//! §oom-leads #9 / task #40: the REMOTE-STRUCT-BARE heal cost. A raw peer
//! inserts naked `\n`s straight into the body text (no paragraph record, no
//! style mark — the docx `w:br` / adversarial shape); every import must heal
//! the record-less boundary. The heal used to cost ~2 whole-doc
//! materializations per round (a regional bail + the repair pass's flat
//! re-projection); this pins the work budget so it can only go down.

use std::sync::Arc;

use flowstate_collab::crdt_runtime::CrdtRuntime;
use flowstate_collab::local_write::{
  GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, SplitParagraphIntent, TextAnchor, WriteGate,
};
use flowstate_document::{ParagraphStyle, instrument};

fn new_handle(title: &str) -> (LocalDocHandle, Arc<WriteGate<CrdtRuntime>>) {
  let core = CrdtRuntime::new_empty(title).expect("runtime");
  LocalDocHandle::new(core, LocalWriteConfig::default())
}

/// Seed a multi-paragraph doc through the real write path.
fn seed(handle: &LocalDocHandle, paragraphs: usize) {
  let mut paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
  for i in 0..paragraphs {
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: format!("paragraph {i} body text with some length to it"),
        style_override: None,
      })
      .expect("seed insert");
    handle
      .split_paragraph(SplitParagraphIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        inherited_style: ParagraphStyle::Normal,
      })
      .expect("seed split");
    paragraph = *handle
      .projection()
      .expect("projection")
      .ids
      .paragraph_ids
      .last()
      .expect("last paragraph");
  }
}

/// The bench's REMOTE-STRUCT-BARE shape, N rounds; returns the work delta.
fn run_bare_rounds(gate: &Arc<WriteGate<CrdtRuntime>>, rounds: usize) -> instrument::WorkCounts {
  // Raw peer: forks the canonical doc and edits the body text directly —
  // no flowstate runtime, so no records/marks accompany its boundaries.
  let snapshot = {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
    guard
      .doc()
      .export(loro::ExportMode::Snapshot)
      .expect("snapshot")
  };
  let peer_doc = loro::LoroDoc::new();
  peer_doc
    .import_with(&snapshot, "remote")
    .expect("peer join");
  let body_b = flowstate_document::loro_schema::body_text(&peer_doc);
  let mut peer_vv = peer_doc.state_vv();

  let before = instrument::snapshot();
  for _ in 0..rounds {
    let len = body_b.len_unicode();
    body_b
      .insert(len / 2, "\n")
      .expect("bare structural insert");
    peer_doc.commit();
    let update = peer_doc
      .export(loro::ExportMode::updates(&peer_vv))
      .expect("peer export");
    peer_vv = peer_doc.state_vv();
    gate
      .lock(GateHolder::ImportChunk)
      .expect("gate")
      .import_remote_update(&update)
      .expect("import bare structural");
  }
  instrument::snapshot().since(before)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Diagnostic escape hatch: `cargo test -p flowstate-collab --test
  /// bare_heal_cost -- --ignored --nocapture` with `RUST_LOG=warn` +
  /// `FLOWSTATE_DERIVE_DEBUG=1` prints the ladder decision and any regional
  /// bail per round.
  #[test]
  #[ignore = "diagnostic: run with RUST_LOG=warn FLOWSTATE_DERIVE_DEBUG=1 --nocapture"]
  fn bare_heal_diagnostic() {
    let _ = tracing_subscriber::fmt()
      .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
      .with_writer(std::io::stderr)
      .try_init();
    let (handle, gate) = new_handle("bare-heal-diag");
    seed(&handle, 40);
    let work = run_bare_rounds(&gate, 8);
    eprintln!(
      "bare-heal work over 8 rounds: full_projections={} body_to_delta={} cursor_resolves={}",
      work.full_projections, work.body_to_delta_builds, work.cursor_pos_resolves
    );
  }

  /// §task #40 gate: the per-round heal must not pay whole-doc materializations.
  /// Baseline before the fix: ~1.85 full projections/round — a `loro_id_u128`
  /// LAW MISMATCH (blake3 in `flowstate-document` vs uuid-v5 in `crdt_runtime`) made
  /// every repaired anchor-keyed record unresolvable to the runtime (regional
  /// bail + defect re-report + repair-pass flat re-projection, EVERY round).
  /// After the unified law + the regional anchor-key probe + the neutral-batch
  /// repair skip, the steady state is ZERO full projections.
  #[test]
  fn bare_boundary_heal_work_is_bounded() {
    let (handle, gate) = new_handle("bare-heal-gate");
    seed(&handle, 40);
    let rounds = 8;
    let work = run_bare_rounds(&gate, rounds);
    assert!(
      work.full_projections <= 2,
      "bare-boundary heal paid {} full projections over {rounds} rounds (steady state must be ~0; ~2/round means the heal regressed)",
      work.full_projections
    );
    // Convergence: the healed live projection must equal an independent full
    // materialization, text and styles.
    let live = handle.projection().expect("projection");
    let canonical = {
      let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
      flowstate_document::document_from_loro(guard.doc()).expect("materializes")
    };
    assert_eq!(
      live.paragraphs.len(),
      canonical.paragraphs.len(),
      "live == canonical paragraph count after heals"
    );
    let live_text: Vec<String> = (0..live.paragraphs.len())
      .map(|ix| flowstate_document::paragraph_text(&live, ix))
      .collect();
    let canonical_text: Vec<String> = (0..canonical.paragraphs.len())
      .map(|ix| flowstate_document::paragraph_text(&canonical, ix))
      .collect();
    assert_eq!(live_text, canonical_text, "live == canonical text after heals");
    let live_styles: Vec<_> = live.paragraphs.iter().map(|p| p.style).collect();
    let canonical_styles: Vec<_> = canonical.paragraphs.iter().map(|p| p.style).collect();
    assert_eq!(live_styles, canonical_styles, "live == canonical styles after heals");
  }
}
