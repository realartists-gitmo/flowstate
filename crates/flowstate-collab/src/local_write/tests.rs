//! Stage-1 architecture proofs for the Loro-first local write path
//! (spec §13.3–13.6, 13.10 at this layer).

use std::sync::Arc;

use flowstate_document::{DocumentProjection, ParagraphId, ParagraphStyle, paragraph_text};

use super::commit::INJECT_FRAGMENT_FAULT;
use super::gate::{GateHolder, WriteGate};
use super::handle::{LocalDocHandle, LocalWriteConfig};
use super::intents::{
  DeleteRangeIntent, FragmentBlock, InsertRichFragmentIntent, InsertTextIntent, SplitParagraphIntent, TextAnchor, WriteRejected,
};
use crate::crdt_runtime::{CrdtRuntime, RuntimeEvent};

fn new_handle(title: &str) -> (LocalDocHandle, Arc<WriteGate<CrdtRuntime>>) {
  let core = CrdtRuntime::new_empty(title).expect("empty runtime");
  LocalDocHandle::new(core, LocalWriteConfig::default())
}

fn first_paragraph(projection: &DocumentProjection) -> ParagraphId {
  projection.ids.paragraph_ids[0]
}

fn body_string(gate: &Arc<WriteGate<CrdtRuntime>>) -> String {
  let guard = gate.lock(GateHolder::Test).expect("gate healthy");
  flowstate_document::loro_schema::body_text(guard.doc()).to_string()
}

fn frontier(gate: &Arc<WriteGate<CrdtRuntime>>) -> Vec<u8> {
  let guard = gate.lock(GateHolder::Test).expect("gate healthy");
  guard.doc().state_frontiers().encode()
}

/// §13.4: local insert commits synchronously and returns an exact patch; the
/// debug-build audit (compare vs full rebuild) runs inside the same call.
#[test]
fn local_insert_commits_synchronously_and_returns_exact_patch() {
  let (handle, gate) = new_handle("insert");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  let before = frontier(&gate);

  let outcome = handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "hello".into(),
      style_override: None,
    })
    .expect("insert commits");
  let commit = outcome.commit();
  assert!(!commit.patches.patches.is_empty(), "insert must return patches");
  assert_ne!(commit.frontier, before, "commit must advance the frontier");
  assert!(commit.counters.loro_ops >= 1, "ops counter must be recorded");
  assert!(!commit.counters.full_rebuild, "a plain insert must never full-rebuild (I-14)");
  assert_eq!(body_string(&gate), "\nhello");

  // Appending resolves through identity + hint (clamped) — still exact.
  let outcome = handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, usize::MAX),
      text: " world".into(),
      style_override: None,
    })
    .expect("append commits");
  assert!(!outcome.commit().counters.full_rebuild);
  assert_eq!(body_string(&gate), "\nhello world");
}

/// §13.5 / I-15: unresolvable identity rejects BEFORE any mutation.
#[test]
fn resolution_failure_rejects_before_mutation() {
  let (handle, gate) = new_handle("reject");
  let before = frontier(&gate);
  let result = handle.insert_text(InsertTextIntent {
    at: TextAnchor::new(ParagraphId(0xDEAD_BEEF), 0),
    text: "x".into(),
    style_override: None,
  });
  assert!(matches!(result, Err(WriteRejected::UnresolvedParagraph(_))), "unknown identity must reject");
  assert_eq!(frontier(&gate), before, "rejection must not mutate the doc");
  assert_eq!(body_string(&gate), "\n");
}

/// I-2/I-8: structure never enters through a text insert.
#[test]
fn insert_text_rejects_structural_payload() {
  let (handle, gate) = new_handle("structure");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  let before = frontier(&gate);
  let result = handle.insert_text(InsertTextIntent {
    at: TextAnchor::new(paragraph, 0),
    text: "a\nb".into(),
    style_override: None,
  });
  assert!(matches!(result, Err(WriteRejected::StructureViolation(_))));
  assert_eq!(frontier(&gate), before);
}

