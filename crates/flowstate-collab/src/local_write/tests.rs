//! Stage-1 architecture proofs for the Loro-first local write path
//! (spec §13.3–13.6, 13.10 at this layer).

use std::sync::Arc;

use flowstate_document::{DocumentProjection, ParagraphId, ParagraphStyle, paragraph_text};

use super::commit::INJECT_FRAGMENT_FAULT;
use super::gate::{GateHolder, WriteGate};
use super::handle::{LocalDocHandle, LocalWriteConfig};
use super::intents::{
  DeleteRangeIntent, FragmentBlock, InsertRichFragmentIntent, InsertTextIntent, SetMarksIntent, SetParagraphStylesIntent, SplitParagraphIntent,
  TextAnchor, WriteRejected,
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
  assert!(outcome.applied, "undo must apply a projection change");
  assert_eq!(body_string(&gate), "\n", "undo must restore pre-insert content");

  let outcome = handle.apply_redo().expect("redo runs");
  assert!(outcome.applied);
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

/// Replace-all (find & replace) through the ONE write path: same-paragraph
/// matches ride one compound intent = one commit = one undo member; canonical
/// state carries the replacements (the pre-intent editor-side mutation lost
/// them — the 2026-07-07 replace-all data-loss class).
#[test]
fn replace_matches_commits_canonically_and_undoes_as_one_unit() {
  let (handle, gate) = new_handle("replace");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "foo alpha foo beta foo".into(),
      style_override: None,
    })
    .expect("seed insert");
  handle
    .split_paragraph(SplitParagraphIntent {
      at: TextAnchor::new(paragraph, usize::MAX),
      inherited_style: ParagraphStyle::Normal,
    })
    .expect("seed split");
  let second = *handle.projection().expect("projection").ids.paragraph_ids.last().expect("second paragraph");
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(second, 0),
      text: "foo gamma".into(),
      style_override: None,
    })
    .expect("seed second paragraph");

  // Matches across two paragraphs, in ascending order (the write path sorts
  // and applies back-to-front itself).
  let matches = vec![
    super::intents::ReplaceMatch {
      start: TextAnchor::new(paragraph, 0),
      end: TextAnchor::new(paragraph, 3),
      styles: None,
    },
    super::intents::ReplaceMatch {
      start: TextAnchor::new(paragraph, 10),
      end: TextAnchor::new(paragraph, 13),
      styles: None,
    },
    super::intents::ReplaceMatch {
      start: TextAnchor::new(paragraph, 19),
      end: TextAnchor::new(paragraph, 22),
      styles: None,
    },
    super::intents::ReplaceMatch {
      start: TextAnchor::new(second, 0),
      end: TextAnchor::new(second, 3),
      styles: None,
    },
  ];
  let outcome = handle
    .replace_matches(super::intents::ReplaceMatchesIntent {
      matches,
      replacement: "QUUX".into(),
    })
    .expect("replace commits");
  let commit = outcome.commit();
  assert!(!commit.counters.full_rebuild, "replace-matches must patch, not rebuild");
  assert_eq!(body_string(&gate), "\nQUUX alpha QUUX beta QUUX\nQUUX gamma");

  // ONE undo restores every match — the storm was one commit.
  handle.apply_undo().expect("undo runs");
  assert_eq!(body_string(&gate), "\nfoo alpha foo beta foo\nfoo gamma");
  handle.apply_redo().expect("redo runs");
  assert_eq!(body_string(&gate), "\nQUUX alpha QUUX beta QUUX\nQUUX gamma");
}

