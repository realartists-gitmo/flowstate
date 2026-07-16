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
  assert!(
    matches!(result, Err(WriteRejected::UnresolvedParagraph(_))),
    "unknown identity must reject"
  );
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
  let _ = gate
    .lock(GateHolder::Test)
    .expect("gate healthy")
    .take_pending_publish();
  let text_before = body_string(&gate);
  let paragraphs_before = handle.projection().expect("projection").paragraphs.len();

  INJECT_FRAGMENT_FAULT.with(|fault| fault.set(true));
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
  INJECT_FRAGMENT_FAULT.with(|fault| fault.set(false));

  // (a) rejected as compensated; doc state export-equivalent to pre-intent.
  assert!(
    matches!(result, Err(WriteRejected::CompensatedFailure { .. })),
    "fault must surface as compensated failure: {result:?}"
  );
  assert_eq!(body_string(&gate), text_before, "doc must converge back to pre-intent state");
  let projection_after = handle.projection().expect("projection");
  assert_eq!(projection_after.paragraphs.len(), paragraphs_before);
  assert_eq!(paragraph_text(&projection_after, 0), "base");

  // (b)/(c): the compensation left exactly one atomic publish payload whose
  // import converges a fresh peer to the same (pre-intent-equivalent) state.
  let events = gate
    .lock(GateHolder::Test)
    .expect("gate healthy")
    .take_pending_publish();
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
  peer
    .import_remote_update(&full_history)
    .expect("peer imports");
  let peer_history = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard
    .import_remote_update(&peer_history)
    .expect("local imports peer seed");
  drop(guard);
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  let local_text = body_string(&gate);
  assert_eq!(peer_text, local_text, "replicas must converge after full exchange");
  assert!(
    !peer_text.contains("partial"),
    "no replica may ever observe the compensated partial content"
  );
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
  peer
    .import_remote_update(&local_seed)
    .expect("peer imports seed");
  let peer_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard
    .import_remote_update(&peer_seed)
    .expect("local imports peer seed");
  drop(guard);
  let peer_paragraph = peer.projection_ref().ids.paragraph_ids[0];
  assert_eq!(
    paragraph, peer_paragraph,
    "mergeable seed must give both replicas the same initial paragraph identity"
  );

  let gate_for_imports = Arc::clone(&gate);
  let (updates_tx, updates_rx) = std::sync::mpsc::channel::<Vec<u8>>();
  let importer = std::thread::spawn(move || {
    for update in updates_rx {
      let mut guard = gate_for_imports
        .lock(GateHolder::ImportChunk)
        .expect("gate healthy");
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
    let update = peer
      .doc()
      .export(loro::ExportMode::updates(&peer_vv))
      .expect("peer export");
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
  peer
    .import_remote_update(&local_update)
    .expect("peer imports local history");
  let local_text = body_string(&gate);
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  assert_eq!(local_text, peer_text, "replicas must converge");

  // The local keystrokes were appends resolved by identity inside the gate:
  // they must appear in order, uninterleaved by the remote prepends.
  let expected: String = (0..LOCAL_CHARS)
    .map(|i| char::from(b'a' + (i % 26) as u8))
    .collect();
  let locals: String = local_text
    .chars()
    .filter(|c| c.is_ascii_lowercase())
    .collect();
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
  peer
    .import_remote_update(&local_seed)
    .expect("peer imports seed");
  let peer_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
  guard
    .import_remote_update(&peer_seed)
    .expect("local imports peer seed");
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
  let update = peer
    .doc()
    .export(loro::ExportMode::updates(&peer_vv))
    .expect("export");
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
  assert!(
    after_first_undo.contains("onetwo") && !after_first_undo.contains("three"),
    "post-import commit undoes alone: {after_first_undo:?}"
  );
  assert!(after_first_undo.contains('R'), "remote content is never undone locally");

  // Second undo removes the grouped pre-import commits as one unit.
  handle.apply_undo().expect("undo runs");
  let after_second_undo = body_string(&gate);
  assert!(
    !after_second_undo.contains("one") && !after_second_undo.contains("two"),
    "the pre-import group undoes as one unit: {after_second_undo:?}"
  );
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
  let second = *handle
    .projection()
    .expect("projection")
    .ids
    .paragraph_ids
    .last()
    .expect("second paragraph");
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
/// undo #2+ fell to the O(doc/history) Loro `UndoManager`; the stack keeps them
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
    handle
      .projection()
      .expect("projection")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
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
  assert!(
    after_three
      .iter()
      .skip(1)
      .all(|style| *style == ParagraphStyle::Custom(3)),
    "third restyle wins for all restyled paragraphs"
  );
  assert_eq!(stack_depths(&gate), (3, 0), "three forward edits stack three undoable inverses");

  // THREE undos — EACH must use the fast path (all three move undo→redo).
  for _ in 0..3 {
    assert!(handle.apply_undo().expect("undo runs").applied);
  }
  assert_eq!(
    stack_depths(&gate),
    (0, 3),
    "P3-deep: all three consecutive undos replayed via the fast path"
  );
  assert_eq!(styles_of(&handle), original, "chained undo restores the original styles");

  // THREE redos — each fast, back to the third restyle.
  for _ in 0..3 {
    assert!(handle.apply_redo().expect("redo runs").applied);
  }
  assert_eq!(
    stack_depths(&gate),
    (3, 0),
    "P3-deep: all three consecutive redos replayed via the fast path"
  );
  assert_eq!(styles_of(&handle), after_three, "chained redo re-applies every restyle");

  // The fast-path chain must produce the SAME canonical Loro state the slow path
  // would: the maintained (live) projection equals a fresh materialization of the
  // committed doc (P3-deep changes only WHICH replay path runs, not the ops).
  let local_canonical: Vec<ParagraphStyle> = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    flowstate_document::document_from_loro(guard.doc())
      .expect("materializes")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  assert_eq!(styles_of(&handle), local_canonical, "the fast-path chain leaves live == canonical");
}

/// §act-ten A10.2 (the field killer): the 900ms autosave commits metadata +
/// revision ops, advancing the frontier — on the old strict `expected_frontier`
/// equality this cleared the recorded stacks, so anyone pausing before Ctrl-Z
/// fell to the O(doc) slow path. The "meta"-origin commit is undo-INERT (it
/// never touches the body), so the stacks must survive it. `stack_depths` is
/// the proof the step stayed fast (a slow-path step clears both stacks).
#[test]
fn recorded_inverse_survives_inert_checkpoint_commits() {
  let (handle, gate) = new_handle("checkpoint-inert");
  seed_mass_fragment(&handle, 12, None);
  let ids: Vec<ParagraphId> = paragraph_ids(&handle).into_iter().skip(1).collect();
  let styles_of = |handle: &LocalDocHandle| -> Vec<ParagraphStyle> {
    handle
      .projection()
      .expect("projection")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  let original = styles_of(&handle);
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: ids,
      style: ParagraphStyle::Custom(4),
    })
    .expect("restyle commits");
  assert_eq!(stack_depths(&gate), (1, 0), "forward edit arms one recorded inverse");

  // The autosave: metadata + revision commits (the in-place checkpoint path).
  let dir = std::env::temp_dir().join(format!("flowstate-checkpoint-inert-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("temp dir");
  let path = dir.join("autosave.db8");
  {
    let mut guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .checkpoint_package("Autosave", Some(path.clone()), &flowstate_document::RevisionStamp::auto())
      .expect("checkpoint runs")
  };

  // Undo AFTER the autosave must still take the fast path.
  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "undo stayed FAST across the inert autosave commit");
  assert_eq!(styles_of(&handle), original, "undo restored the pre-restyle styles");

  // And redo across ANOTHER autosave.
  {
    let mut guard = gate.lock(GateHolder::Test).expect("gate healthy");
    guard
      .checkpoint_package("Autosave", Some(path), &flowstate_document::RevisionStamp::auto())
      .expect("second checkpoint runs")
  };
  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "redo stayed FAST across the inert autosave commit");
  let _ = std::fs::remove_dir_all(&dir);
}

/// §act-ten A10.2: a fully-pending (out-of-order) remote import applies NOTHING
/// — the frontier is unchanged, so the recorded fast-undo timelines are exactly
/// as valid as before. The old eager clear-at-entry destroyed them on every
/// drip, which under live collab meant fast undo almost never survived.
#[test]
fn recorded_inverse_survives_noop_pending_import() {
  let (handle, gate) = new_handle("noop-import");
  seed_mass_fragment(&handle, 12, None);
  let ids: Vec<ParagraphId> = paragraph_ids(&handle).into_iter().skip(1).collect();
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: ids,
      style: ParagraphStyle::Custom(4),
    })
    .expect("restyle commits");
  assert_eq!(stack_depths(&gate), (1, 0));

  // Build an out-of-order update on a peer: two edits, export ONLY the second
  // (its dependency is missing on the receiver ⇒ fully pending, applies nothing).
  let (peer, peer_gate) = new_handle("noop-import-peer");
  let peer_paragraph = first_paragraph(&peer.projection().expect("projection"));
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(peer_paragraph, 0),
      text: "first".into(),
      style_override: None,
    })
    .expect("peer edit 1");
  let vv_after_first = {
    let guard = peer_gate.lock(GateHolder::Test).expect("gate healthy");
    guard.doc().state_vv()
  };
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(peer_paragraph, 0),
      text: "second".into(),
      style_override: None,
    })
    .expect("peer edit 2");
  let second_only = {
    let guard = peer_gate.lock(GateHolder::Test).expect("gate healthy");
    let export = guard
      .export_updates_for(&vv_after_first)
      .expect("delta export");
    drop(guard);
    export
  };

  // Import the dependency-less blob: fully pending, frontier unchanged.
  let mut guard = gate.lock(GateHolder::Test).expect("gate healthy");
  let frontier_before = guard.doc().state_frontiers();
  guard
    .import_remote_update(&second_only)
    .expect("pending import is accepted");
  let frontier_after = guard.doc().state_frontiers();
  drop(guard);
  assert_eq!(frontier_after, frontier_before, "fully-pending import applied nothing");
  assert_eq!(stack_depths(&gate), (1, 0), "no-op import kept the recorded timeline");
  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "undo stayed FAST after the no-op import");
}