/// Split commits through Loro paragraph-boundary state and patches exactly.
#[test]
fn split_paragraph_commits_and_patches() {
  let (handle, gate) = new_handle("split");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "aabb".into(),
      style_override: None,
    })
    .expect("seed text");
  let outcome = handle
    .split_paragraph(SplitParagraphIntent {
      at: TextAnchor::new(paragraph, 2),
      inherited_style: ParagraphStyle::Normal,
    })
    .expect("split commits");
  assert!(!outcome.commit().counters.full_rebuild, "split must patch, not rebuild");
  assert_eq!(body_string(&gate), "\naa\nbb");

  let projection = handle.projection().expect("projection after split");
  assert_eq!(projection.paragraphs.len(), 2);
  assert_eq!(paragraph_text(&projection, 0), "aa");
  assert_eq!(paragraph_text(&projection, 1), "bb");
}

/// Cross-paragraph delete merges paragraphs and retires records + blocks.
#[test]
fn delete_range_across_paragraphs_merges() {
  let (handle, gate) = new_handle("merge");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "aabb".into(),
      style_override: None,
    })
    .expect("seed");
  handle
    .split_paragraph(SplitParagraphIntent {
      at: TextAnchor::new(paragraph, 2),
      inherited_style: ParagraphStyle::Normal,
    })
    .expect("split");
  let projection = handle.projection().expect("projection");
  let second = projection.ids.paragraph_ids[1];

  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(paragraph, 1),
      end: TextAnchor::new(second, 1),
    })
    .expect("cross-paragraph delete commits");
  assert_eq!(body_string(&gate), "\nab");
  let projection = handle.projection().expect("projection after merge");
  assert_eq!(projection.paragraphs.len(), 1);
  assert_eq!(paragraph_text(&projection, 0), "ab");
}

/// §13.6: compound-intent failure atomicity. A deterministic mid-apply fault
/// must (a) converge the doc back to export-equivalent pre-intent state,
/// (b) let no projection patch escape, (c) publish partial + inverse as ONE
/// payload a peer can import without ever observing partial state.
#[test]
fn compound_intent_failure_compensates_atomically() {
  let (handle, gate) = new_handle("compensate");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "base".into(),
      style_override: None,
    })
    .expect("seed");
  // Drain the seed's publish traffic so the compensation payload is isolated.
  let _ = gate.lock(GateHolder::Test).expect("gate healthy").take_pending_publish();
  let text_before = body_string(&gate);
  let paragraphs_before = handle.projection().expect("projection").paragraphs.len();

  INJECT_FRAGMENT_FAULT.store(true, std::sync::atomic::Ordering::SeqCst);
  let result = handle.insert_rich_fragment(InsertRichFragmentIntent {
    at: TextAnchor::new(paragraph, 4),
    blocks: vec![
      FragmentBlock::Paragraph(flowstate_document::InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![flowstate_document::InputRun {
          text: "partial".into(),
          styles: flowstate_document::RunStyles::default(),
        }],
      }),
      FragmentBlock::Paragraph(flowstate_document::InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![flowstate_document::InputRun {
          text: "never".into(),
          styles: flowstate_document::RunStyles::default(),
        }],
      }),
    ],
  });
  INJECT_FRAGMENT_FAULT.store(false, std::sync::atomic::Ordering::SeqCst);

  // (a) rejected as compensated; doc state export-equivalent to pre-intent.
  assert!(matches!(result, Err(WriteRejected::CompensatedFailure { .. })), "fault must surface as compensated failure: {result:?}");
  assert_eq!(body_string(&gate), text_before, "doc must converge back to pre-intent state");
  let projection_after = handle.projection().expect("projection");
  assert_eq!(projection_after.paragraphs.len(), paragraphs_before);
  assert_eq!(paragraph_text(&projection_after, 0), "base");

  // (b)/(c): the compensation left exactly one atomic publish payload whose
  // import converges a fresh peer to the same (pre-intent-equivalent) state.
  let events = gate.lock(GateHolder::Test).expect("gate healthy").take_pending_publish();
  let payload_count = events
    .iter()
    .filter(|event| matches!(event, RuntimeEvent::LocalUpdate { .. }))
    .count();
  assert_eq!(payload_count, 1, "partial + inverse must publish as ONE atomic payload");

  // Convergence bar: a fresh peer importing our FULL history (which contains
  // the compensated pair) must agree with us and must never see the partial
  // content. (Both replicas seed their own sentinel, so we assert convergence
  // and partial-invisibility rather than byte equality with our replica.)
  let mut peer = CrdtRuntime::new_empty("compensate").expect("peer");
  let full_history = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export full updates")
  };
  peer.import_remote_update(&full_history).expect("peer imports");
  let peer_history = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&peer_history).expect("local imports peer seed");
  drop(guard);
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  let local_text = body_string(&gate);
  assert_eq!(peer_text, local_text, "replicas must converge after full exchange");
  assert!(!peer_text.contains("partial"), "no replica may ever observe the compensated partial content");
  assert!(!peer_text.contains("never"), "the never-applied second block must not exist anywhere");
  assert!(peer_text.contains("base"), "the pre-intent content survives");
}