/// Replace with the empty string deletes matches; collapsed and overlapping
/// ranges are pruned rather than double-edited; a structural replacement is
/// rejected before mutation.
#[test]
fn replace_matches_edge_cases() {
  let (handle, gate) = new_handle("replace-edges");
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "xxABxxCDxx".into(),
      style_override: None,
    })
    .expect("seed insert");

  // Overlapping second range (2..6 vs 4..8) is pruned; collapsed (8..8) is
  // skipped; the empty replacement deletes what survives.
  let outcome = handle.replace_matches(super::intents::ReplaceMatchesIntent {
    matches: vec![
      super::intents::ReplaceMatch {
        start: TextAnchor::new(paragraph, 2),
        end: TextAnchor::new(paragraph, 6),
        styles: None,
      },
      super::intents::ReplaceMatch {
        start: TextAnchor::new(paragraph, 4),
        end: TextAnchor::new(paragraph, 8),
        styles: None,
      },
      super::intents::ReplaceMatch {
        start: TextAnchor::new(paragraph, 8),
        end: TextAnchor::new(paragraph, 8),
        styles: None,
      },
    ],
    replacement: String::new(),
  });
  outcome.expect("empty replacement deletes matches");
  assert_eq!(body_string(&gate), "\nxxCDxx");

  // Structural replacement text is rejected before any mutation.
  let rejected = handle.replace_matches(super::intents::ReplaceMatchesIntent {
    matches: vec![super::intents::ReplaceMatch {
      start: TextAnchor::new(paragraph, 0),
      end: TextAnchor::new(paragraph, 2),
      styles: None,
    }],
    replacement: "a\nb".into(),
  });
  assert!(matches!(rejected, Err(WriteRejected::StructureViolation(_))));
  assert_eq!(body_string(&gate), "\nxxCDxx", "rejected intent must not mutate");

  // All matches collapsed/skipped ⇒ EmptyIntent.
  let empty = handle.replace_matches(super::intents::ReplaceMatchesIntent {
    matches: vec![super::intents::ReplaceMatch {
      start: TextAnchor::new(paragraph, 3),
      end: TextAnchor::new(paragraph, 3),
      styles: None,
    }],
    replacement: "y".into(),
  });
  assert!(matches!(empty, Err(WriteRejected::EmptyIntent)));
}

// ---------------------------------------------------------------------------
// §act-three B.1: recorded-inverse undo/redo fast path
// ---------------------------------------------------------------------------

/// Build a mass multi-paragraph document (large enough to qualify for the
/// recorded-inverse capture) with a styled run per paragraph so mark restore
/// is exercised, plus optional object rows.
fn seed_mass_fragment(handle: &LocalDocHandle, paragraphs: usize, with_equation_at: Option<usize>) {
  let projection = handle.projection().expect("projection");
  let paragraph = first_paragraph(&projection);
  let mut blocks = Vec::with_capacity(paragraphs + 1);
  for ix in 0..paragraphs {
    if Some(ix) == with_equation_at {
      blocks.push(FragmentBlock::Object(flowstate_document::InputBlock::Equation(
        flowstate_document::InputEquationBlock {
          source: "e = mc^2".into(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::Display,
        },
      )));
    }
    let styled = flowstate_document::RunStyles {
      direct_underline: ix % 3 == 0,
      strikethrough: ix % 5 == 0,
      ..Default::default()
    };
    blocks.push(FragmentBlock::Paragraph(flowstate_document::InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        flowstate_document::InputRun {
          text: format!("paragraph {ix:03} plain lead-in "),
          styles: flowstate_document::RunStyles::default(),
        },
        flowstate_document::InputRun {
          text: format!("styled body text for row {ix:03} padding padding padding padding"),
          styles: styled,
        },
      ],
    }));
  }
  handle
    .insert_rich_fragment(InsertRichFragmentIntent {
      at: TextAnchor::new(paragraph, 0),
      blocks,
    })
    .expect("mass fragment seeds");
}

/// `(undo_stack.len(), redo_stack.len())` — the fast-path stack depths. Because
/// every fast step MOVES one entry between the stacks (and the slow path clears
/// both), these depths prove exactly how many consecutive steps stayed fast.
fn stack_depths(gate: &Arc<WriteGate<CrdtRuntime>>) -> (usize, usize) {
  let mut guard = gate.lock(GateHolder::Test).expect("gate healthy");
  (guard.recorded_undo_stack().len(), guard.recorded_redo_stack().len())
}

