//! Headless collab profiler bench — drives the real field hot paths through the actual
//! `CrdtRuntime` + `LocalDocHandle` write path on a real `.docx`, and dumps the same
//! hotpath tables `cargo survey` does. Complements `hotpath_bench` (which measures the
//! COLD reopen projection); this measures the WARM steady-state operations:
//!   OPEN         — docx -> projection -> canonical `CrdtRuntime`.
//!   IMPORT       — receiving-peer `import_remote_update` (a concurrent peer's edit).
//!   KEYSTROKE    — one `InsertTextIntent` through the write authority.
//!   MASS-RESTYLE — one `SetParagraphStylesIntent` over EVERY paragraph (select-all).
//!   UNDO / REDO  — revert/replay the mass restyle.
//!
//! Run: `cargo run --release --features hotpath-alloc --bin collab_bench -- <FILE.docx> [ROUNDS]`.

use std::alloc::{GlobalAlloc, Layout};
use std::time::Instant;

use flowstate_collab::crdt_runtime::CrdtRuntime;
use flowstate_collab::local_write::{GateHolder, LocalDocHandle, LocalWriteConfig};
use flowstate_document::block_ix_scan_count;
use flowstate_document::instrument;
use gpui_flowtext::{
  DeleteRangeIntent, InsertTextIntent, JoinParagraphsIntent, ParagraphStyle, ReplaceMatch, ReplaceMatchesIntent, SetParagraphStylesIntent,
  SplitParagraphIntent, TextAnchor,
};

/// Print the algorithmic-work counters accumulated during `label`'s op — the
/// §perf-heaven tripwires (whole-body delta builds, block-index scans, full
/// projections, cursor resolves). In `--release` (where these benches run) the
/// debug audit is OFF, so a warm op should show ZERO whole-body builds and ZERO
/// block-index scans; a non-zero number is a T2/T3 regression made visible.
fn report_work(label: &str, before: instrument::WorkCounts, before_scans: u64) {
  let work = instrument::snapshot().since(before);
  let scans = block_ix_scan_count().saturating_sub(before_scans);
  eprintln!(
    "  {label:<12} work: body_to_delta={} block_ix_scans={} full_projections={} cursor_resolves={}",
    work.body_to_delta_builds, scans, work.full_projections, work.cursor_pos_resolves,
  );
}

struct FlowstateAllocator;

impl Default for FlowstateAllocator {
  fn default() -> Self {
    Self
  }
}

// SAFETY: forwards every call unchanged to mimalloc, which upholds the contract.
unsafe impl GlobalAlloc for FlowstateAllocator {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    // SAFETY: layout contract upheld by the caller; forwarded unchanged.
    unsafe { mimalloc::MiMalloc.alloc(layout) }
  }
  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    // SAFETY: ptr/layout originate from this allocator; forwarded unchanged.
    unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
  }
}

#[cfg(not(feature = "hotpath-alloc"))]
#[global_allocator]
static GLOBAL: FlowstateAllocator = FlowstateAllocator;

