//! §perf-heaven T8.18 — the retained import calculator's eager-warm
//! (`LoroDoc::warm_import_diff_calculator`) pre-builds the per-container trackers
//! so the FIRST remote import of a session reuses the built `id_to_cursor` index
//! instead of constructing it cold on the receive path.
//!
//! Warming is a pure PERFORMANCE precomputation: `start_tracking` re-validates
//! each tracker on every real import and rebuilds it if it does not cover the
//! requested version, so a warmed peer MUST converge to exactly the same state as
//! a cold peer that applied the identical operations. This test pins that
//! invariant (the convergence fuzz separately proves the import path itself is
//! unchanged by the additive vendored method).

#[cfg(test)]
mod tests {
use loro::{ExportMode, LoroDoc};

#[test]
fn warm_import_diff_calculator_preserves_convergence() {
  let src = LoroDoc::new();
  src.set_peer_id(1).unwrap();
  let text = src.get_text("text");
  text.insert(0, "hello").unwrap();
  src.commit();
  let first_update = src.export(ExportMode::all_updates()).unwrap();
  let first_vv = src.oplog_vv();

  text.insert(5, " world").unwrap();
  src.commit();
  let second_update = src.export(ExportMode::updates(&first_vv)).unwrap();

  // A forward-extension import AFTER warming converges to src (and a second warm
  // is idempotent).
  let dst = LoroDoc::new();
  dst.set_peer_id(2).unwrap();
  dst.import(&first_update).unwrap();
  dst.inner().warm_import_diff_calculator();
  dst.inner().warm_import_diff_calculator();
  dst.import(&second_update).unwrap();
  assert_eq!(dst.get_deep_value(), src.get_deep_value());
  assert_eq!(dst.oplog_frontiers(), src.oplog_frontiers());

  // A CONCURRENT import after warming must match a COLD peer that applied the
  // identical ops (same peer id ⇒ same op ids; the only difference is the warm).
  let make_peer = |warm: bool| {
    let peer = LoroDoc::new();
    peer.set_peer_id(3).unwrap();
    peer.import(&first_update).unwrap();
    if warm {
      peer.inner().warm_import_diff_calculator();
    }
    peer.get_text("text").insert(5, "!").unwrap();
    peer.commit();
    peer.import(&second_update).unwrap();
    peer
  };
  let warmed = make_peer(true);
  let cold = make_peer(false);
  assert_eq!(
    warmed.get_deep_value(),
    cold.get_deep_value(),
    "warmed and cold peers must converge to identical state"
  );
  assert_eq!(warmed.oplog_frontiers(), cold.oplog_frontiers());

  // Warming an empty doc is a no-op (must not panic).
  LoroDoc::new().inner().warm_import_diff_calculator();
}
}