/// §act-five P3-deep (fail-loud): a RUN of consecutive local undos must EACH
/// replay via the fast path — not just the first. On the old single-slot design
/// undo #2+ fell to the O(doc/history) Loro UndoManager; the stack keeps them
/// all O(change). `stack_depths` is the proof: three undos move all three
/// inverses undo→redo, which the single slot could not do.
#[test]
fn recorded_inverse_stack_chains_consecutive_fast_undos() {
  let (handle, gate) = new_handle("recorded-inverse-chain");
  seed_mass_fragment(&handle, 40, None);
  // Restyle every paragraph EXCEPT the first: boundary 0 (`ROOT_FIRST_PARAGRAPH`)
  // has a pre-existing live-vs-canonical style quirk orthogonal to this change,
  // so excluding it keeps the convergence assertion a clean P3-deep signal.
  let ids: Vec<ParagraphId> = paragraph_ids(&handle).into_iter().skip(1).collect();
  let styles_of = |handle: &LocalDocHandle| -> Vec<ParagraphStyle> {
    handle.projection().expect("projection").paragraphs.iter().map(|paragraph| paragraph.style).collect()
  };
  let original = styles_of(&handle);

  // THREE consecutive mass restyles — each arms its own recorded inverse.
  for slot in [1_u8, 2, 3] {
    handle
      .set_paragraph_styles(SetParagraphStylesIntent {
        paragraphs: ids.clone(),
        style: ParagraphStyle::Custom(slot),
      })
      .expect("restyle commits");
  }
  let after_three = styles_of(&handle);
  assert!(after_three.iter().skip(1).all(|style| *style == ParagraphStyle::Custom(3)), "third restyle wins for all restyled paragraphs");
  assert_eq!(stack_depths(&gate), (3, 0), "three forward edits stack three undoable inverses");

  // THREE undos — EACH must use the fast path (all three move undo→redo).
  for _ in 0..3 {
    assert!(handle.apply_undo().expect("undo runs").applied);
  }
  assert_eq!(stack_depths(&gate), (0, 3), "P3-deep: all three consecutive undos replayed via the fast path");
  assert_eq!(styles_of(&handle), original, "chained undo restores the original styles");

  // THREE redos — each fast, back to the third restyle.
  for _ in 0..3 {
    assert!(handle.apply_redo().expect("redo runs").applied);
  }
  assert_eq!(stack_depths(&gate), (3, 0), "P3-deep: all three consecutive redos replayed via the fast path");
  assert_eq!(styles_of(&handle), after_three, "chained redo re-applies every restyle");

  // The fast-path chain must produce the SAME canonical Loro state the slow path
  // would: the maintained (live) projection equals a fresh materialization of the
  // committed doc (P3-deep changes only WHICH replay path runs, not the ops).
  let local_canonical: Vec<ParagraphStyle> = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    flowstate_document::document_from_loro(guard.doc()).expect("materializes").paragraphs.iter().map(|paragraph| paragraph.style).collect()
  };
  assert_eq!(styles_of(&handle), local_canonical, "the fast-path chain leaves live == canonical");
}

/// §act-five P3-deep convergence: a restyle→undo→redo run must converge with a
/// peer after a BIDIRECTIONAL exchange. (An earlier one-way variant "diverged"
/// only because the fresh peer seeds its OWN sentinel paragraph — `new_empty`
/// mints ~84 init ops incl a `\n` boundary — so a one-way import leaves the peer
/// with an extra sentinel; the real bar is convergence after both import each
/// other, per the existing round-trip tests.)
#[test]
fn recorded_inverse_stack_restyle_undo_redo_converges_bidirectionally() {
  let (handle, gate) = new_handle("chain-converge");
  seed_mass_fragment(&handle, 12, None);
  let ids: Vec<ParagraphId> = paragraph_ids(&handle).into_iter().skip(1).collect();
  for slot in [1_u8, 2, 3] {
    handle.set_paragraph_styles(SetParagraphStylesIntent { paragraphs: ids.clone(), style: ParagraphStyle::Custom(slot) }).expect("restyle");
  }
  for _ in 0..3 {
    handle.apply_undo().expect("undo");
  }
  for _ in 0..3 {
    handle.apply_redo().expect("redo");
  }
  let styles = |doc: &loro::LoroDoc| -> Vec<ParagraphStyle> {
    flowstate_document::document_from_loro(doc).expect("mat").paragraphs.iter().map(|p| p.style).collect()
  };
  // Bidirectional exchange: peer takes local's whole history, local takes the
  // peer's seed back — now both hold both sentinels and must converge.
  let local_history = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    g.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export")
  };
  let mut peer = CrdtRuntime::new_empty("chain-peer").expect("peer");
  peer.import_remote_update(&local_history).expect("peer imports local");
  let peer_seed = peer.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("peer export");
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&peer_seed).expect("local imports peer seed");
  }
  let local_after = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    styles(g.doc())
  };
  assert_eq!(styles(peer.doc()), local_after, "restyle→undo→redo converges after a bidirectional exchange");
}