/// §oom-leads #9 (mass-op collab): a remote import whose changes land strictly
/// BEFORE every recorded coordinate must REBASE the fast-path stacks — shift
/// the recorded coordinates through the import's net delta and keep the fast
/// step armed — instead of the old unconditional clear (which made fast undo
/// effectively dead under live collab: any partner keystroke between an edit
/// and Ctrl-Z forced the O(doc/history) slow path). A remote change AT/INSIDE
/// the recorded hull must still clear: the recorded replay content would
/// clobber it, and mixed-space coordinate math is unsound there.
#[test]
fn recorded_inverse_rebases_across_hull_disjoint_remote_import() {
  let (handle, gate) = new_handle("rebase-disjoint");
  seed_mass_fragment(&handle, 24, None);
  // Bidirectional seed exchange so later peer deltas import cleanly (both
  // sides hold both `new_empty` sentinels).
  let (peer, peer_gate) = new_handle("rebase-disjoint-peer");
  let local_history = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  {
    let mut g = peer_gate.lock(GateHolder::ImportChunk).expect("peer gate");
    g.import_remote_update(&local_history)
      .expect("peer imports local history")
  };
  let peer_history = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("peer export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&peer_history)
      .expect("local imports peer seed")
  };
  let styles_of = |handle: &LocalDocHandle| -> Vec<ParagraphStyle> {
    handle
      .projection()
      .expect("projection")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  let original = styles_of(&handle);

  // A restyles the BOTTOM half — the recorded hull sits far above position 0.
  let ids: Vec<ParagraphId> = paragraph_ids(&handle).into_iter().skip(12).collect();
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: ids,
      style: ParagraphStyle::Custom(5),
    })
    .expect("restyle commits");
  assert_eq!(stack_depths(&gate), (1, 0), "forward edit arms one recorded inverse");

  // B types near the TOP of the document — strictly before the hull.
  let peer_paragraphs = paragraph_ids(&peer);
  let vv_before_chatter = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc().state_vv()
  };
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(peer_paragraphs[1], 0),
      text: "typed above the restyled region ".into(),
      style_override: None,
    })
    .expect("peer chatter above the hull");
  let chatter = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.export_updates_for(&vv_before_chatter)
      .expect("delta export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&chatter).expect("chatter imports")
  };
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "hull-disjoint remote import REBASED the recorded inverse instead of clearing it"
  );

  // Undo/redo AFTER the remote chatter must both stay on the fast path AND be
  // content-correct (the coordinates were shifted through the net delta).
  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "undo stayed FAST across the rebased remote import");
  assert_eq!(
    styles_of(&handle),
    original,
    "undo restored the pre-restyle styles at the shifted positions"
  );
  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "redo stayed FAST across the rebased remote import");

  // The fast-path replay must leave live == canonical (same bar as P3-deep).
  let canonical: Vec<ParagraphStyle> = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    flowstate_document::document_from_loro(guard.doc())
      .expect("materializes")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  assert_eq!(styles_of(&handle), canonical, "rebased fast-path replay leaves live == canonical");

  // §v2: chatter INSIDE the hull used to clear (v1's global-hull law) — a
  // MARK-ONLY entry now rebases per-coordinate instead (its replay is
  // position-neutral, so in-hull chatter is sound). The residual clear cases
  // live in `remote_insert_at_recorded_coordinate_clears_instead_of_rebasing`
  // (content entry, at-coordinate) and the truncation half of
  // `mark_top_survives_while_deeper_keystroke_truncates_then_slow_path_converges`.
  let vv_before_inside = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc().state_vv()
  };
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(peer_paragraphs[20], 0),
      text: "typed inside the restyled region ".into(),
      style_override: None,
    })
    .expect("peer chatter inside the hull");
  let inside = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.export_updates_for(&vv_before_inside)
      .expect("delta export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&inside)
      .expect("in-hull chatter imports")
  };
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "in-hull chatter REBASES a mark-only entry per-coordinate (v2)"
  );
  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "undo stayed FAST across the in-hull rebase");
  assert_eq!(
    styles_of(&handle),
    original,
    "fast undo after the in-hull rebase still restores the styles"
  );
  let canonical_after: Vec<ParagraphStyle> = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    flowstate_document::document_from_loro(guard.doc())
      .expect("materializes")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  assert_eq!(styles_of(&handle), canonical_after, "in-hull rebased replay leaves live == canonical");
}