/// §13.3 gate atomicity: a competing import thread hammering remote updates
/// can never interleave inside an intent — every committed insert lands where
/// its identity resolved, and both peers converge.
#[test]
fn concurrent_imports_never_interleave_inside_an_intent() {
  let (handle, gate) = new_handle("gate-atomicity");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);

  // Peer B edits its own replica. Converge the two mergeable seeds FIRST so
  // the race runs over one shared document state (each replica seeds its own
  // sentinel; the exchange merges them identically on both sides).
  let mut peer = CrdtRuntime::new_empty("gate-atomicity").expect("peer");
  let local_seed = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  peer.import_remote_update(&local_seed).expect("peer imports seed");
  let peer_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&peer_seed).expect("local imports peer seed");
  drop(guard);
  let peer_paragraph = peer.projection_ref().ids.paragraph_ids[0];
  assert_eq!(paragraph, peer_paragraph, "mergeable seed must give both replicas the same initial paragraph identity");

  let gate_for_imports = Arc::clone(&gate);
  let (updates_tx, updates_rx) = std::sync::mpsc::channel::<Vec<u8>>();
  let importer = std::thread::spawn(move || {
    for update in updates_rx {
      let mut guard = gate_for_imports.lock(GateHolder::ImportChunk).expect("gate healthy");
      guard.import_remote_update(&update).expect("import applies");
    }
  });

  const LOCAL_CHARS: usize = 40;
  let mut peer_vv = peer.doc().state_vv();
  for i in 0..LOCAL_CHARS {
    // Peer B prepends into the shared paragraph and ships the update.
    let peer_body = flowstate_document::loro_schema::body_text(peer.doc());
    peer_body.insert(1, "R").expect("peer insert");
    peer.doc().commit();
    let update = peer.doc().export(loro::ExportMode::updates(&peer_vv)).expect("peer export");
    peer_vv = peer.doc().state_vv();
    updates_tx.send(update).expect("send update");

    // Local user appends at the paragraph end through the intent API while
    // imports race on the other side of the gate.
    let digit = char::from(b'a' + (i % 26) as u8);
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: digit.to_string(),
        style_override: None,
      })
      .expect("local insert commits");
  }
  drop(updates_tx);
  importer.join().expect("importer thread");

  // Full exchange: peer receives everything local, local received everything
  // remote (already, via the importer thread).
  let local_update = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  peer.import_remote_update(&local_update).expect("peer imports local history");
  let local_text = body_string(&gate);
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  assert_eq!(local_text, peer_text, "replicas must converge");

  // The local keystrokes were appends resolved by identity inside the gate:
  // they must appear in order, uninterleaved by the remote prepends.
  let expected: String = (0..LOCAL_CHARS).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
  let locals: String = local_text.chars().filter(|c| c.is_ascii_lowercase()).collect();
  assert_eq!(locals, expected, "local appends must retain their intended order and placement");
  assert_eq!(local_text.chars().filter(|c| *c == 'R').count(), LOCAL_CHARS);
}