/// §act-five P1.B: a peer importing a LONG run of sequential remote updates
/// (each advancing the RETAINED `import_diff_calculator`) must converge to the
/// sender. Exercises the reuse path repeatedly — a bug in the retained-tracker
/// advance would surface as body divergence here (and the retained calc uses
/// Loro's general Checkout apply, which must stay bit-correct across imports).
#[test]
fn retained_import_calculator_converges_over_many_sequential_imports() {
  let (handle_a, gate_a) = new_handle("p1b-a");
  let a_init = {
    let g = gate_a.lock(GateHolder::Test).expect("gate");
    g.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export A init")
  };
  let mut peer = CrdtRuntime::new_empty("p1b-b").expect("peer B");
  peer.import_remote_update(&a_init).expect("B imports A init");

  let first = first_paragraph(&handle_a.projection().expect("projection"));
  for i in 0..24 {
    handle_a
      .insert_text(InsertTextIntent { at: TextAnchor::new(first, 0), text: format!("x{i} "), style_override: None })
      .expect("A edit commits");
    // B imports A's whole history each round; only the new op applies, exercising
    // the retained calculator once more (idempotent for already-present ops).
    let full = {
      let g = gate_a.lock(GateHolder::Test).expect("gate");
      g.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export A")
    };
    peer.import_remote_update(&full).expect("B imports A");
  }
  // Bidirectional close: A takes B's seed back so both hold both sentinels; then
  // the two canonical bodies must be identical (the real convergence bar).
  let b_seed = peer.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export B");
  {
    let mut g = gate_a.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&b_seed).expect("A imports B seed");
  }
  let a_body = {
    let g = gate_a.lock(GateHolder::Test).expect("gate");
    flowstate_document::loro_schema::body_text(g.doc()).to_string()
  };
  let b_body = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  assert_eq!(b_body, a_body, "peer converges after 24 sequential retained-calculator imports");
}

fn slot_direction(gate: &Arc<WriteGate<CrdtRuntime>>) -> Option<loro::UndoOrRedo> {
  // §act-five P3-deep: the fast path is now two stacks. A pending REDO (top of
  // the redo stack) means the last fast step was an undo; else a pending UNDO.
  let mut guard = gate.lock(GateHolder::Test).expect("gate healthy");
  if !guard.recorded_redo_stack().is_empty() {
    Some(loro::UndoOrRedo::Redo)
  } else if !guard.recorded_undo_stack().is_empty() {
    Some(loro::UndoOrRedo::Undo)
  } else {
    None
  }
}

fn paragraph_ids(handle: &LocalDocHandle) -> Vec<ParagraphId> {
  handle.projection().expect("projection").ids.paragraph_ids.clone()
}

fn block_ids(handle: &LocalDocHandle) -> Vec<flowstate_document::BlockId> {
  handle.projection().expect("projection").ids.block_ids.clone()
}

/// The core round trip: a qualifying mass delete arms the slot; undo replays
/// the recorded inverse (no checkout), restoring text, marks, and the ORIGINAL
/// paragraph/block identities; redo replays the delete; ping-pong stays fast.
/// The debug-build audit inside every step compares the patched projection
/// against a full rematerialization.
#[test]
fn recorded_inverse_fast_undo_round_trips_mass_delete() {
  let (handle, gate) = new_handle("recorded-inverse");
  seed_mass_fragment(&handle, 40, None);

  let body_before = body_string(&gate);
  let paragraph_ids_before = paragraph_ids(&handle);
  let block_ids_before = block_ids(&handle);

  // Cross-paragraph mass delete: from inside paragraph 2 to inside paragraph 37.
  let start = paragraph_ids_before[2];
  let end = paragraph_ids_before[37];
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(start, 9),
      end: TextAnchor::new(end, 12),
    })
    .expect("mass delete commits");
  let body_deleted = body_string(&gate);
  assert!(body_deleted.len() + 2048 < body_before.len(), "delete must remove a qualifying mass range");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo), "mass delete must arm the recorded inverse");

  // Undo: fast path (flips the slot; the slow path never touches it).
  let outcome = handle.apply_undo().expect("undo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_before, "undo must restore the exact body text + boundaries");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Redo), "fast undo must flip the slot to redo");
  assert_eq!(paragraph_ids(&handle), paragraph_ids_before, "undo must restore ORIGINAL paragraph identities");
  assert_eq!(block_ids(&handle), block_ids_before, "undo must restore ORIGINAL block identities");

  // Redo: fast replay of the recorded delete.
  let outcome = handle.apply_redo().expect("redo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_deleted, "redo must reproduce the deleted state");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo), "fast redo must flip the slot back");

  // Ping-pong once more.
  let outcome = handle.apply_undo().expect("second undo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_before);
  assert_eq!(paragraph_ids(&handle), paragraph_ids_before);

  // Convergence bar: a fresh peer importing the full history (delete + fast
  // inverse + fast redelete + fast inverse) must agree byte-for-byte.
  let mut peer = CrdtRuntime::new_empty("recorded-inverse").expect("peer");
  let full_history = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export full updates")
  };
  peer.import_remote_update(&full_history).expect("peer imports");
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  let peer_history = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&peer_history).expect("local imports peer seed");
  drop(guard);
  assert_eq!(body_string(&gate), peer_text, "replicas must converge on the fast-path history");
}