/// §oom-leads #9 (keystroke undo): EVERY `InsertText` and intra-paragraph
/// `DeleteRange` records its inverse, so the most common ops — typing and
/// backspace — undo/redo O(change) with no checkout, AND no longer clear a
/// deeper captured fast path (a single typed char used to kill a recorded
/// mass-op inverse behind it). `stack_depths` is the fast-path proof: the slow
/// path clears both stacks; every fast step moves one entry.
#[test]
fn recorded_inverse_captures_keystrokes_and_backspace() {
  let (handle, gate) = new_handle("keystroke-capture");
  seed_mass_fragment(&handle, 12, None);
  let target = paragraph_ids(&handle)[3];
  let original = body_string(&gate);

  // Three keystrokes, one styled — three armed inverses.
  for (ch, style) in [
    ("a", None),
    ("b", None),
    (
      "c",
      Some(flowstate_document::RunStyles {
        direct_underline: true,
        ..Default::default()
      }),
    ),
  ] {
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(target, 0),
        text: ch.to_string(),
        style_override: style,
      })
      .expect("keystroke commits");
  }
  assert_eq!(stack_depths(&gate), (3, 0), "every keystroke arms a recorded inverse");

  // Backspace: a 1-char intra-paragraph delete — captured too.
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(target, 0),
      end: TextAnchor::new(target, 1),
    })
    .expect("backspace commits");
  assert_eq!(stack_depths(&gate), (4, 0), "an intra-paragraph delete arms a recorded inverse");
  let after_edits = body_string(&gate);

  // Four undos — ALL fast (undo→redo moves, no clear), back to the original.
  for _ in 0..4 {
    assert!(handle.apply_undo().expect("undo runs").applied);
  }
  assert_eq!(stack_depths(&gate), (0, 4), "all four keystroke undos replayed via the fast path");
  assert_eq!(body_string(&gate), original, "chained keystroke undo restores the original body");

  // Four redos — all fast, back to the edited body (styled char included).
  for _ in 0..4 {
    assert!(handle.apply_redo().expect("redo runs").applied);
  }
  assert_eq!(stack_depths(&gate), (4, 0), "all four keystroke redos replayed via the fast path");
  assert_eq!(body_string(&gate), after_edits, "chained keystroke redo re-applies every edit");

  // Live projection == canonical materialization (the same bar as P3-deep).
  let live = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  let canonical = {
    let document = {
      let guard = gate.lock(GateHolder::Test).expect("gate healthy");
      flowstate_document::document_from_loro(guard.doc()).expect("materializes")
    };
    (0..document.paragraphs.len())
      .map(|ix| paragraph_text(&document, ix))
      .collect::<Vec<_>>()
  };
  assert_eq!(live, canonical, "keystroke fast-path replay leaves live == canonical");
}

/// §oom-leads #9 (keystroke undo × rebase): the field's most common freeze —
/// type, partner types ABOVE you, Ctrl-Z — must stay on the fast path: the
/// keystroke's recorded inverse rebases through the hull-disjoint remote import
/// (coordinates shift by the net delta) instead of clearing.
#[test]
fn recorded_inverse_keystroke_survives_hull_disjoint_chatter() {
  let (handle, gate) = new_handle("keystroke-rebase");
  seed_mass_fragment(&handle, 24, None);
  let (peer, peer_gate) = new_handle("keystroke-rebase-peer");
  let local_history = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  {
    let mut g = peer_gate.lock(GateHolder::ImportChunk).expect("peer gate");
    g.import_remote_update(&local_history)
      .expect("peer imports local history")
  };
  let peer_history = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("peer export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&peer_history)
      .expect("local imports peer seed")
  };

  // A types DEEP in the document — the recorded hull sits far above position 0.
  let deep = paragraph_ids(&handle)[16];
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(deep, 4),
      text: "X".into(),
      style_override: None,
    })
    .expect("local keystroke commits");
  assert_eq!(stack_depths(&gate), (1, 0), "the keystroke armed a recorded inverse");
  let after_keystroke_projection = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };

  // B types near the TOP — strictly before the hull.
  let peer_paragraphs = paragraph_ids(&peer);
  let vv_before_chatter = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc().state_vv()
  };
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(peer_paragraphs[1], 0),
      text: "partner typed above ".into(),
      style_override: None,
    })
    .expect("peer chatter above the hull");
  let chatter = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.export_updates_for(&vv_before_chatter)
      .expect("delta export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&chatter).expect("chatter imports")
  };
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "hull-disjoint chatter REBASED the keystroke inverse instead of clearing it"
  );

  // Ctrl-Z after the partner's edit: fast, and the deep 'X' is gone while the
  // partner's text survives.
  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "keystroke undo stayed FAST across the rebased remote import");
  let live = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  assert!(
    live
      .iter()
      .any(|text| text.contains("partner typed above ")),
    "the partner's chatter survives the undo"
  );
  assert!(!live.iter().any(|text| text.contains('X')), "the undone keystroke is gone");
  let canonical = {
    let document = {
      let guard = gate.lock(GateHolder::Test).expect("gate healthy");
      flowstate_document::document_from_loro(guard.doc()).expect("materializes")
    };
    (0..document.paragraphs.len())
      .map(|ix| paragraph_text(&document, ix))
      .collect::<Vec<_>>()
  };
  assert_eq!(live, canonical, "rebased keystroke undo leaves live == canonical");
  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "keystroke redo stayed FAST too");
  let live_after_redo = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  assert!(
    live_after_redo.iter().any(|text| text.contains('X')),
    "redo re-applied the keystroke at the shifted position"
  );
  // The projection must equal the pre-chatter shape plus the chatter.
  assert_ne!(live_after_redo, after_keystroke_projection, "the chatter is part of the final projection");
}

/// §oom-leads #9 (rebase soundness, v1 gap): a mass delete's pre-recorded UNDO
/// PATCHES snapshot the WHOLE first paragraph — including its prefix BEFORE the
/// delete start, which is OUTSIDE the coordinate hull. A hull-disjoint remote
/// edit landing in that prefix passes the rebase, and replaying the stale
/// snapshot would silently clobber it in the projection (the doc-side mutation
/// is shifted and stays correct — live and canonical diverge). The rebase must
/// downgrade content-carrying patches to the derive ladder.
#[test]
fn rebase_downgrades_content_patches_when_remote_edits_first_patched_paragraph() {
  let (handle, gate) = new_handle("rebase-patch-content");
  seed_mass_fragment(&handle, 40, None);
  let (peer, peer_gate) = new_handle("rebase-patch-content-peer");
  let local_history = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  {
    let mut g = peer_gate.lock(GateHolder::ImportChunk).expect("peer gate");
    g.import_remote_update(&local_history)
      .expect("peer imports local history")
  };
  let peer_history = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("peer export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&peer_history)
      .expect("local imports peer seed")
  };

  // A mass-deletes paragraphs 12..38 starting MID-paragraph-12: the first
  // patched paragraph keeps a prefix BEFORE the recorded hull.
  let ids = paragraph_ids(&handle);
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[12], 9),
      end: TextAnchor::new(ids[38], 12),
    })
    .expect("mass delete commits");
  assert_eq!(stack_depths(&gate), (1, 0), "the mass delete armed a recorded inverse");

  // B types INSIDE paragraph 12's surviving prefix — before the hull, but
  // inside the first patched paragraph's snapshot.
  let peer_paragraphs = paragraph_ids(&peer);
  let vv_before = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc().state_vv()
  };
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(peer_paragraphs[12], 2),
      text: "REMOTE".into(),
      style_override: None,
    })
    .expect("peer edits the first patched paragraph's prefix");
  let chatter = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.export_updates_for(&vv_before).expect("delta export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&chatter).expect("chatter imports")
  };
  assert_eq!(stack_depths(&gate), (1, 0), "the prefix edit is hull-disjoint — the inverse rebased");

  // Undo: the restore must NOT clobber B's prefix edit in the projection.
  assert!(handle.apply_undo().expect("undo runs").applied);
  let live = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  let canonical = {
    let document = {
      let guard = gate.lock(GateHolder::Test).expect("gate healthy");
      flowstate_document::document_from_loro(guard.doc()).expect("materializes")
    };
    (0..document.paragraphs.len())
      .map(|ix| paragraph_text(&document, ix))
      .collect::<Vec<_>>()
  };
  assert_eq!(
    live, canonical,
    "post-rebase undo leaves live == canonical (stale content patches must downgrade to derive)"
  );
  assert!(
    live.iter().any(|text| text.contains("REMOTE")),
    "the remote prefix edit survives the undo replay"
  );
}