/// Undo executes through the Loro `UndoManager` and restores prior content.
#[test]
fn undo_redo_round_trip_through_loro() {
  let (handle, gate) = new_handle("undo");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "abc".into(),
      style_override: None,
    })
    .expect("insert");
  assert_eq!(body_string(&gate), "\nabc");

  let outcome = handle.apply_undo().expect("undo runs");
  assert!(outcome.replace.is_some(), "undo must replace the projection");
  assert_eq!(body_string(&gate), "\n", "undo must restore pre-insert content");

  let outcome = handle.apply_redo().expect("redo runs");
  assert!(outcome.replace.is_some());
  assert_eq!(body_string(&gate), "\nabc", "redo must restore the insert");
}

/// Spec §13.9 (rewritten per the Loro semantics audit): a NON-DISJOINT remote
/// import closes the active undo group — that is Loro's collaborative-undo
/// safety behavior, asserted rather than fought. Commits before the import
/// undo as one unit; commits after it are separate items; the editor re-arms
/// grouping at the next boundary.
#[test]
fn remote_import_mid_group_closes_the_group() {
  let (handle, gate) = new_handle("undo-groups");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);

  // A converged peer (seed exchange first, as in the other harnesses).
  let mut peer = CrdtRuntime::new_empty("undo-groups").expect("peer");
  let local_seed = {
    let guard = gate.lock(GateHolder::Test).expect("gate");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  peer.import_remote_update(&local_seed).expect("peer imports seed");
  let peer_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
  guard.import_remote_update(&peer_seed).expect("local imports peer seed");
  drop(guard);
  let peer_vv = peer.doc().state_vv();

  // Open a group and commit two intents inside it.
  assert!(handle.begin_undo_group().expect("gate healthy"), "group opens");
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, usize::MAX),
      text: "one".into(),
      style_override: None,
    })
    .expect("first grouped insert");
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, usize::MAX),
      text: "two".into(),
      style_override: None,
    })
    .expect("second grouped insert");

  // Remote peer edits the SAME body container; importing it mid-group is
  // non-disjoint and closes the group underneath us.
  let peer_body = flowstate_document::loro_schema::body_text(peer.doc());
  peer_body.insert(1, "R").expect("peer insert");
  peer.doc().commit();
  let update = peer.doc().export(loro::ExportMode::updates(&peer_vv)).expect("export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
  guard.import_remote_update(&update).expect("import applies");
  drop(guard);

  // Post-import commit: with merge_interval(0) and the group closed, this is
  // its own undo item.
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, usize::MAX),
      text: "three".into(),
      style_override: None,
    })
    .expect("post-import insert");
  // end_undo_group is a no-op on the already-closed group; the editor re-arms.
  handle.finish_undo_group().expect("gate healthy");

  let full_text = body_string(&gate);
  assert!(
    full_text.contains("onetwo") && full_text.contains("three"),
    "all three inserts landed (remote R may interleave): {full_text:?}"
  );

  // First undo removes ONLY the post-import commit ("three") — the group did
  // not swallow it.
  handle.apply_undo().expect("undo runs");
  let after_first_undo = body_string(&gate);
  assert!(after_first_undo.contains("onetwo") && !after_first_undo.contains("three"), "post-import commit undoes alone: {after_first_undo:?}");
  assert!(after_first_undo.contains('R'), "remote content is never undone locally");

  // Second undo removes the grouped pre-import commits as one unit.
  handle.apply_undo().expect("undo runs");
  let after_second_undo = body_string(&gate);
  assert!(!after_second_undo.contains("one") && !after_second_undo.contains("two"), "the pre-import group undoes as one unit: {after_second_undo:?}");
  assert!(after_second_undo.contains('R'), "remote content survives all local undo");
}