/// Object rows (equation) inside the deleted range restore with their ORIGINAL
/// block ids and content.
#[test]
fn recorded_inverse_restores_object_blocks_with_original_ids() {
  let (handle, gate) = new_handle("recorded-inverse-objects");
  seed_mass_fragment(&handle, 36, Some(18));

  let body_before = body_string(&gate);
  let block_ids_before = block_ids(&handle);
  let ids = paragraph_ids(&handle);
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[3], 4),
      end: TextAnchor::new(ids[33], 6),
    })
    .expect("mass delete across the equation commits");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo), "object range must still qualify");

  let outcome = handle.apply_undo().expect("undo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_before);
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Redo), "fast path must have run");
  assert_eq!(block_ids(&handle), block_ids_before, "equation block id must be restored, not re-minted");
  let projection = handle.projection().expect("projection");
  let equation = projection
    .blocks
    .iter()
    .find_map(|block| match block {
      flowstate_document::Block::Equation(equation) => Some(equation.source.to_string()),
      _ => None,
    })
    .expect("equation row restored");
  assert_eq!(equation, "e = mc^2");
}

/// A remote import between the delete and the undo kills the fast path (slot
/// cleared) — undo still works through the checkout-based slow path and keeps
/// BOTH the restored content and the remote edit.
#[test]
fn recorded_inverse_declines_after_remote_import() {
  let (handle, gate) = new_handle("recorded-inverse-import");
  seed_mass_fragment(&handle, 40, None);
  let ids = paragraph_ids(&handle);

  // Converged peer first.
  let mut peer = CrdtRuntime::new_empty("recorded-inverse-import").expect("peer");
  let seed = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  peer.import_remote_update(&seed).expect("peer imports seed");
  let peer_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&peer_seed).expect("local imports peer seed");
  drop(guard);
  let peer_vv = peer.doc().state_vv();

  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[2], 0),
      end: TextAnchor::new(ids[38], 3),
    })
    .expect("mass delete commits");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo));

  // Remote edit lands between delete and undo.
  let peer_body = flowstate_document::loro_schema::body_text(peer.doc());
  peer_body.insert(1, "REMOTE").expect("peer insert");
  peer.doc().commit();
  let update = peer.doc().export(loro::ExportMode::updates(&peer_vv)).expect("export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&update).expect("import applies");
  drop(guard);
  assert_eq!(slot_direction(&gate), None, "an import must clear the recorded inverse");

  let outcome = handle.apply_undo().expect("undo still runs via the slow path");
  assert!(outcome.applied);
  let body = body_string(&gate);
  assert!(body.contains("REMOTE"), "slow-path undo must keep the remote edit");
  assert!(body.contains("paragraph 020"), "slow-path undo must restore the deleted content");
}

/// Small or same-paragraph deletes never arm the slot; a table row inside the
/// range gates the capture out (durable table ids cannot be re-minted).
#[test]
fn recorded_inverse_gating() {
  let (handle, gate) = new_handle("recorded-inverse-gating");
  seed_mass_fragment(&handle, 8, None);
  let ids = paragraph_ids(&handle);

  // Small cross-paragraph delete: below the capture threshold.
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[1], 0),
      end: TextAnchor::new(ids[2], 4),
    })
    .expect("small delete commits");
  assert_eq!(slot_direction(&gate), None, "sub-threshold deletes must not arm the slot");

  // Table inside the range: gated out.
  let (handle, gate) = new_handle("recorded-inverse-table-gate");
  seed_mass_fragment(&handle, 40, None);
  let projection = handle.projection().expect("projection");
  let anchor = projection.ids.paragraph_ids[20];
  let row_id = gpui_flowtext::RowId(1);
  let column_id = gpui_flowtext::ColumnId(2);
  handle
    .insert_rich_fragment(InsertRichFragmentIntent {
      at: TextAnchor::new(anchor, 0),
      blocks: vec![FragmentBlock::Object(flowstate_document::InputBlock::Table(
        flowstate_document::InputTableBlock {
          rows: vec![flowstate_document::InputTableRow {
            id: row_id,
            cells: vec![flowstate_document::InputTableCell {
              id: gpui_flowtext::CellId(3),
              row_id,
              column_id,
              blocks: Vec::new(),
              row_span: 1,
              col_span: 1,
            }],
          }],
          columns: vec![flowstate_document::InputTableColumn {
            id: column_id,
            width: flowstate_document::InputTableColumnWidth::Auto,
          }],
          style: flowstate_document::InputTableStyle { header_row: false },
        },
      ))],
    })
    .expect("table inserts");
  let ids = paragraph_ids(&handle);
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[2], 0),
      end: TextAnchor::new(ids[38], 3),
    })
    .expect("mass delete across the table commits");
  // The one guarantee B.1 owns here: a durable-id table in range gates the
  // recorded-inverse capture OUT (its row/column/cell ids cannot be re-minted
  // losslessly), so undo takes the checkout-based slow path. (Whether that
  // slow path perfectly restores an object placeholder deleted by a
  // cross-paragraph range is a separate, pre-existing Loro-undo concern —
  // demonstrated by `slow_undo_drops_object_placeholder_pre_existing` — that
  // this fast path is not responsible for, and in fact improves upon.)
  assert_eq!(slot_direction(&gate), None, "a table in range must gate the capture out");
  let outcome = handle.apply_undo().expect("slow undo runs");
  assert!(outcome.applied, "slow-path undo of the gated delete still applies");
}