/// Bidirectionally seed `peer_gate` with `gate`'s history (both end holding
/// both sentinels) — the standard two-peer test opening.
fn exchange_full_histories(gate: &Arc<WriteGate<CrdtRuntime>>, peer_gate: &Arc<WriteGate<CrdtRuntime>>) {
  let local_history = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  {
    let mut g = peer_gate.lock(GateHolder::ImportChunk).expect("peer gate");
    g.import_remote_update(&local_history)
      .expect("peer imports local history")
  };
  let peer_history = {
    let g = peer_gate.lock(GateHolder::Test).expect("peer gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("peer export")
  };
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&peer_history)
      .expect("local imports peer seed")
  };
}

fn gate_state_vv(gate: &Arc<WriteGate<CrdtRuntime>>) -> loro::VersionVector {
  let g = gate.lock(GateHolder::Test).expect("gate");
  g.doc().state_vv()
}

/// Export `from`'s ops since `vv` and import them into `to`.
fn sync_delta(from: &Arc<WriteGate<CrdtRuntime>>, to: &Arc<WriteGate<CrdtRuntime>>, vv: &loro::VersionVector) {
  let delta = {
    let g = from.lock(GateHolder::Test).expect("gate");
    g.export_updates_for(vv).expect("delta export")
  };
  let mut g = to.lock(GateHolder::ImportChunk).expect("gate");
  g.import_remote_update(&delta).expect("delta imports");
}

/// The live-projection audit: text AND styles must equal an independent full
/// materialization (the styles half is what makes the mark-rebase tests bite).
fn assert_live_equals_canonical(handle: &LocalDocHandle, gate: &Arc<WriteGate<CrdtRuntime>>, context: &str) {
  let (live_text, live_styles) = {
    let projection = handle.projection().expect("projection");
    (
      (0..projection.paragraphs.len())
        .map(|ix| paragraph_text(&projection, ix))
        .collect::<Vec<_>>(),
      projection
        .paragraphs
        .iter()
        .map(|p| p.style)
        .collect::<Vec<_>>(),
    )
  };
  let (canonical_text, canonical_styles) = {
    let document = {
      let guard = gate.lock(GateHolder::Test).expect("gate healthy");
      flowstate_document::document_from_loro(guard.doc()).expect("materializes")
    };
    (
      (0..document.paragraphs.len())
        .map(|ix| paragraph_text(&document, ix))
        .collect::<Vec<_>>(),
      document
        .paragraphs
        .iter()
        .map(|p| p.style)
        .collect::<Vec<_>>(),
    )
  };
  assert_eq!(live_text, canonical_text, "{context}: live text == canonical");
  assert_eq!(live_styles, canonical_styles, "{context}: live styles == canonical");
}

/// §oom-leads #9 v2 (select-all rebase): a mass restyle's recorded inverse is
/// MARK-ONLY — its replay is position-neutral — so remote chatter INSIDE the
/// restyled span rebases per-coordinate instead of clearing. (v1 demanded the
/// chatter sit strictly before the recorded hull, which a select-all pins to
/// the doc start: any chatter anywhere killed the fast path — the bench's
/// UNDO-AFTER-REMOTE slow-path cliff.) Covers both stacks: a second chatter
/// burst lands while the entry sits on the REDO side.
#[test]
fn select_all_restyle_rebases_per_coordinate_through_remote_chatter() {
  let (handle, gate) = new_handle("select-all-rebase");
  seed_mass_fragment(&handle, 24, None);
  let (peer, peer_gate) = new_handle("select-all-rebase-peer");
  exchange_full_histories(&gate, &peer_gate);

  let styles_before: Vec<ParagraphStyle> = handle
    .projection()
    .expect("projection")
    .paragraphs
    .iter()
    .map(|p| p.style)
    .collect();
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: paragraph_ids(&handle),
      style: ParagraphStyle::Custom(4),
    })
    .expect("select-all restyle");
  assert_eq!(stack_depths(&gate), (1, 0), "the restyle armed a recorded inverse");

  // B types in the MIDDLE of the restyled span — inside the v1 hull.
  let vv = gate_state_vv(&peer_gate);
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&peer)[12], 0),
      text: "partner typed inside ".into(),
      style_override: None,
    })
    .expect("peer chatter inside the span");
  sync_delta(&peer_gate, &gate, &vv);
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "chatter inside the restyled span REBASED the mark entry per-coordinate"
  );

  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "select-all undo stayed FAST across the rebased import");
  let after_undo = handle.projection().expect("projection");
  let styles: Vec<ParagraphStyle> = after_undo.paragraphs.iter().map(|p| p.style).collect();
  assert_eq!(styles, styles_before, "undo restored every paragraph's prior style");
  let text = (0..after_undo.paragraphs.len())
    .map(|ix| paragraph_text(&after_undo, ix))
    .collect::<Vec<_>>();
  assert!(
    text.iter().any(|t| t.contains("partner typed inside ")),
    "the partner's chatter survives the undo"
  );
  assert_live_equals_canonical(&handle, &gate, "select-all undo after mid-span chatter");

  // Another chatter burst while the entry sits on the REDO stack.
  let vv = gate_state_vv(&peer_gate);
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&peer)[3], 2),
      text: "more partner text ".into(),
      style_override: None,
    })
    .expect("peer chatter while entry is on the redo stack");
  sync_delta(&peer_gate, &gate, &vv);
  assert_eq!(stack_depths(&gate), (0, 1), "redo-stack mark entries rebase per-coordinate too");
  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "select-all redo stayed FAST");
  let after_redo = handle.projection().expect("projection");
  assert!(
    after_redo
      .paragraphs
      .iter()
      .all(|p| p.style == ParagraphStyle::Custom(4)),
    "redo re-applied the restyle to every paragraph"
  );
  assert_live_equals_canonical(&handle, &gate, "select-all redo after second chatter");
}

/// §oom-leads #9 v2: a remote delete that REMOVES a targeted boundary (merges
/// two paragraphs) drops that TARGET, not the whole entry — the native slow
/// path's remote-diff transform would exclude the dead boundary the same way.
#[test]
fn select_all_rebase_drops_targets_whose_boundary_the_remote_deleted() {
  let (handle, gate) = new_handle("select-all-drop-target");
  seed_mass_fragment(&handle, 24, None);
  let (peer, peer_gate) = new_handle("select-all-drop-target-peer");
  exchange_full_histories(&gate, &peer_gate);

  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: paragraph_ids(&handle),
      style: ParagraphStyle::Custom(4),
    })
    .expect("select-all restyle");
  assert_eq!(stack_depths(&gate), (1, 0), "the restyle armed a recorded inverse");

  // B deletes across the paragraph 9/10 boundary — that boundary char dies.
  let vv = gate_state_vv(&peer_gate);
  let peer_ids = paragraph_ids(&peer);
  peer
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(peer_ids[9], 4),
      end: TextAnchor::new(peer_ids[10], 2),
    })
    .expect("peer merges two paragraphs");
  sync_delta(&peer_gate, &gate, &vv);
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "the mark entry survived with the dead boundary's target dropped"
  );

  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "undo stayed FAST despite the dropped target");
  let after_undo = handle.projection().expect("projection");
  assert!(
    after_undo
      .paragraphs
      .iter()
      .all(|p| p.style == ParagraphStyle::Normal),
    "undo reverted every surviving paragraph (the merged one rides its surviving boundary)"
  );
  assert_live_equals_canonical(&handle, &gate, "select-all undo after remote boundary delete");

  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "redo stayed FAST");
  let after_redo = handle.projection().expect("projection");
  assert!(
    after_redo
      .paragraphs
      .iter()
      .all(|p| p.style == ParagraphStyle::Custom(4)),
    "redo re-restyled every surviving paragraph"
  );
  assert_live_equals_canonical(&handle, &gate, "select-all redo after remote boundary delete");
}