#[hotpath::main(allocator = FlowstateAllocator)]
fn main() {
  // Surface `tracing::warn!` bail diagnostics (regional-rematerializer bails
  // etc.) on stderr when RUST_LOG is set — they are invisible otherwise.
  if std::env::var_os("RUST_LOG").is_some() {
    let _ = tracing_subscriber::fmt()
      .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
      .with_writer(std::io::stderr)
      .try_init();
  }
  let mut args = std::env::args().skip(1);
  let Some(path) = args
    .next()
    .or_else(|| std::env::var("FLOWSTATE_BENCH_FIXTURE").ok())
  else {
    eprintln!(
      "usage: collab_bench <FILE.docx | synthetic:<kind>:<paragraphs>> [ROUNDS]   (or set FLOWSTATE_BENCH_FIXTURE)\n  \
       synthetic kinds: equations (default), mixed, text — e.g. synthetic:equations:4000"
    );
    std::process::exit(2);
  };
  let rounds: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(20);

  // OPEN: source -> projection -> canonical runtime (body decoded, as after a live open).
  // A `synthetic:*` selector builds an equation/object-bearing fixture in memory
  // (§perf-heaven T7 — the corpus has no equation docs); otherwise convert a docx.
  let open = Instant::now();
  let (projection, paragraph_count) = flowstate_corpus::fixtures::from_selector(&path).unwrap_or_else(|| {
    let (projection, report) = flowstate_docx::convert_docx_to_document(&path).expect("convert docx");
    let count = report.paragraphs_imported;
    (projection, count)
  });
  let core = CrdtRuntime::from_document_projection(&projection, "collab-bench").expect("runtime");
  eprintln!(
    "OPEN            {:8.1} ms   ({paragraph_count} paragraphs)",
    open.elapsed().as_secs_f64() * 1000.0
  );

  // A concurrent peer B forks the shared history and will feed edits to A.
  let doc_b = core.doc().fork();
  doc_b.set_peer_id(0x00B0_B0B0).expect("distinct peer id");
  let mut peer_vv = doc_b.state_vv();
  let body_b = flowstate_document::loro_schema::body_text(&doc_b);

  let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
  let live = handle.projection().expect("projection");
  let paragraph_ids = live.ids.paragraph_ids.clone();
  let first = *paragraph_ids.first().expect("at least one paragraph");

  // IMPORT: receiving-peer cost — peer B prepends a char, A imports the delta. This is
  // the field "another peer is typing" path (derive projection events on the receiver).
  // §perf-heaven T1: the receive path is regional (derive_body_projection_events), so
  // in --release the work report should show ZERO whole-body deltas per import; a
  // non-zero number means the regional receive regressed to a full re-decode.
  let import_work = instrument::snapshot();
  let import_scans = block_ix_scan_count();
  let mut import_ms = 0.0;
  for _ in 0..rounds {
    body_b.insert(1, "x").expect("peer insert");
    doc_b.commit();
    let update = doc_b
      .export(loro::ExportMode::updates(&peer_vv))
      .expect("peer export");
    peer_vv = doc_b.state_vv();
    let t = Instant::now();
    gate
      .lock(GateHolder::ImportChunk)
      .expect("gate")
      .import_remote_update(&update)
      .expect("import");
    import_ms += t.elapsed().as_secs_f64() * 1000.0;
  }
  eprintln!(
    "IMPORT          {:8.1} ms   ({rounds} rounds, {:.2} ms/round)",
    import_ms,
    import_ms / rounds as f64
  );
  report_work("IMPORT", import_work, import_scans);

  // KEYSTROKE: local insert intent through the write authority.
  let key_work = instrument::snapshot();
  let key_scans = block_ix_scan_count();
  let mut key_ms = 0.0;
  for _ in 0..rounds {
    let t = Instant::now();
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(first, 0),
        text: "y".to_string(),
        style_override: None,
      })
      .expect("insert_text");
    key_ms += t.elapsed().as_secs_f64() * 1000.0;
  }
  eprintln!(
    "KEYSTROKE       {:8.1} ms   ({rounds} rounds, {:.2} ms/keystroke)",
    key_ms,
    key_ms / rounds as f64
  );
  report_work("KEYSTROKE", key_work, key_scans);

  // MASS-RESTYLE: select-all paragraph-style change (one batched intent over every para).
  let mass_work = instrument::snapshot();
  let mass_scans = block_ix_scan_count();
  let mass = Instant::now();
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: paragraph_ids.to_vec(),
      style: ParagraphStyle::Custom(2),
    })
    .expect("set_paragraph_styles");
  eprintln!(
    "MASS-RESTYLE    {:8.1} ms   ({} paragraphs)",
    mass.elapsed().as_secs_f64() * 1000.0,
    paragraph_ids.len()
  );
  report_work("MASS-RESTYLE", mass_work, mass_scans);

  // §oom-leads #4: the structural rounds are COUNTER nets (fallback/whole-body
  // tripwires), and REMOTE-STRUCT is a known ~340ms/op cliff until the remote
  // regional fix lands — they'd swamp the hotpath share-gate's profile, so
  // gate runs skip them (heaven.sh sets the env).
  let skip_structural = std::env::var_os("FLOWSTATE_BENCH_SKIP_STRUCTURAL").is_some();
  if !skip_structural {
    // STRUCTURAL (§oom-leads #4): split/join through the write authority plus a
    // REMOTE structural import (peer B inserts a paragraph break). The §6-R
    // regional rematerializer must serve all of these — `full_projections`
    // in the work line is the fallback tripwire: every count here is a whole-doc
    // rebuild (~warm-project cost) where a region-sized patch was intended.
    let structural_work = instrument::snapshot();
    let structural_scans = block_ix_scan_count();
    let mut structural_ms = 0.0;
    for round in 0..rounds {
      let live = handle.projection().expect("projection");
      let mid_ix = live.ids.paragraph_ids.len() / 2 + (round % 3);
      let mid = live.ids.paragraph_ids[mid_ix];
      let t = Instant::now();
      handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(mid, 1),
          inherited_style: ParagraphStyle::Normal,
        })
        .expect("split_paragraph");
      structural_ms += t.elapsed().as_secs_f64() * 1000.0;
      let live = handle.projection().expect("projection");
      let first_id = live.ids.paragraph_ids[mid_ix];
      let second_id = live.ids.paragraph_ids[mid_ix + 1];
      let t = Instant::now();
      handle
        .join_paragraphs(JoinParagraphsIntent {
          first: first_id,
          second: second_id,
        })
        .expect("join_paragraphs");
      structural_ms += t.elapsed().as_secs_f64() * 1000.0;
    }
    eprintln!(
      "STRUCTURAL      {:8.1} ms   ({rounds} split+join rounds, {:.2} ms/op)",
      structural_ms,
      structural_ms / (rounds as f64 * 2.0)
    );
    report_work("STRUCTURAL", structural_work, structural_scans);

    // REMOTE-STRUCTURAL (§oom-leads #4): peer B is a REAL runtime driving the
    // actual split_paragraph write path (registry records + sentinel mark — the
    // true wire shape), then A imports the delta. The earlier synthetic rounds
    // wrote bare/naked sentinels, which never carry registry records and forced
    // the region resolver into permanent full-rebuild — realistic for the docx
    // `w:br` shape (kept below as REMOTE-STRUCT-BARE) but NOT for peer splits.
    let peer_runtime = CrdtRuntime::new_empty("collab-bench-peer").expect("peer runtime");
    let (peer_handle, peer_gate) = LocalDocHandle::new(peer_runtime, LocalWriteConfig::default());
    {
      // Bidirectional init exchange (the known one-way sentinel trap).
      let init_a = gate
        .lock(GateHolder::ExportUpdates)
        .expect("gate")
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("A init export");
      peer_gate
        .lock(GateHolder::ImportChunk)
        .expect("peer gate")
        .import_remote_update(&init_a)
        .expect("peer imports A");
      let init_b = peer_gate
        .lock(GateHolder::ExportUpdates)
        .expect("peer gate")
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("B init export");
      gate
        .lock(GateHolder::ImportChunk)
        .expect("gate")
        .import_remote_update(&init_b)
        .expect("A imports B init")
    };
    let mut peer_handle_vv = {
      let guard = peer_gate
        .lock(GateHolder::ExportUpdates)
        .expect("peer gate");
      guard.doc().state_vv()
    };
    let remote_structural_work = instrument::snapshot();
    let remote_structural_scans = block_ix_scan_count();
    let mut remote_structural_ms = 0.0;
    for round in 0..rounds {
      let peer_projection = peer_handle.projection().expect("peer projection");
      let mid_ix = peer_projection.ids.paragraph_ids.len() / 2 + (round % 3);
      let mid = peer_projection.ids.paragraph_ids[mid_ix];
      peer_handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(mid, 1),
          inherited_style: ParagraphStyle::Normal,
        })
        .expect("peer split");
      let update = {
        let guard = peer_gate
          .lock(GateHolder::ExportUpdates)
          .expect("peer gate");
        let update = guard
          .doc()
          .export(loro::ExportMode::updates(&peer_handle_vv))
          .expect("peer export");
        peer_handle_vv = guard.doc().state_vv();
        update
      };
      let t = Instant::now();
      gate
        .lock(GateHolder::ImportChunk)
        .expect("gate")
        .import_remote_update(&update)
        .expect("import real split");
      remote_structural_ms += t.elapsed().as_secs_f64() * 1000.0;
    }
    eprintln!(
      "REMOTE-SPLIT-REAL {:8.1} ms   ({rounds} rounds, {:.2} ms/round)",
      remote_structural_ms,
      remote_structural_ms / rounds as f64
    );
    report_work("REMOTE-SPLIT-REAL", remote_structural_work, remote_structural_scans);

    // REMOTE-SPLIT-CONCUR: same real peer split, but A commits a local keystroke
    // AFTER B's export and BEFORE the import, so the imported chunk is CONCURRENT
    // with A's state — the diff arrives as origin `DiffMode::Import` (the field
    // shape for two people typing) instead of `ImportGreaterUpdates` (the quiet
    // strictly-ahead shape REMOTE-SPLIT-REAL measures). Exercises the vendored
    // Import-origin per-key registry map diff.
    let concurrent_work = instrument::snapshot();
    let concurrent_scans = block_ix_scan_count();
    let mut concurrent_ms = 0.0;
    for round in 0..rounds {
      let peer_projection = peer_handle.projection().expect("peer projection");
      let mid_ix = peer_projection.ids.paragraph_ids.len() / 2 + (round % 3);
      let mid = peer_projection.ids.paragraph_ids[mid_ix];
      peer_handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(mid, 1),
          inherited_style: ParagraphStyle::Normal,
        })
        .expect("peer split");
      let update = {
        let guard = peer_gate
          .lock(GateHolder::ExportUpdates)
          .expect("peer gate");
        let update = guard
          .doc()
          .export(loro::ExportMode::updates(&peer_handle_vv))
          .expect("peer export");
        peer_handle_vv = guard.doc().state_vv();
        update
      };
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(first, 0),
          text: "z".to_string(),
          style_override: None,
        })
        .expect("concurrent local keystroke");
      let t = Instant::now();
      gate
        .lock(GateHolder::ImportChunk)
        .expect("gate")
        .import_remote_update(&update)
        .expect("import concurrent split");
      concurrent_ms += t.elapsed().as_secs_f64() * 1000.0;
    }
    eprintln!(
      "REMOTE-SPLIT-CONCUR {:8.1} ms   ({rounds} rounds, {:.2} ms/round)",
      concurrent_ms,
      concurrent_ms / rounds as f64
    );
    report_work("REMOTE-SPLIT-CONCUR", concurrent_work, concurrent_scans);

    // ---- §mass-op collab rounds (field complaint: whole-doc restyles/deletes/
    // replaces and undo/redo "freeze for a little, especially on collab").
    // Each round times the RECEIVER's import of one mass chunk, or the local
    // undo AFTER remote chatter (the recorded-inverse staleness case). ---------
    let mut import_peer_chunk = |label: &str| {
      let update = {
        let guard = peer_gate
          .lock(GateHolder::ExportUpdates)
          .expect("peer gate");
        let update = guard
          .doc()
          .export(loro::ExportMode::updates(&peer_handle_vv))
          .expect("peer export");
        peer_handle_vv = guard.doc().state_vv();
        update
      };
      let work = instrument::snapshot();
      let scans = block_ix_scan_count();
      let t = Instant::now();
      let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
      let lock_ms = t.elapsed().as_secs_f64() * 1000.0;
      let t_call = Instant::now();
      let events = guard
        .import_remote_update(&update)
        .expect("import peer mass chunk");
      let call_ms = t_call.elapsed().as_secs_f64() * 1000.0;
      let t_drop = Instant::now();
      drop(events);
      drop(guard);
      let drop_ms = t_drop.elapsed().as_secs_f64() * 1000.0;
      eprintln!(
        "{label} {:8.1} ms   (one mass import: lock {lock_ms:.1} + call {call_ms:.1} + drop {drop_ms:.1})",
        t.elapsed().as_secs_f64() * 1000.0
      );
      report_work(label, work, scans);
    };

    // REMOTE-MASS-RESTYLE: B restyles EVERY paragraph; A imports one chunk.
    let peer_paragraphs = peer_handle
      .projection()
      .expect("peer projection")
      .ids
      .paragraph_ids
      .clone();
    peer_handle
      .set_paragraph_styles(SetParagraphStylesIntent {
        paragraphs: peer_paragraphs.to_vec(),
        style: ParagraphStyle::Custom(3),
      })
      .expect("peer mass restyle");
    import_peer_chunk("REMOTE-MASS-RESTYLE");

    // REMOTE-UNDO-MASS: B undoes the mass restyle; A imports the inverse chunk.
    peer_handle.apply_undo().expect("peer undo of mass restyle");
    import_peer_chunk("REMOTE-UNDO-MASS  ");

    // REMOTE-REPLACE-ALL: B replaces the first char of every non-empty paragraph
    // (a same-shape proxy for find&replace-all); A imports one chunk.
    {
      let peer_projection = peer_handle.projection().expect("peer projection");
      let mut matches = Vec::new();
      let mut paragraph_ix = 0usize;
      for block in peer_projection.blocks.iter() {
        if let flowstate_document::Block::Paragraph(paragraph) = block {
          let id = peer_projection.ids.paragraph_ids[paragraph_ix];
          if paragraph.runs.iter().map(|run| run.len).sum::<usize>() >= 3 {
            matches.push(ReplaceMatch {
              start: TextAnchor::new(id, 0),
              end: TextAnchor::new(id, 1),
              styles: None,
            });
          }
          paragraph_ix += 1;
        }
      }
      let match_count = matches.len();
      peer_handle
        .replace_matches(ReplaceMatchesIntent {
          matches,
          replacement: "Q".to_string(),
        })
        .expect("peer replace-all");
      eprintln!("  (replace-all touched {match_count} paragraphs)");
    };
    import_peer_chunk("REMOTE-REPLACE-ALL");

    // REMOTE-MASS-DELETE: B deletes a quarter of the document in one intent; A
    // imports one chunk.
    {
      let peer_projection = peer_handle.projection().expect("peer projection");
      let ids = &peer_projection.ids.paragraph_ids;
      let start = TextAnchor::new(ids[ids.len() / 4], 0);
      let end = TextAnchor::new(ids[ids.len() / 2], 0);
      peer_handle
        .delete_range(DeleteRangeIntent { start, end })
        .expect("peer mass delete")
    };
    import_peer_chunk("REMOTE-MASS-DELETE");

    // UNDO-AFTER-REMOTE: the receiver-side staleness case — A commits a mass
    // restyle, ONE remote char arrives, then A hits Ctrl-Z. The remote frontier
    // advance invalidates the recorded-inverse fast path today, so this measures
    // the slow-path cliff the rebase fix must close (gate: compare to the tail
    // UNDO, which stays fast-path).
    {
      let live = handle.projection().expect("projection");
      handle
        .set_paragraph_styles(SetParagraphStylesIntent {
          paragraphs: live.ids.paragraph_ids.to_vec(),
          style: ParagraphStyle::Custom(4),
        })
        .expect("mass restyle before remote chatter");
      let peer_first = peer_handle
        .projection()
        .expect("peer projection")
        .ids
        .paragraph_ids[0];
      peer_handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(peer_first, 0),
          text: "r".to_string(),
          style_override: None,
        })
        .expect("peer chatter keystroke");
      import_peer_chunk("REMOTE-CHATTER    ");
      let work = instrument::snapshot();
      let scans = block_ix_scan_count();
      let t = Instant::now();
      handle.apply_undo().expect("undo after remote chatter");
      eprintln!(
        "UNDO-AFTER-REMOTE {:8.1} ms   (select-all restyle: v2 per-coordinate rebase keeps the fast path)",
        t.elapsed().as_secs_f64() * 1000.0
      );
      report_work("UNDO-AFTER-REMOTE", work, scans);
    };

    // UNDO-AFTER-REMOTE-PARTIAL (§oom-leads #9): A restyles the BOTTOM half, B's
    // chatter lands at the TOP — strictly before the recorded hull, so the
    // recorded-inverse REBASE keeps the fast path armed. Gate: this undo must be
    // O(change) fast (same order as a no-chatter undo), NOT the slow path.
    {
      let live = handle.projection().expect("projection");
      let bottom_half: Vec<_> = live
        .ids
        .paragraph_ids
        .iter()
        .skip(live.ids.paragraph_ids.len() / 2)
        .copied()
        .collect();
      handle
        .set_paragraph_styles(SetParagraphStylesIntent {
          paragraphs: bottom_half,
          style: ParagraphStyle::Custom(5),
        })
        .expect("partial mass restyle");
      let peer_first = peer_handle
        .projection()
        .expect("peer projection")
        .ids
        .paragraph_ids[1];
      peer_handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(peer_first, 0),
          text: "t".to_string(),
          style_override: None,
        })
        .expect("peer chatter above hull");
      import_peer_chunk("REMOTE-CHATTER-TOP");
      let work = instrument::snapshot();
      let scans = block_ix_scan_count();
      let t = Instant::now();
      handle.apply_undo().expect("undo after top chatter");
      eprintln!(
        "UNDO-AFTER-REMOTE-PARTIAL {:8.1} ms   (rebase keeps the fast path)",
        t.elapsed().as_secs_f64() * 1000.0
      );
      report_work("UNDO-AFTER-REMOTE-PARTIAL", work, scans);
    };

    // KEYSTROKE-UNDO-AFTER-CHATTER (§oom-leads #9 v2): the single most common
    // field freeze — type a char, partner types ABOVE you, Ctrl-Z. The keystroke
    // capture + hull-disjoint rebase must keep this O(change); before the
    // capture landed this was the uncaptured slow path (~2.6s on this doc with
    // dirty history). Gate: same order as a no-chatter undo, zero fallbacks.
    {
      let live = handle.projection().expect("projection");
      let deep_ix = live.ids.paragraph_ids.len() / 2;
      let deep = live.ids.paragraph_ids[deep_ix];
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(deep, 0),
          text: "k".to_string(),
          style_override: None,
        })
        .expect("deep keystroke");
      let peer_first = peer_handle
        .projection()
        .expect("peer projection")
        .ids
        .paragraph_ids[1];
      peer_handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(peer_first, 0),
          text: "t".to_string(),
          style_override: None,
        })
        .expect("peer chatter above hull");
      import_peer_chunk("REMOTE-CHATTER-TOP2");
      let work = instrument::snapshot();
      let scans = block_ix_scan_count();
      let t = Instant::now();
      handle.apply_undo().expect("keystroke undo after chatter");
      eprintln!(
        "KEYSTROKE-UNDO-AFTER-CHATTER {:8.1} ms   (captured keystroke + rebase keep the fast path)",
        t.elapsed().as_secs_f64() * 1000.0
      );
      report_work("KEYSTROKE-UNDO-AFTER-CHATTER", work, scans);
    };

    // REMOTE-STRUCT-BARE: naked '\n' with no record/mark — the docx `w:br` /
    // adversarial shape; measures the repair/heal story. Skippable so profiling
    // runs can isolate the REAL-split rounds (its per-round cost COMPOUNDS while
    // record-less boundaries accumulate un-healed).
    if std::env::var_os("FLOWSTATE_BENCH_SKIP_BARE").is_none() {
      let bare_work = instrument::snapshot();
      let bare_scans = block_ix_scan_count();
      let mut bare_ms = 0.0;
      for _ in 0..rounds {
        let len = body_b.len_unicode();
        body_b
          .insert(len / 2, "\n")
          .expect("bare structural insert");
        doc_b.commit();
        let update = doc_b
          .export(loro::ExportMode::updates(&peer_vv))
          .expect("peer export");
        peer_vv = doc_b.state_vv();
        let t = Instant::now();
        gate
          .lock(GateHolder::ImportChunk)
          .expect("gate")
          .import_remote_update(&update)
          .expect("import bare structural");
        bare_ms += t.elapsed().as_secs_f64() * 1000.0;
      }
      eprintln!(
        "REMOTE-STRUCT-BARE {:8.1} ms   ({rounds} rounds, {:.2} ms/round)",
        bare_ms,
        bare_ms / rounds as f64
      );
      report_work("REMOTE-STRUCT-BARE", bare_work, bare_scans);
    }
  }

  // UNDO / REDO the mass restyle.
  let undo = Instant::now();
  handle.apply_undo().expect("undo");
  eprintln!("UNDO            {:8.1} ms", undo.elapsed().as_secs_f64() * 1000.0);
  let redo = Instant::now();
  handle.apply_redo().expect("redo");
  eprintln!("REDO            {:8.1} ms", redo.elapsed().as_secs_f64() * 1000.0);

  eprintln!("collab_bench: done — tables below.");
}
