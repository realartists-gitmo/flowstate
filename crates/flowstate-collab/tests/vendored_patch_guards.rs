//! §act-eleven C6: workspace guards for the vendored loro-internal checkout
//! patches (flowstate-loro-vendor-patch #4–#7 — the richtext tracker/`id_to_cursor`
//! quadratics). The vendor tree carries its own perf-regression tests, but the
//! vendored crate cannot `cargo test` (workspace-detection + feature conflicts),
//! so those tests NEVER RUN — a Loro upgrade could silently drop a patch with no
//! failing test anywhere. This guard exercises the property through the PUBLIC
//! import path and asserts SCALING (a work ratio), not wall-clock, so it is
//! machine- and build-profile-independent in what it rejects: the unpatched
//! quadratic makes the ratio grow with N (16x at these sizes), the patched
//! amortized-linear behavior keeps it near the size ratio (4x).

use anyhow::Result;
use flowstate_collab::crdt_runtime::CrdtRuntime;
use flowstate_collab::local_write::{GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, TextAnchor};
use std::time::Instant;

/// One peer builds a large single-fragment body; a forked peer splits it with
/// `splits` interleaved single-char inserts (each lands mid-fragment, the
/// pathological shape); the first peer imports the whole divergence as ONE
/// blob, driving the vendored tracker checkout over every split.
fn interleaved_split_import_cost(splits: usize) -> Result<std::time::Duration> {
  let base_len: usize = 64 * 1024;
  let runtime = CrdtRuntime::new_empty("patch-guard")?;
  let (handle, gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
  let projection = handle.projection()?;
  let paragraph = projection.ids.paragraph_ids[0];
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "x".repeat(base_len),
      style_override: None,
    })
    .map_err(|error| anyhow::anyhow!("seed insert rejected: {error:?}"))?;

  // Forked peer: same history, then `splits` mid-fragment inserts.
  let fork_bytes = {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::Snapshot)
      .expect("snapshot export")
  };
  let peer = CrdtRuntime::new_empty("patch-guard-peer")?;
  let (peer_handle, peer_gate) = LocalDocHandle::new(peer, LocalWriteConfig::default());
  {
    let mut guard = peer_gate
      .lock(GateHolder::ImportChunk)
      .expect("gate healthy");
    guard.import_remote_update(&fork_bytes)?
  };
  // Bidirectional base sync: a `new_empty` peer mints its OWN sentinel init
  // ops, so the importer must see them too before the timed leg — otherwise
  // the final convergence assert compares different baselines (the known
  // one-way-exchange trap from the P3-deep validation).
  {
    let peer_init = {
      let guard = peer_gate
        .lock(GateHolder::ExportUpdates)
        .expect("gate healthy");
      guard
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("peer init export")
    };
    let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&peer_init)?
  };
  let peer_vv_before = {
    let guard = peer_gate
      .lock(GateHolder::ExportUpdates)
      .expect("gate healthy");
    guard.doc().state_vv()
  };
  let peer_projection = peer_handle.projection()?;
  let peer_paragraph = *peer_projection
    .ids
    .paragraph_ids
    .last()
    .expect("peer paragraph");
  let stride = base_len / (splits + 1);
  for split_ix in 0..splits {
    peer_handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(peer_paragraph, (split_ix + 1) * stride + split_ix),
        text: "y".into(),
        style_override: None,
      })
      .map_err(|error| anyhow::anyhow!("split insert {split_ix} rejected: {error:?}"))?;
  }
  let divergence = {
    let guard = peer_gate
      .lock(GateHolder::ExportUpdates)
      .expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&peer_vv_before))
      .expect("delta export")
  };

  let start = Instant::now();
  {
    let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&divergence)?
  };
  let elapsed = start.elapsed();

  // Convergence stays the point: both peers must agree byte-for-byte.
  let local = {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate healthy");
    flowstate_document::loro_schema::body_text(guard.doc()).to_string()
  };
  let remote = {
    let guard = peer_gate
      .lock(GateHolder::ExportUpdates)
      .expect("gate healthy");
    flowstate_document::loro_schema::body_text(guard.doc()).to_string()
  };
  assert_eq!(local, remote, "interleaved-split import diverged at splits={splits}");
  Ok(elapsed)
}