/// §oom-leads #9 v2 (suffix truncation + the slow-path handoff): remote chatter
/// AFTER a deep keystroke's hull truncates the keystroke entry, but the
/// mark-only select-all entry ABOVE it survives per-coordinate. Undo #1 replays
/// fast; undo #2 finds the recorded stack dry and takes the native slow path,
/// which must transform through the buffered remote diff correctly (the vendor
/// patch #22 window: the fast step's inverse commit already transformed the
/// buffers, native-style).
#[test]
fn mark_top_survives_while_deeper_keystroke_truncates_then_slow_path_converges() {
  let (handle, gate) = new_handle("mark-top-truncate");
  seed_mass_fragment(&handle, 24, None);
  let (peer, peer_gate) = new_handle("mark-top-truncate-peer");
  exchange_full_histories(&gate, &peer_gate);

  // A types at paragraph 8 (captured content entry, hull deep in the doc)...
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&handle)[8], 4),
      text: "X".into(),
      style_override: None,
    })
    .expect("local keystroke commits");
  // ...then select-all restyles (mark entry on top).
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: paragraph_ids(&handle),
      style: ParagraphStyle::Custom(6),
    })
    .expect("select-all restyle");
  assert_eq!(stack_depths(&gate), (2, 0), "keystroke + restyle both recorded");

  // B types at paragraph 20 — AFTER the keystroke's hull, inside the mark span.
  let vv = gate_state_vv(&peer_gate);
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&peer)[20], 0),
      text: "partner deep chatter ".into(),
      style_override: None,
    })
    .expect("peer chatter after the keystroke hull");
  sync_delta(&peer_gate, &gate, &vv);
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "the mark top survived per-coordinate; the keystroke entry (chatter after its hull) truncated"
  );

  // Undo #1: the mark entry, fast.
  assert!(handle.apply_undo().expect("undo #1 runs").applied);
  let after_first_undo = handle.projection().expect("projection");
  assert!(
    after_first_undo
      .paragraphs
      .iter()
      .all(|p| p.style == ParagraphStyle::Normal),
    "undo #1 reverted the restyle"
  );
  // Undo #2: recorded stack dry — the native slow path pops the keystroke item
  // and must land exactly on the keystroke (partner text intact).
  assert!(handle.apply_undo().expect("undo #2 runs").applied);
  let live = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  assert!(!live.iter().any(|text| text.contains('X')), "the slow-path undo removed the keystroke");
  assert!(
    live
      .iter()
      .any(|text| text.contains("partner deep chatter ")),
    "the partner's chatter survives both undos"
  );
  assert_live_equals_canonical(&handle, &gate, "fast mark undo then slow keystroke undo");
}

/// §oom-leads #9 v2 (structural rebase): a remote paragraph SPLIT strictly
/// before a captured keystroke's hull shifts the recorded coordinates like any
/// other insert — the entry survives (patches downgrade to the derive ladder;
/// the projection row set changed) instead of clearing.
#[test]
fn keystroke_rebase_survives_remote_structural_insert_before_hull() {
  let (handle, gate) = new_handle("keystroke-structural-rebase");
  seed_mass_fragment(&handle, 24, None);
  let (peer, peer_gate) = new_handle("keystroke-structural-rebase-peer");
  exchange_full_histories(&gate, &peer_gate);

  let paragraph_count_before = handle.projection().expect("projection").paragraphs.len();
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&handle)[16], 4),
      text: "X".into(),
      style_override: None,
    })
    .expect("local keystroke commits");
  assert_eq!(stack_depths(&gate), (1, 0), "the keystroke armed a recorded inverse");

  // B splits paragraph 2 — a structural `\n` insert strictly before the hull.
  let vv = gate_state_vv(&peer_gate);
  peer
    .split_paragraph(SplitParagraphIntent {
      at: TextAnchor::new(paragraph_ids(&peer)[2], 3),
      inherited_style: ParagraphStyle::Normal,
    })
    .expect("peer splits a paragraph above the hull");
  sync_delta(&peer_gate, &gate, &vv);
  assert_eq!(
    stack_depths(&gate),
    (1, 0),
    "structural chatter above the hull REBASED the keystroke entry (derive), not cleared"
  );

  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "keystroke undo stayed FAST across the structural rebase");
  let after_undo = handle.projection().expect("projection");
  assert_eq!(
    after_undo.paragraphs.len(),
    paragraph_count_before + 1,
    "the remote split survives the undo"
  );
  let undo_text = (0..after_undo.paragraphs.len())
    .map(|ix| paragraph_text(&after_undo, ix))
    .collect::<Vec<_>>();
  assert!(!undo_text.iter().any(|t| t.contains('X')), "the undone keystroke is gone");
  assert_live_equals_canonical(&handle, &gate, "keystroke undo after remote structural insert");

  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "keystroke redo stayed FAST");
  let after_redo = handle.projection().expect("projection");
  let redo_text = (0..after_redo.paragraphs.len())
    .map(|ix| paragraph_text(&after_redo, ix))
    .collect::<Vec<_>>();
  assert!(
    redo_text.iter().any(|t| t.contains('X')),
    "redo re-applied the keystroke at the shifted position"
  );
  assert_live_equals_canonical(&handle, &gate, "keystroke redo after remote structural insert");
}