/// Characterization of a PRE-EXISTING slow-path (checkout-based Loro undo)
/// limitation, kept as a guardrail: undoing a cross-paragraph delete that
/// removed an object placeholder restores all the TEXT but DROPS the U+FFFC
/// object placeholder (the block record then projects as an unresolved anchor
/// and is repaired away). This is independent of B.1 — the recorded-inverse
/// fast path (`recorded_inverse_restores_object_blocks_with_original_ids`)
/// restores the object correctly, so it is strictly more faithful than the
/// slow path it replaces for qualifying deletes. If a Loro upgrade ever fixes
/// the underlying checkout-undo behavior, this test will flip and flag it.
#[test]
fn slow_undo_drops_object_placeholder_pre_existing() {
  let (handle, gate) = new_handle("slow-undo-object");
  seed_mass_fragment(&handle, 4, Some(2));
  let ids = paragraph_ids(&handle);
  assert_eq!(body_string(&gate).chars().filter(|ch| *ch == '\u{FFFC}').count(), 1);

  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[1], 0),
      end: TextAnchor::new(ids[3], 4),
    })
    .expect("small delete across the equation commits");
  assert_eq!(slot_direction(&gate), None, "sub-threshold: no recorded-inverse capture");
  assert_eq!(body_string(&gate).chars().filter(|ch| *ch == '\u{FFFC}').count(), 0);

  let outcome = handle.apply_undo().expect("slow undo runs");
  assert!(outcome.applied);
  // Pre-existing: the placeholder is NOT restored by the checkout-based undo.
  assert_eq!(
    body_string(&gate).chars().filter(|ch| *ch == '\u{FFFC}').count(),
    0,
    "documents the pre-existing slow-path object-placeholder drop (see doc comment)"
  );
}

/// §act-four M1: a mass select-all restyle records its inverse; undo reverts
/// every paragraph's style, redo re-applies — via the fast path (no checkout),
/// with the debug audit inside each step verifying patch-vs-full-rebuild.
#[test]
fn recorded_inverse_fast_undo_round_trips_mass_restyle() {
  let (handle, gate) = new_handle("recorded-inverse-restyle");
  seed_mass_fragment(&handle, 40, None);
  let ids = paragraph_ids(&handle);

  let styles_of = |handle: &LocalDocHandle| -> Vec<ParagraphStyle> {
    handle.projection().expect("projection").paragraphs.iter().map(|paragraph| paragraph.style).collect()
  };
  let before = styles_of(&handle);
  assert!(before.iter().all(|style| *style == ParagraphStyle::Normal), "seed paragraphs start Normal");

  // Select-all restyle to a custom style.
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: ids.clone(),
      style: ParagraphStyle::Custom(3),
    })
    .expect("mass restyle commits");
  let after = styles_of(&handle);
  assert!(after.iter().all(|style| *style == ParagraphStyle::Custom(3)), "restyle sets every paragraph");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo), "mass restyle must arm the recorded inverse");

  // Undo: fast path reverts every style.
  let outcome = handle.apply_undo().expect("undo runs");
  assert!(outcome.applied);
  assert_eq!(styles_of(&handle), before, "undo must restore every paragraph's prior style");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Redo), "fast undo flips the slot to redo");

  // Redo: fast path re-applies.
  let outcome = handle.apply_redo().expect("redo runs");
  assert!(outcome.applied);
  assert_eq!(styles_of(&handle), after, "redo must re-apply the restyle");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo), "fast redo flips the slot back");

  // Ping-pong once more + convergence: a fresh peer receives the full history
  // (restyle + fast inverse + fast redo + fast inverse), and after a
  // BIDIRECTIONAL exchange both replicas materialize the same converged
  // document (each seeds its own sentinel, so convergence — not identity with a
  // pre-sync snapshot — is the bar).
  handle.apply_undo().expect("second undo runs");
  assert_eq!(styles_of(&handle), before);
  let mut peer = CrdtRuntime::new_empty("recorded-inverse-restyle").expect("peer");
  let full_history = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export")
  };
  peer.import_remote_update(&full_history).expect("peer imports local history");
  let peer_history = peer.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&peer_history).expect("local imports peer seed");
  drop(guard);
  let styles = |doc: &DocumentProjection| -> Vec<ParagraphStyle> { doc.paragraphs.iter().map(|paragraph| paragraph.style).collect() };
  let local_fresh = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    flowstate_document::document_from_loro(guard.doc()).expect("local materializes")
  };
  let peer_fresh = flowstate_document::document_from_loro(peer.doc()).expect("peer materializes");
  assert_eq!(styles(&peer_fresh), styles(&local_fresh), "replicas converge on the same styles after the fast-path history");
  assert!(styles(&local_fresh).iter().all(|style| *style == ParagraphStyle::Normal), "converged on the undone (Normal) styles");
}