/// §act-eleven A11.5 net: styled-divergence cost at `edits` ops. One peer types
/// `edits` single-char inserts at cycling positions, marking a short (20-char)
/// same-key range every 8th op — the shape that accumulates style-range
/// BOUNDARIES (2 per mark) so later inserts keep landing on the
/// `StyleRangeMap` boundary-intersection path. Returns (local edit time,
/// reconnect import time on the offline peer).
fn styled_divergence_cost(edits: usize) -> (std::time::Duration, std::time::Duration) {
  let a = loro::LoroDoc::new();
  a.set_peer_id(1).expect("peer id");
  let b = loro::LoroDoc::new();
  b.set_peer_id(2).expect("peer id");
  let ta = a.get_text("t");
  ta.insert(0, &"base text ".repeat(200)).expect("seed");
  a.commit();
  b.import(&a.export(loro::ExportMode::all_updates()).expect("export"))
    .expect("base import");
  let b_offline_vv = a.oplog_vv();
  b.get_text("t").insert(0, "b").expect("divergence seed");
  b.commit();

  let edit_start = Instant::now();
  for edit_ix in 0..edits {
    let pos = (edit_ix * 13) % ta.len_unicode();
    ta.insert(pos, "a").expect("insert");
    if edit_ix % 8 == 0 {
      let end = (pos + 20).min(ta.len_unicode());
      if pos + 1 < end {
        ta.mark(pos..end, "bold", true).expect("mark");
      }
    }
    a.commit();
  }
  let edit_time = edit_start.elapsed();

  let blob = a
    .export(loro::ExportMode::updates(&b_offline_vv))
    .expect("divergence export");
  let import_start = Instant::now();
  b.import(&blob).expect("reconnect import");
  let import_time = import_start.elapsed();
  a.import(
    &b.export(loro::ExportMode::updates(&a.oplog_vv()))
      .expect("b export"),
  )
  .expect("a import");
  b.import(
    &a.export(loro::ExportMode::updates(&b.oplog_vv()))
      .expect("a export"),
  )
  .expect("b import");
  assert_eq!(ta.to_string(), b.get_text("t").to_string(), "styled divergence failed to converge");
  (edit_time, import_time)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// §stylemap pass (2026-07-09, follow-on to act-eleven A11.5): pins the
  /// LAYERED `StyleValue` representation in the vendored `style_range_map.rs`
  /// (persistent `im::OrdSet` base + small owned overlay + one-side-reuse
  /// intersections + `max_common` for the keystroke path). Measured on this
  /// shape at 16k→48k ops (3× input):
  /// * EDIT (per-keystroke, the user-felt path): pre-pass 91.7× ratio
  ///   (5.39s absolute at 48k) → **15.4× (0.96s)** — the boundary-set copy
  ///   quadratic's constant is gone; the residual super-linearity is the
  ///   per-boundary intersection compare walk.
  /// * IMPORT (reconnect): ratio 87× (3.5s at 48k; pre-pass 30.9×/1.2s — the
  ///   absolute regression on THIS adversarial shape is the accepted cost of
  ///   the edit win). The import curve is dominated by the annotate WALK
  ///   (every covered elem must record membership — 9M visits measured at
  ///   48k), which NO per-elem representation can beat.
  ///
  /// Both residuals have one fix: a lazy-tag range structure (annotate as an
  /// O(log) subtree tag instead of a per-elem walk) — the scheduled next
  /// `StyleRangeMap` pass. When it lands, tighten BOTH bounds to < 8.
  #[test]
  fn styled_divergence_scales_within_layered_representation_bounds() {
    let _ = styled_divergence_cost(1_000); // warm
    let (small_edit, small_import) = styled_divergence_cost(16_000);
    let (large_edit, large_import) = styled_divergence_cost(48_000);
    let edit_ratio = large_edit.as_secs_f64() / small_edit.as_secs_f64().max(0.001);
    let import_ratio = large_import.as_secs_f64() / small_import.as_secs_f64().max(0.001);
    // Bounds = measured-at-landing + headroom. The pre-pass representation
    // trips the edit bound at ~92×; a lost overlay/one-side-reuse patch trips
    // it too. The import bound catches regression past the documented walk
    // quadratic.
    assert!(
      edit_ratio < 30.0 && import_ratio < 140.0,
      "styled-divergence scaling for 3x input: edit {edit_ratio:.1}x (small={small_edit:?}, large={large_edit:?}, bound 30), import {import_ratio:.1}x (small={small_import:?}, large={large_import:?}, bound 140)"
    );
  }

  #[test]
  fn interleaved_split_checkout_scales_subquadratically() -> Result<()> {
    // Warm once so allocator/lazy-init noise doesn't pollute the small run.
    let _ = interleaved_split_import_cost(16)?;
    let small = interleaved_split_import_cost(128)?;
    let large = interleaved_split_import_cost(512)?;
    // Size ratio 4x. Patched (amortized-linear) checkout ⇒ time ratio ≈ 4;
    // the unpatched quadratic ⇒ ≈ 16. The bound sits between with margin for
    // noise; on a loaded machine the ABSOLUTE floor also protects against a
    // degenerate tiny `small` inflating the ratio.
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.0005);
    assert!(
      ratio < 10.0,
      "interleaved-split import scaled {ratio:.1}x for a 4x input (small={small:?}, large={large:?}) — a vendored checkout patch (loro-vendor-patch #4–#7) has regressed"
    );
    Ok(())
  }

  /// §bimodal-undo fix (vendor patch #23 + the vendored generic-btree
  /// `next_leaf_matching`): two peers each PREPEND `n` chars concurrently, then
  /// a checkout crosses from one tip to the other. The LCA is the ROOT, so the
  /// tracker rebuild feeds branch A, RETREATS it wholesale (marks every A
  /// element FUTURE), then feeds branch B — whose inserts land at position 0
  /// with the entire retreated A block to their right. Upstream's linear
  /// `origin_right` scan made every such insert O(block) ⇒ O(n²) per rebuild:
  /// the 80-195s dirty-history undo regime, load/merge-boundary dependent. The
  /// probe + `non_future_num` cache jump makes it O(n log n).
  fn concurrent_prepend_checkout_cost(n: usize) -> (std::time::Duration, String) {
    let a = loro::LoroDoc::new();
    a.set_peer_id(1).expect("peer id");
    let ta = a.get_text("t");
    for _ in 0..n {
      ta.insert(0, "a").expect("A prepend");
      a.commit();
    }
    let b = loro::LoroDoc::new();
    b.set_peer_id(2).expect("peer id");
    let tb = b.get_text("t");
    for _ in 0..n {
      tb.insert(0, "b").expect("B prepend");
      b.commit();
    }
    let a_tip = a.state_frontiers();
    let b_tip = b.state_frontiers();
    a.import(
      &b.export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("B export"),
    )
    .expect("A imports B");
    let merged = {
      // Reference merged text from an independent replica (convergence oracle).
      let c = loro::LoroDoc::new();
      c.import(
        &a.export(loro::ExportMode::updates(&loro::VersionVector::default()))
          .expect("A export"),
      )
      .expect("C imports merged");
      c.get_text("t").to_string()
    };
    a.checkout(&a_tip).expect("checkout A tip");
    let t = Instant::now();
    // The timed leg: A-tip → B-tip crosses the root LCA — the tracker feeds
    // both branches and the second faces the first as one giant future run.
    a.checkout(&b_tip).expect("checkout B tip");
    let cost = t.elapsed();
    a.checkout_to_latest();
    assert_eq!(
      a.get_text("t").to_string(),
      merged,
      "checkout round-trip must not corrupt the merged text"
    );
    (cost, merged)
  }

  #[test]
  fn concurrent_prepend_checkout_scales_subquadratically() {
    let _ = concurrent_prepend_checkout_cost(256); // warm
    let (small, small_text) = concurrent_prepend_checkout_cost(4_000);
    let (large, large_text) = concurrent_prepend_checkout_cost(12_000);
    assert_eq!(small_text.len(), 8_000);
    assert_eq!(large_text.len(), 24_000);
    // Size ratio 3x. Near-linear ⇒ time ratio ≈ 3-4; the unpatched quadratic
    // ⇒ ≥ 9 (measured far higher — the future-run Vec collection compounds).
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.0005);
    assert!(
      ratio < 8.0,
      "concurrent-prepend checkout scaled {ratio:.1}x for a 3x input (small={small:?}, large={large:?}) — the future-run probe/jump (vendor patch #23, vendored generic-btree next_leaf_matching) has regressed"
    );
  }
}