/// §oom-leads #9 v2 (the at-coordinate trap stays closed): a remote insert
/// EXACTLY AT a recorded coordinate is ambiguous — the CRDT parks remote edits
/// whose neighbors were locally touched at that spot — so the entry must CLEAR
/// (slow path), never shift-and-replay (which would delete the partner's char).
#[test]
fn remote_insert_at_recorded_coordinate_clears_instead_of_rebasing() {
  let (handle, gate) = new_handle("keystroke-at-coordinate");
  seed_mass_fragment(&handle, 24, None);
  let (peer, peer_gate) = new_handle("keystroke-at-coordinate-peer");
  exchange_full_histories(&gate, &peer_gate);

  let vv_peer_baseline = gate_state_vv(&gate);
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&handle)[16], 4),
      text: "X".into(),
      style_override: None,
    })
    .expect("local keystroke commits");
  assert_eq!(stack_depths(&gate), (1, 0), "the keystroke armed a recorded inverse");

  // B sees the keystroke, then types at the SAME anchor — its char lands
  // exactly at the recorded delete coordinate.
  sync_delta(&gate, &peer_gate, &vv_peer_baseline);
  let vv = gate_state_vv(&peer_gate);
  peer
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph_ids(&peer)[16], 4),
      text: "Y".into(),
      style_override: None,
    })
    .expect("peer types at the recorded coordinate");
  sync_delta(&peer_gate, &gate, &vv);
  assert_eq!(
    stack_depths(&gate),
    (0, 0),
    "an insert AT the recorded coordinate cleared the entry (ambiguous parking spot)"
  );

  // The slow path still serves the undo, removing exactly the local 'X'.
  assert!(handle.apply_undo().expect("undo runs").applied);
  let live = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  assert!(!live.iter().any(|text| text.contains('X')), "the undone keystroke is gone");
  assert!(live.iter().any(|text| text.contains('Y')), "the partner's at-coordinate char survives");
  assert_live_equals_canonical(&handle, &gate, "slow-path undo after at-coordinate chatter");
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
    handle
      .set_paragraph_styles(SetParagraphStylesIntent {
        paragraphs: ids.clone(),
        style: ParagraphStyle::Custom(slot),
      })
      .expect("restyle");
  }
  for _ in 0..3 {
    handle.apply_undo().expect("undo");
  }
  for _ in 0..3 {
    handle.apply_redo().expect("redo");
  }
  let styles = |doc: &loro::LoroDoc| -> Vec<ParagraphStyle> {
    flowstate_document::document_from_loro(doc)
      .expect("mat")
      .paragraphs
      .iter()
      .map(|p| p.style)
      .collect()
  };
  // Bidirectional exchange: peer takes local's whole history, local takes the
  // peer's seed back — now both hold both sentinels and must converge.
  let local_history = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  let mut peer = CrdtRuntime::new_empty("chain-peer").expect("peer");
  peer
    .import_remote_update(&local_history)
    .expect("peer imports local");
  let peer_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  {
    let mut g = gate.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&peer_seed)
      .expect("local imports peer seed")
  };
  let local_after = {
    let g = gate.lock(GateHolder::Test).expect("gate");
    styles(g.doc())
  };
  assert_eq!(
    styles(peer.doc()),
    local_after,
    "restyle→undo→redo converges after a bidirectional exchange"
  );
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
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export A init")
  };
  let mut peer = CrdtRuntime::new_empty("p1b-b").expect("peer B");
  peer
    .import_remote_update(&a_init)
    .expect("B imports A init");

  let first = first_paragraph(&handle_a.projection().expect("projection"));
  for i in 0..24 {
    handle_a
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(first, 0),
        text: format!("x{i} "),
        style_override: None,
      })
      .expect("A edit commits");
    // B imports A's whole history each round; only the new op applies, exercising
    // the retained calculator once more (idempotent for already-present ops).
    let full = {
      let g = gate_a.lock(GateHolder::Test).expect("gate");
      g.doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("export A")
    };
    peer.import_remote_update(&full).expect("B imports A");
  }
  // Bidirectional close: A takes B's seed back so both hold both sentinels; then
  // the two canonical bodies must be identical (the real convergence bar).
  let b_seed = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("export B");
  {
    let mut g = gate_a.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&b_seed).expect("A imports B seed")
  };
  let a_body = {
    let g = gate_a.lock(GateHolder::Test).expect("gate");
    flowstate_document::loro_schema::body_text(g.doc()).to_string()
  };
  let b_body = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  assert_eq!(b_body, a_body, "peer converges after 24 sequential retained-calculator imports");
}

/// §oom-leads #9 P1.C (vendor patch #21, meet-clamped diff feed): the RECEIVER
/// keeps making local edits between sequential imports from a peer that never
/// syncs back — the bench's REMOTE-STRUCT-BARE shape and the deep-LCA
/// pathology's trigger. The retained import calculator is never fed local
/// commits, so its tracker has a coverage GAP each round; the clamp must
/// re-feed exactly that gap (a clamped-away hole corrupts the import diff's
/// retain offsets and diverges the body).
#[test]
fn meet_clamped_import_feed_converges_with_interleaved_local_edits() {
  let (handle_a, gate_a) = new_handle("p21-clamp-a");
  seed_mass_fragment(&handle_a, 8, None);
  let (handle_b, gate_b) = new_handle("p21-clamp-b");
  let a_init = {
    let g = gate_a.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export A init")
  };
  {
    let mut g = gate_b.lock(GateHolder::ImportChunk).expect("B gate");
    g.import_remote_update(&a_init).expect("B imports A init")
  };
  let b_init = {
    let g = gate_b.lock(GateHolder::Test).expect("B gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export B init")
  };
  {
    let mut g = gate_a.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&b_init).expect("A imports B init")
  };

  let mut b_exported_vv = {
    let g = gate_b.lock(GateHolder::Test).expect("B gate");
    g.doc().state_vv()
  };
  for i in 0..16 {
    // B edits its own copy — a growing chain A must import; B NEVER imports
    // A's edits, so every delta is maximally concurrent with A's local work
    // (the deep-dominator shape).
    let b_target = paragraph_ids(&handle_b)[1];
    handle_b
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(b_target, 0),
        text: format!("b{i} "),
        style_override: None,
      })
      .expect("B edit commits");
    // A makes a LOCAL edit — the retained import calculator's coverage gap.
    let a_target = paragraph_ids(&handle_a)[3];
    handle_a
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(a_target, 0),
        text: format!("a{i} "),
        style_override: None,
      })
      .expect("A edit commits");
    let delta = {
      let g = gate_b.lock(GateHolder::Test).expect("B gate");
      let delta = g
        .export_updates_for(&b_exported_vv)
        .expect("B delta export");
      b_exported_vv = g.doc().state_vv();
      delta
    };
    let mut g = gate_a.lock(GateHolder::ImportChunk).expect("gate");
    g.import_remote_update(&delta).expect("A imports B delta");
  }

  // Close the loop: B takes A's full history; both canonical bodies must match.
  let a_full = {
    let g = gate_a.lock(GateHolder::Test).expect("gate");
    g.doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export A full")
  };
  {
    let mut g = gate_b.lock(GateHolder::ImportChunk).expect("B gate");
    g.import_remote_update(&a_full).expect("B imports A full")
  };
  let a_body = {
    let g = gate_a.lock(GateHolder::Test).expect("gate");
    flowstate_document::loro_schema::body_text(g.doc()).to_string()
  };
  let b_body = {
    let g = gate_b.lock(GateHolder::Test).expect("B gate");
    flowstate_document::loro_schema::body_text(g.doc()).to_string()
  };
  assert_eq!(
    b_body, a_body,
    "interleaved local edits + one-way sequential imports converge (gap re-feed is complete)"
  );
  // And A's LIVE projection must equal its canonical materialization — a
  // clamped-away tracker hole shows up as a projection/doc offset mismatch.
  let live = {
    let projection = handle_a.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  let canonical = {
    let document = {
      let guard = gate_a.lock(GateHolder::Test).expect("gate healthy");
      flowstate_document::document_from_loro(guard.doc()).expect("materializes")
    };
    (0..document.paragraphs.len())
      .map(|ix| paragraph_text(&document, ix))
      .collect::<Vec<_>>()
  };
  assert_eq!(live, canonical, "A's live projection == canonical after clamped imports");
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
  handle
    .projection()
    .expect("projection")
    .ids
    .paragraph_ids
    .to_vec()
}