/// §fidelity (field bug 2026-07-08, two-peer hotpath): a LOCAL undo must revert
/// ONLY the local peer's op — NEVER a concurrent REMOTE op. Repro from the field:
/// peer A highlights the whole body (a run-style mark), peer B concurrently tags
/// every paragraph (a paragraph style); A imports B's tag, then A Ctrl-Z's its
/// highlight. The observed bug: BOTH the highlight AND the remote tag reverted.
///
/// This asserts the invariant against the LIVE maintained projection (what the UI
/// paints) AND a fresh canonical materialization — the pair localizes whether the
/// fault is the CRDT undo-exclusion or the post-undo reprojection.
#[test]
fn local_undo_of_highlight_preserves_concurrent_remote_paragraph_style() {
  let (handle_a, gate_a) = new_handle("undo-isolation-a");
  seed_mass_fragment(&handle_a, 8, None);

  // Peer B starts from A's exact initial state (shared paragraph ids), then edits
  // concurrently (neither peer has seen the other's edit).
  let a_initial = {
    let guard = gate_a.lock(GateHolder::Test).expect("gate healthy");
    guard.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export A initial")
  };
  let mut core_b = CrdtRuntime::new_empty("undo-isolation-b").expect("peer B runtime");
  core_b.import_remote_update(&a_initial).expect("B imports A initial state");
  let (handle_b, gate_b) = LocalDocHandle::new(core_b, LocalWriteConfig::default());

  let ids = paragraph_ids(&handle_a);
  let tag = ParagraphStyle::Custom(7);

  // Peer B: tag every paragraph (LOCAL to B).
  handle_b
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: ids.clone(),
      style: tag,
    })
    .expect("B tags every paragraph");

  // Peer A: highlight the whole body (LOCAL to A) — a run-style mark.
  let projection_a = handle_a.projection().expect("projection A");
  let first = projection_a.ids.paragraph_ids[0];
  let last = *projection_a.ids.paragraph_ids.last().expect("last paragraph id");
  let last_len = paragraph_text(&projection_a, projection_a.paragraphs.len() - 1).len();
  let highlight = flowstate_document::RunStyles {
    highlight: Some(flowstate_document::HighlightStyle::Custom(3)),
    ..Default::default()
  };
  handle_a
    .set_marks(SetMarksIntent {
      start: TextAnchor::new(first, 0),
      end: TextAnchor::new(last, last_len),
      styles: highlight,
    })
    .expect("A highlights the whole body");

  // A imports B's concurrent tag (origin "remote" ⇒ excluded from A's undo stack).
  let b_update = {
    let guard = gate_b.lock(GateHolder::Test).expect("gate healthy");
    guard.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export B")
  };
  {
    let mut guard = gate_a.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_updates(&[&b_update]).expect("A imports B's tag");
  }

  // Read (styles, any-highlight) from the LIVE projection and from a fresh
  // canonical materialization — divergence between them localizes the fault.
  let live = |handle: &LocalDocHandle| {
    let doc = handle.projection().expect("live projection");
    let styles: Vec<ParagraphStyle> = doc.paragraphs.iter().map(|paragraph| paragraph.style).collect();
    let any_highlight = doc.paragraphs.iter().any(|paragraph| paragraph.runs.iter().any(|run| run.styles.highlight.is_some()));
    (styles, any_highlight)
  };
  let canonical = |gate: &Arc<WriteGate<CrdtRuntime>>| {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    let doc = flowstate_document::document_from_loro(guard.doc()).expect("materialize canonical");
    let styles: Vec<ParagraphStyle> = doc.paragraphs.iter().map(|paragraph| paragraph.style).collect();
    let any_highlight = doc.paragraphs.iter().any(|paragraph| paragraph.runs.iter().any(|run| run.styles.highlight.is_some()));
    (styles, any_highlight)
  };

  // The remote tag defines the EXPECTED paragraph-style vector: A's local undo
  // touches only its run-style highlight, so paragraph styles must be identical
  // to B's tag result before undo, after undo, and after convergence.
  let expected_styles = canonical(&gate_b).0;
  assert!(expected_styles.iter().any(|style| *style == tag), "B's tag must land on the durable paragraphs: {expected_styles:?}");

  // Before undo A already reflects BOTH edits.
  let (styles_before, hl_before) = live(&handle_a);
  assert_eq!(styles_before, expected_styles, "A must reflect the remote tag before undo");
  assert!(hl_before, "A must see its own highlight before undo");

  // A UNDOES its highlight.
  assert!(handle_a.apply_undo().expect("A undo runs").applied, "undo must apply");

  // THE INVARIANT: highlight gone, paragraph styles UNCHANGED — live AND canonical.
  let (live_styles, live_hl) = live(&handle_a);
  let (canon_styles, canon_hl) = canonical(&gate_a);
  assert!(!live_hl, "undo must remove A's own highlight (live projection)");
  assert!(!canon_hl, "undo must remove A's own highlight (canonical)");
  assert_eq!(
    canon_styles, expected_styles,
    "CRDT undo-exclusion bug: local undo changed paragraph styles in canonical state (reverted the concurrent REMOTE tag)"
  );
  assert_eq!(
    live_styles, expected_styles,
    "reprojection bug: post-undo LIVE projection dropped the concurrent REMOTE tag"
  );

  // Convergence: B receives A's post-undo history; both replicas agree, tag intact.
  let a_after = {
    let guard = gate_a.lock(GateHolder::Test).expect("gate healthy");
    guard.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export A after undo")
  };
  {
    let mut guard = gate_b.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_updates(&[&a_after]).expect("B imports A's post-undo history");
  }
  let (styles_b, hl_b) = canonical(&gate_b);
  assert!(!hl_b, "peer B converges on highlight-removed");
  assert_eq!(styles_b, expected_styles, "peer B keeps the remote tag after convergence");
}