fn block_ids(handle: &LocalDocHandle) -> Vec<flowstate_document::BlockId> {
  handle
    .projection()
    .expect("projection")
    .ids
    .block_ids
    .to_vec()
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
  assert!(
    body_deleted.len() + 2048 < body_before.len(),
    "delete must remove a qualifying mass range"
  );
  assert_eq!(
    slot_direction(&gate),
    Some(loro::UndoOrRedo::Undo),
    "mass delete must arm the recorded inverse"
  );

  // Undo: fast path (flips the slot; the slow path never touches it).
  let outcome = handle.apply_undo().expect("undo runs");
  assert!(outcome.applied);
  assert_eq!(body_string(&gate), body_before, "undo must restore the exact body text + boundaries");
  assert_eq!(
    slot_direction(&gate),
    Some(loro::UndoOrRedo::Redo),
    "fast undo must flip the slot to redo"
  );
  assert_eq!(
    paragraph_ids(&handle),
    paragraph_ids_before,
    "undo must restore ORIGINAL paragraph identities"
  );
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
  peer
    .import_remote_update(&full_history)
    .expect("peer imports");
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  let peer_history = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard
    .import_remote_update(&peer_history)
    .expect("local imports peer seed");
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

/// A remote import whose change lands INSIDE the recorded hull kills the fast
/// path (stacks cleared) — undo still works through the checkout-based slow
/// path and keeps BOTH the restored content and the remote edit. (§oom-leads
/// #9: a hull-DISJOINT import now rebases instead — see
/// `recorded_inverse_rebases_across_hull_disjoint_remote_import`.)
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
  guard
    .import_remote_update(&peer_seed)
    .expect("local imports peer seed");
  drop(guard);
  let peer_vv = peer.doc().state_vv();

  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[2], 0),
      end: TextAnchor::new(ids[38], 3),
    })
    .expect("mass delete commits");
  assert_eq!(slot_direction(&gate), Some(loro::UndoOrRedo::Undo));

  // Remote edit lands between delete and undo, INSIDE the deleted range's
  // hull (the delete spans paragraphs 2..38 — body position 500 is well
  // within it), so the rebase is unsound and the stacks must clear.
  let peer_body = flowstate_document::loro_schema::body_text(peer.doc());
  peer_body.insert(500, "REMOTE").expect("peer insert");
  peer.doc().commit();
  let update = peer
    .doc()
    .export(loro::ExportMode::updates(&peer_vv))
    .expect("export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard.import_remote_update(&update).expect("import applies");
  drop(guard);
  assert_eq!(slot_direction(&gate), None, "an in-hull import must clear the recorded inverse");

  let outcome = handle
    .apply_undo()
    .expect("undo still runs via the slow path");
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
  assert_eq!(
    body_string(&gate)
      .chars()
      .filter(|ch| *ch == '\u{FFFC}')
      .count(),
    1
  );

  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(ids[1], 0),
      end: TextAnchor::new(ids[3], 4),
    })
    .expect("small delete across the equation commits");
  assert_eq!(slot_direction(&gate), None, "sub-threshold: no recorded-inverse capture");
  assert_eq!(
    body_string(&gate)
      .chars()
      .filter(|ch| *ch == '\u{FFFC}')
      .count(),
    0
  );

  let outcome = handle.apply_undo().expect("slow undo runs");
  assert!(outcome.applied);
  // Pre-existing: the placeholder is NOT restored by the checkout-based undo.
  assert_eq!(
    body_string(&gate)
      .chars()
      .filter(|ch| *ch == '\u{FFFC}')
      .count(),
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
    handle
      .projection()
      .expect("projection")
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  let before = styles_of(&handle);
  assert!(
    before.iter().all(|style| *style == ParagraphStyle::Normal),
    "seed paragraphs start Normal"
  );

  // Select-all restyle to a custom style.
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: ids.clone(),
      style: ParagraphStyle::Custom(3),
    })
    .expect("mass restyle commits");
  let after = styles_of(&handle);
  assert!(
    after
      .iter()
      .all(|style| *style == ParagraphStyle::Custom(3)),
    "restyle sets every paragraph"
  );
  assert_eq!(
    slot_direction(&gate),
    Some(loro::UndoOrRedo::Undo),
    "mass restyle must arm the recorded inverse"
  );

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
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  peer
    .import_remote_update(&full_history)
    .expect("peer imports local history");
  let peer_history = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard
    .import_remote_update(&peer_history)
    .expect("local imports peer seed");
  drop(guard);
  let styles = |doc: &DocumentProjection| -> Vec<ParagraphStyle> {
    doc
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect()
  };
  let local_fresh = {
    let guard = gate.lock(GateHolder::Test).expect("gate healthy");
    flowstate_document::document_from_loro(guard.doc()).expect("local materializes")
  };
  let peer_fresh = flowstate_document::document_from_loro(peer.doc()).expect("peer materializes");
  assert_eq!(
    styles(&peer_fresh),
    styles(&local_fresh),
    "replicas converge on the same styles after the fast-path history"
  );
  assert!(
    styles(&local_fresh)
      .iter()
      .all(|style| *style == ParagraphStyle::Normal),
    "converged on the undone (Normal) styles"
  );
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
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export A initial")
  };
  let mut core_b = CrdtRuntime::new_empty("undo-isolation-b").expect("peer B runtime");
  core_b
    .import_remote_update(&a_initial)
    .expect("B imports A initial state");
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
  let last = *projection_a
    .ids
    .paragraph_ids
    .last()
    .expect("last paragraph id");
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
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export B")
  };
  {
    let mut guard = gate_a.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard
      .import_remote_updates(&[&b_update])
      .expect("A imports B's tag")
  };

  // Read (styles, any-highlight) from the LIVE projection and from a fresh
  // canonical materialization — divergence between them localizes the fault.
  let live = |handle: &LocalDocHandle| {
    let doc = handle.projection().expect("live projection");
    let styles: Vec<ParagraphStyle> = doc
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect();
    let any_highlight = doc.paragraphs.iter().any(|paragraph| {
      paragraph
        .runs
        .iter()
        .any(|run| run.styles.highlight.is_some())
    });
    (styles, any_highlight)
  };
  let canonical = |gate: &Arc<WriteGate<CrdtRuntime>>| {
    let doc = {
      let guard = gate.lock(GateHolder::Test).expect("gate healthy");
      flowstate_document::document_from_loro(guard.doc()).expect("materialize canonical")
    };
    let styles: Vec<ParagraphStyle> = doc
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.style)
      .collect();
    let any_highlight = doc.paragraphs.iter().any(|paragraph| {
      paragraph
        .runs
        .iter()
        .any(|run| run.styles.highlight.is_some())
    });
    (styles, any_highlight)
  };

  // The remote tag defines the EXPECTED paragraph-style vector: A's local undo
  // touches only its run-style highlight, so paragraph styles must be identical
  // to B's tag result before undo, after undo, and after convergence.
  let expected_styles = canonical(&gate_b).0;
  assert!(
    expected_styles.contains(&tag),
    "B's tag must land on the durable paragraphs: {expected_styles:?}"
  );

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
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export A after undo")
  };
  {
    let mut guard = gate_b.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard
      .import_remote_updates(&[&a_after])
      .expect("B imports A's post-undo history")
  };
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
  assert_eq!(
    slot_direction(&gate),
    Some(loro::UndoOrRedo::Undo),
    "mass replace must arm the recorded inverse"
  );

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
    guard
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("export")
  };
  peer
    .import_remote_update(&full_history)
    .expect("peer imports local history");
  let peer_history = peer
    .doc()
    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
    .expect("peer export");
  let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate healthy");
  guard
    .import_remote_update(&peer_history)
    .expect("local imports peer seed");
  drop(guard);
  let local_text = body_string(&gate);
  let peer_text = flowstate_document::loro_schema::body_text(peer.doc()).to_string();
  assert_eq!(local_text, peer_text, "replicas converge on the replace-all fast-path history");
  assert!(!peer_text.contains("COLUMN"), "converged on the undone (original) content");
}

/// §act-twelve A12.2.2: Enter (`SplitParagraph`) is captured — undo merges the
/// split back (retiring the new records), redo re-splits with the SAME
/// paragraph id (identity preservation), and none of it clears deeper
/// captured entries.
#[test]
fn recorded_inverse_captures_split_paragraph() {
  let (handle, gate) = new_handle("split-capture");
  seed_mass_fragment(&handle, 8, None);
  let target = paragraph_ids(&handle)[3];
  let count_before = handle.projection().expect("projection").paragraphs.len();

  let outcome = handle
    .split_paragraph(SplitParagraphIntent {
      at: TextAnchor::new(target, 5),
      inherited_style: ParagraphStyle::Normal,
    })
    .expect("split commits");
  let _ = outcome;
  assert_eq!(stack_depths(&gate), (1, 0), "the split armed a recorded inverse");
  let split_ids: Vec<ParagraphId> = handle
    .projection()
    .expect("projection")
    .ids
    .paragraph_ids
    .to_vec();
  assert_eq!(split_ids.len(), count_before + 1);

  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "split undo stayed FAST");
  let after_undo = handle.projection().expect("projection");
  assert_eq!(after_undo.paragraphs.len(), count_before, "undo merged the split back");
  assert_live_equals_canonical(&handle, &gate, "split undo");

  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "split redo stayed FAST");
  let after_redo = handle.projection().expect("projection");
  assert_eq!(
    after_redo.ids.paragraph_ids.to_vec(),
    split_ids,
    "redo re-created the boundary with the SAME paragraph identity"
  );
  assert_live_equals_canonical(&handle, &gate, "split redo");
}

/// §act-twelve A12.2.2: Backspace at paragraph start (`JoinParagraphs`) is
/// captured — undo re-creates the boundary with the second paragraph's
/// ORIGINAL id and prior style; redo re-joins.
#[test]
fn recorded_inverse_captures_join_paragraphs() {
  let (handle, gate) = new_handle("join-capture");
  seed_mass_fragment(&handle, 8, None);
  let ids_before: Vec<ParagraphId> = handle
    .projection()
    .expect("projection")
    .ids
    .paragraph_ids
    .to_vec();
  let (first, second) = (ids_before[3], ids_before[4]);
  // Give the second paragraph a distinct style so undo must restore it.
  handle
    .set_paragraph_styles(SetParagraphStylesIntent {
      paragraphs: handle
        .projection()
        .expect("projection")
        .ids
        .paragraph_ids
        .to_vec(),
      style: ParagraphStyle::Custom(2),
    })
    .expect("restyle commits");

  handle
    .join_paragraphs(crate::local_write::JoinParagraphsIntent { first, second })
    .expect("join commits");
  assert_eq!(stack_depths(&gate), (2, 0), "restyle + join both recorded");

  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 1), "join undo stayed FAST");
  let after_undo = handle.projection().expect("projection");
  assert_eq!(
    after_undo.ids.paragraph_ids.to_vec(),
    ids_before,
    "undo restored the second paragraph's ORIGINAL id"
  );
  let second_ix = after_undo
    .ids
    .paragraph_ids
    .iter()
    .position(|id| *id == second)
    .expect("second restored");
  assert_eq!(
    after_undo.paragraphs[second_ix].style,
    ParagraphStyle::Custom(2),
    "undo restored the second paragraph's prior style"
  );
  assert_live_equals_canonical(&handle, &gate, "join undo");

  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (2, 0), "join redo stayed FAST");
  assert_eq!(
    handle.projection().expect("projection").paragraphs.len(),
    ids_before.len() - 1,
    "redo re-joined the paragraphs"
  );
  assert_live_equals_canonical(&handle, &gate, "join redo");
}

/// §act-twelve A12.2.2: `SetMarks` (bold/italic class) is captured — undo
/// restores the EXACT prior per-span run styles (the seeded content mixes
/// underline/strikethrough runs), redo re-applies the uniform mark.
#[test]
fn recorded_inverse_captures_set_marks() {
  let (handle, gate) = new_handle("marks-capture");
  seed_mass_fragment(&handle, 8, None);
  let target = paragraph_ids(&handle)[3];
  let runs_before = format!("{:?}", handle.projection().expect("projection").paragraphs[3].runs);

  handle
    .set_marks(SetMarksIntent {
      start: TextAnchor::new(target, 2),
      end: TextAnchor::new(target, 40),
      styles: flowstate_document::RunStyles {
        strikethrough: true,
        ..Default::default()
      },
    })
    .expect("marks commit");
  assert_eq!(stack_depths(&gate), (1, 0), "the mark op armed a recorded inverse");
  let runs_marked = format!("{:?}", handle.projection().expect("projection").paragraphs[3].runs);
  assert_ne!(runs_before, runs_marked, "the mark changed the runs");

  assert!(handle.apply_undo().expect("undo runs").applied);
  assert_eq!(stack_depths(&gate), (0, 1), "marks undo stayed FAST");
  assert_eq!(
    format!("{:?}", handle.projection().expect("projection").paragraphs[3].runs),
    runs_before,
    "undo restored the exact prior run styles"
  );
  assert_live_equals_canonical(&handle, &gate, "marks undo");

  assert!(handle.apply_redo().expect("redo runs").applied);
  assert_eq!(stack_depths(&gate), (1, 0), "marks redo stayed FAST");
  assert_eq!(
    format!("{:?}", handle.projection().expect("projection").paragraphs[3].runs),
    runs_marked,
    "redo re-applied the mark"
  );
  assert_live_equals_canonical(&handle, &gate, "marks redo");
}

/// §act-twelve A12.2.2: the typing-session shape — keystrokes, Enter, marks,
/// restyle, backspace-join chained; EVERY undo and redo must stay on the
/// fast path (the stacks prove it: each fast step moves one entry).
#[test]
fn recorded_inverse_chains_full_typing_session() {
  let (handle, gate) = new_handle("session-capture");
  seed_mass_fragment(&handle, 8, None);
  let target = paragraph_ids(&handle)[2];
  let original_text = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };

  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(target, 0),
      text: "abc".into(),
      style_override: None,
    })
    .expect("keystrokes");
  handle
    .split_paragraph(SplitParagraphIntent {
      at: TextAnchor::new(target, 3),
      inherited_style: ParagraphStyle::Normal,
    })
    .expect("enter");
  handle
    .set_marks(SetMarksIntent {
      start: TextAnchor::new(target, 0),
      end: TextAnchor::new(target, 3),
      styles: flowstate_document::RunStyles {
        direct_underline: true,
        ..Default::default()
      },
    })
    .expect("bold-ish");
  handle
    .set_paragraph_style(crate::local_write::SetParagraphStyleIntent {
      paragraph: target,
      style: ParagraphStyle::Custom(3),
    })
    .expect("restyle one");
  assert_eq!(stack_depths(&gate), (4, 0), "all four ops recorded");

  for step in 0..4 {
    assert!(handle.apply_undo().expect("undo runs").applied, "undo {step} applied");
  }
  assert_eq!(stack_depths(&gate), (0, 4), "ALL undos stayed on the fast path");
  let live = {
    let projection = handle.projection().expect("projection");
    (0..projection.paragraphs.len())
      .map(|ix| paragraph_text(&projection, ix))
      .collect::<Vec<_>>()
  };
  assert_eq!(live, original_text, "the session fully unwound");
  assert_live_equals_canonical(&handle, &gate, "session unwind");

  for step in 0..4 {
    assert!(handle.apply_redo().expect("redo runs").applied, "redo {step} applied");
  }
  assert_eq!(stack_depths(&gate), (4, 0), "ALL redos stayed on the fast path");
  assert_live_equals_canonical(&handle, &gate, "session replay");
}