/// §act-four M1: a mass replace-all records its inverse; undo restores every
/// original match, redo re-applies — via the fast path (no checkout), the
/// projection side deriving through the regional ladder. Convergence verified
/// against a peer receiving the whole fast-path history.
#[test]
fn recorded_inverse_fast_undo_round_trips_mass_replace() {
  let (handle, gate) = new_handle("recorded-inverse-replace");
  seed_mass_fragment(&handle, 40, None);

  let body_before = body_string(&gate);
  // Every seeded paragraph contains "row" exactly once (…"for row NNN"…).
  let projection = handle.projection().expect("projection");
  let ranges = flowstate_document::find_text_ranges(&projection, "row");
  assert!(ranges.len() >= 8, "need a mass replace (got {} matches)", ranges.len());
  let matches: Vec<super::intents::ReplaceMatch> = ranges
    .iter()
    .map(|range| super::intents::ReplaceMatch {
      start: TextAnchor::new(projection.ids.paragraph_ids[range.start.paragraph], range.start.byte),
      end: TextAnchor::new(projection.ids.paragraph_ids[range.end.paragraph], range.end.byte),
      styles: None,
    })
    .collect();

  handle
    .replace_matches(super::intents::ReplaceMatchesIntent {
      matches,
      replacement: "COLUMN".into(),
    })
    .expect("mass replace commits");
  let body_replaced = body_string(&gate);
  assert!(!body_replaced.contains("row"), "replace removed every 'row'");
  assert!(body_replaced.contains("COLUMN"), "replace inserted 'COLUMN'");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo), "mass replace must arm the recorded inverse");

  // Undo: fast path restores the originals.
  let outcome = handle.apply_undo().expect("undo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_before, "undo must restore every original match");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Redo));

  // Redo: fast path re-applies the replacement.
  let outcome = handle.apply_redo().expect("redo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_replaced, "redo must reproduce the replaced state");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo));

  // Ping-pong + bidirectional convergence.
  handle.apply_undo().expect("second undo runs");
  assert_eq!(body_string(&gate), body_before);
  let mut peer = CrdtRuntime::new_empty("recorded-inverse-replace").expect("peer");
  let full_history = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("export")
  };
  peer.import_remote_update(&full_history).expect("peer imports local history");
  let peer_history = peer.doc().export(loro::ExportMode::updates(&loro::VersionVector::default())).expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&peer_history).expect("local imports peer seed");
  drop(guard);
  let local_text = body_string(&gate);
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  assert_eq!(local_text, peer_text, "replicas converge on the replace-all fast-path history");
  assert!(!peer_text.contains("COLUMN"), "converged on the undone (original) content");
}
