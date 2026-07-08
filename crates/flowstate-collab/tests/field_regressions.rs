//! Regression proofs for the 2026-07-07 04:29–04:36 field-run trio:
//! (A) the projection-stream ordering race ("canonical patch batch base
//! frontier mismatch" cascades), and (C) package exports holding the write
//! gate / I/O loop for up to 18.3s (frozen typing, starved imports, peer pull
//! timeouts). (B — the `session_timers` drift-replace leftover — is deleted
//! code; its regression is (A)'s ordered stream.)

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::doc_io::DocIoHandle;
  use flowstate_collab::local_write::{
    GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteAuthority, LocalWriteConfig, SplitParagraphIntent, TextAnchor, WriteGate,
  };
  use gpui_flowtext::ProjectionStreamItem;
  use gpui_flowtext::{DocumentProjection, ParagraphStyle};

  fn new_handle(title: &str) -> (LocalDocHandle, Arc<WriteGate<CrdtRuntime>>) {
    let core = CrdtRuntime::new_empty(title).expect("runtime");
    LocalDocHandle::new(core, LocalWriteConfig::default())
  }

  fn body_string(gate: &Arc<WriteGate<CrdtRuntime>>) -> String {
    let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
    flowstate_document::loro_schema::body_text(guard.doc()).to_string()
  }

  /// Simulated editor: applies stream items exactly the way
  /// `sync_projection_from_authority` does, and PANICS on any base-frontier
  /// mismatch — the field symptom that must now be unreachable.
  fn apply_stream(editor_projection: &mut DocumentProjection, items: Vec<ProjectionStreamItem>) {
    for item in items {
      match item {
        ProjectionStreamItem::Patches(batch) => {
          if batch.new_frontier == editor_projection.frontier {
            continue;
          }
          assert_eq!(
            batch.base_frontier, editor_projection.frontier,
            "ORDERING RACE: patch base does not match the editor frontier (the field bug)"
          );
          gpui_flowtext::apply_projection_patch_batch(editor_projection, &batch).expect("ordered batch applies");
        },
        ProjectionStreamItem::Replace(document) => *editor_projection = *document,
      }
    }
  }

  /// Field bug A: a remote import lands between two local intents. The old
  /// split delivery (synchronous local batch + async remote batch) applied the
  /// second local batch onto a stale editor frontier and dropped it. The
  /// ordered stream must interleave the import batch at its committed position
  /// so every item applies cleanly.
  #[test]
  fn ordered_stream_interleaves_imports_between_local_intents() {
    let (handle, gate) = new_handle("ordering");
    let paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];

    // A converged peer to produce genuine remote traffic.
    let mut peer = CrdtRuntime::new_empty("ordering").expect("peer");
    let seed = {
      let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("export")
    };
    peer.import_remote_update(&seed).expect("peer seed");
    let peer_seed = peer
      .doc()
      .export(loro::ExportMode::updates(&loro::VersionVector::default()))
      .expect("peer export");
    let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
    guard.import_remote_update(&peer_seed).expect("local imports peer seed");
    drop(guard);
    let mut peer_vv = peer.doc().state_vv();

    // Editor attaches: canonical projection + drain whatever the seed produced.
    let mut editor = LocalWriteAuthority::canonical_projection(&handle).expect("attach projection");
    let _ = handle.drain_projection_stream().expect("attach drain");

    for round in 0..12 {
      // Local intent 1.
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("l{round}a"),
          style_override: None,
        })
        .expect("local intent");

      // Remote import lands BETWEEN the editor's syncs (peer edits + we import
      // while the editor hasn't drained yet).
      let peer_body = flowstate_document::loro_schema::body_text(peer.doc());
      peer_body.insert(1, "R").expect("peer insert");
      peer.doc().commit();
      let update = peer.doc().export(loro::ExportMode::updates(&peer_vv)).expect("export");
      peer_vv = peer.doc().state_vv();
      let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
      guard.import_remote_update(&update).expect("import");
      drop(guard);

      // Local intent 2 — under the OLD split delivery this batch's base was
      // ahead of the editor's frontier and got dropped.
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("l{round}b"),
          style_override: None,
        })
        .expect("local intent");

      // Editor drains ONCE per round: local batch, import batch(es), local
      // batch — in commit order, every base matching.
      apply_stream(&mut editor, handle.drain_projection_stream().expect("drain"));
    }

    // The editor's replica equals the canonical projection exactly.
    let canonical = LocalWriteAuthority::canonical_projection(&handle).expect("canonical");
    assert_eq!(editor.frontier, canonical.frontier, "editor tracked every committed change");
    assert_eq!(editor.paragraphs.len(), canonical.paragraphs.len());
    for ix in 0..editor.paragraphs.len() {
      assert_eq!(
        flowstate_document::paragraph_text(&editor, ix),
        flowstate_document::paragraph_text(&canonical, ix),
        "paragraph {ix} text"
      );
    }
  }

  /// 2026-07-07 ctrl-A + undo field freeze: undo/redo of a mass styled delete
  /// must complete, converge, and match a fresh full rematerialization — the
  /// path crosses the linear event compose (vendored), the dead-anchor-skip
  /// materializer, and the O(1)-per-defect repair pass, each of which was a
  /// multi-minute freeze in the field.
  #[test]
  fn mass_delete_undo_redo_round_trips() {
    let (handle, _gate) = new_handle("mass-undo");
    let mut paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
    for i in 0..60 {
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("styled paragraph {i} with enough text to matter"),
          style_override: Some(flowstate_document::RunStyles {
            semantic: flowstate_document::RunSemanticStyle::Custom(2),
            ..Default::default()
          }),
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
    let mut editor = LocalWriteAuthority::canonical_projection(&handle).expect("attach");
    let _ = handle.drain_projection_stream().expect("attach drain");
    let full_text = |projection: &DocumentProjection| {
      (0..projection.paragraphs.len())
        .map(|ix| flowstate_document::paragraph_text(projection, ix))
        .collect::<Vec<_>>()
    };
    let before = full_text(&editor);

    // ctrl-A delete.
    let projection = handle.projection().expect("projection");
    let first = projection.ids.paragraph_ids[0];
    let last = *projection.ids.paragraph_ids.last().expect("paragraphs");
    handle
      .delete_range(flowstate_collab::local_write::DeleteRangeIntent {
        start: TextAnchor::new(first, 0),
        end: TextAnchor::new(last, usize::MAX),
      })
      .expect("select-all delete");
    apply_stream(&mut editor, handle.drain_projection_stream().expect("drain"));
    assert_eq!(editor.paragraphs.len(), 1, "select-all delete leaves the sentinel paragraph");

    // Undo restores every paragraph's text; redo empties again; both keep the
    // editor byte-identical to canonical.
    handle.apply_undo().expect("undo mass delete");
    apply_stream(&mut editor, handle.drain_projection_stream().expect("drain"));
    let canonical = LocalWriteAuthority::canonical_projection(&handle).expect("canonical");
    assert_eq!(editor.frontier, canonical.frontier);
    assert_eq!(full_text(&editor), before, "undo restored the full document text");
    assert_eq!(full_text(&editor), full_text(&canonical));

    handle.apply_redo().expect("redo mass delete");
    apply_stream(&mut editor, handle.drain_projection_stream().expect("drain"));
    let canonical = LocalWriteAuthority::canonical_projection(&handle).expect("canonical");
    assert_eq!(editor.frontier, canonical.frontier);
    assert_eq!(editor.paragraphs.len(), canonical.paragraphs.len());
    assert_eq!(canonical.paragraphs.len(), 1, "redo re-empties the document");
  }

  /// Spec §6-R: a remote peer's structural edits (Enter/Backspace storms) must
  /// take the REGIONAL rematerializer — exact patches (`ProjectionPatched`),
  /// never a full-document rebuild (`ProjectionUpdated`) — and the ordered
  /// stream must keep the editor byte-identical to canonical throughout.
  #[test]
  fn remote_structural_chunks_take_the_regional_path() {
    let (handle, gate) = new_handle("regional");
    let mut paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];

    // A meaty multi-paragraph doc so regions are genuine sub-spans.
    for i in 0..80 {
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

    // Peer forks from the doc's own snapshot; export baseline predates runtime
    // startup so its chunks are causally complete.
    let snapshot = {
      let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard.doc().export(loro::ExportMode::Snapshot).expect("snapshot")
    };
    let peer_doc = loro::LoroDoc::new();
    peer_doc.import_with(&snapshot, "remote").expect("peer join");
    let mut peer_vv = peer_doc.state_vv();
    let peer_core = flowstate_collab::crdt_runtime::CrdtRuntime::from_doc(peer_doc, None, None).expect("peer runtime");
    let (peer_handle, peer_gate) = LocalDocHandle::new(
      peer_core,
      flowstate_collab::local_write::LocalWriteConfig {
        release_audit_sample: None,
      },
    );
    let peer_target = peer_handle.projection().expect("peer projection").ids.paragraph_ids[40];

    // Editor attaches to the main handle.
    let mut editor = LocalWriteAuthority::canonical_projection(&handle).expect("attach projection");
    let _ = handle.drain_projection_stream().expect("attach drain");

    // Storm: 10 chunks, each a typing burst + an Enter (structural), then a
    // Backspace-join chunk and a cross-paragraph delete chunk.
    let mut structural_chunks = 0_usize;
    let mut rebuilds = 0_usize;
    for round in 0..12 {
      match round {
        10 => {
          // Backspace at paragraph start: join the two peers' split rows.
          let projection = peer_handle.projection().expect("peer projection");
          let ix = projection.ids.paragraph_ids.iter().position(|id| *id == peer_target).expect("target present");
          let second = projection.ids.paragraph_ids[ix + 1];
          peer_handle
            .join_paragraphs(flowstate_collab::local_write::JoinParagraphsIntent {
              first: peer_target,
              second,
            })
            .expect("peer join");
        },
        11 => {
          // Cross-paragraph delete spanning a boundary.
          let projection = peer_handle.projection().expect("peer projection");
          let ix = projection.ids.paragraph_ids.iter().position(|id| *id == peer_target).expect("target present");
          let next = projection.ids.paragraph_ids[ix + 1];
          peer_handle
            .delete_range(flowstate_collab::local_write::DeleteRangeIntent {
              start: TextAnchor::new(peer_target, 3),
              end: TextAnchor::new(next, 2),
            })
            .expect("peer cross-paragraph delete");
        },
        _ => {
          for _ in 0..3 {
            peer_handle
              .insert_text(InsertTextIntent {
                at: TextAnchor::new(peer_target, usize::MAX),
                text: "r".to_string(),
                style_override: None,
              })
              .expect("peer typing");
          }
          peer_handle
            .split_paragraph(SplitParagraphIntent {
              at: TextAnchor::new(peer_target, usize::MAX),
              inherited_style: ParagraphStyle::Normal,
            })
            .expect("peer split");
        },
      }
      let update = {
        let guard = peer_gate.lock(GateHolder::ExportUpdates).expect("gate");
        let update = guard.doc().export(loro::ExportMode::updates(&peer_vv)).expect("export");
        peer_vv = guard.doc().state_vv();
        update
      };
      structural_chunks += 1;
      let mut guard = gate.lock(GateHolder::ImportChunk).expect("gate");
      let events = guard.import_remote_update(&update).expect("import");
      drop(guard);
      for event in &events {
        if matches!(event, flowstate_collab::crdt_runtime::RuntimeEvent::ProjectionUpdated { .. }) {
          rebuilds += 1;
        }
      }
      apply_stream(&mut editor, handle.drain_projection_stream().expect("drain"));
    }
    assert_eq!(rebuilds, 0, "all {structural_chunks} structural chunks must take the regional patched path, never a full rebuild");

    let canonical = LocalWriteAuthority::canonical_projection(&handle).expect("canonical");
    assert_eq!(editor.frontier, canonical.frontier, "editor tracked every structural import");
    assert_eq!(editor.paragraphs.len(), canonical.paragraphs.len());
    for ix in 0..editor.paragraphs.len() {
      assert_eq!(
        flowstate_document::paragraph_text(&editor, ix),
        flowstate_document::paragraph_text(&canonical, ix),
        "paragraph {ix} text"
      );
    }
  }

  /// Field bug C: package exports held the `DocumentService` gate (and the
  /// serial I/O loop) for up to 18.3s — typing froze behind the gate and
  /// imports starved. Post-fix the export forks under a SHORT hold and
  /// assembles off-thread: typing during an in-flight package export must stay
  /// responsive, and no non-import gate hold may approach the old freeze.
  #[test]
  fn package_export_does_not_stall_typing() {
    let (handle, gate) = new_handle("package-stall");
    let projection = handle.projection().expect("projection");
    let mut paragraph = projection.ids.paragraph_ids[0];

    // A meaty document so assembly is measurably slower than a fork.
    for i in 0..300 {
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("paragraph {i} with a reasonable amount of body text for sizing"),
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

    let io = DocIoHandle::spawn(Arc::clone(&gate)).expect("io service");
    // Fire the package export (the recovery-snapshot shape from the field
    // logs) and type while it assembles.
    let export = std::thread::spawn(move || pollster::block_on(io.package_bytes("Recovery snapshot".to_string())));

    let mut worst_keystroke = std::time::Duration::ZERO;
    for i in 0..40 {
      let started = std::time::Instant::now();
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("{i}"),
          style_override: None,
        })
        .expect("typing during export");
      worst_keystroke = worst_keystroke.max(started.elapsed());
    }
    let bytes = export.join().expect("export thread").expect("package bytes");
    assert!(!bytes.is_empty(), "export still produces the package");

    // Generous debug-build ceiling: the OLD shape held the gate for the whole
    // assembly (seconds); the fork-only hold is milliseconds.
    assert!(
      worst_keystroke < std::time::Duration::from_millis(750),
      "typing stalled behind the package export: worst keystroke {worst_keystroke:?}"
    );
  }

  /// Longevity: a DEEP undo/redo chain (every op undone to the seed, then
  /// fully redone) round-trips exactly and keeps the maintained projection
  /// equal to a fresh rematerialization at every checkpoint — undo depth must
  /// never accumulate drift.
  #[test]
  fn deep_undo_redo_chain_round_trips() {
    let (handle, gate) = new_handle("deep-undo");
    let mut paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
    let mut checkpoints: Vec<String> = vec![body_string(&gate)];
    for step in 0..60 {
      if step % 7 == 6 {
        handle
          .split_paragraph(SplitParagraphIntent {
            at: TextAnchor::new(paragraph, usize::MAX),
            inherited_style: ParagraphStyle::Normal,
          })
          .expect("split");
        paragraph = *handle.projection().expect("projection").ids.paragraph_ids.last().expect("paragraph");
      } else {
        handle
          .insert_text(InsertTextIntent {
            at: TextAnchor::new(paragraph, usize::MAX),
            text: format!("w{step} "),
            style_override: None,
          })
          .expect("insert");
      }
      checkpoints.push(body_string(&gate));
    }

    let self_consistent = |label: &str| {
      let maintained = handle.projection().expect("projection");
      let fresh = LocalWriteAuthority::canonical_projection(&handle).expect("canonical");
      assert_eq!(maintained.paragraphs.len(), fresh.paragraphs.len(), "{label}: paragraph count drifted");
      for ix in 0..maintained.paragraphs.len() {
        assert_eq!(
          flowstate_document::paragraph_text(&maintained, ix),
          flowstate_document::paragraph_text(&fresh, ix),
          "{label}: paragraph {ix} drifted"
        );
      }
    };

    // Unwind the WHOLE chain, checking text at every step and consistency
    // every tenth.
    for step in (0..60).rev() {
      handle.apply_undo().expect("undo");
      assert_eq!(body_string(&gate), checkpoints[step], "undo depth {} text mismatch", 60 - step);
      if step % 10 == 0 {
        self_consistent("undo checkpoint");
      }
    }
    // And forward again.
    for step in 0..60 {
      handle.apply_redo().expect("redo");
      assert_eq!(body_string(&gate), checkpoints[step + 1], "redo step {step} text mismatch");
      if step % 10 == 0 {
        self_consistent("redo checkpoint");
      }
    }
  }

  /// Revisions during live edits: opening and forking a stored revision is a
  /// READ — it must not disturb the maintained projection, the ordered
  /// stream, or subsequent undo.
  #[test]
  fn revision_open_during_live_edits_is_inert() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("live-revisions.db8");
    let doc = flowstate_document::new_loro_document("Live").expect("doc");
    let mut package = flowstate_document::DocumentPackage::from_loro_snapshot(&doc, "Live").expect("package");
    let revision_id = package
      .create_named_revision(&doc, "Blank", "Blank document", None, None)
      .expect("revision");
    package.write(&path).expect("write");

    let runtime = CrdtRuntime::open_package(&path).expect("open");
    let (handle, gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
    let paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: "typed before open".into(),
        style_override: None,
      })
      .expect("insert");

    // Open + fork the revision mid-session.
    let mut guard = gate.lock(GateHolder::UndoRedo).expect("gate");
    let events = guard
      .command(flowstate_collab::crdt_runtime::SemanticCommand::OpenRevision { revision_id })
      .expect("open revision");
    assert_eq!(events.len(), 1, "revision open emits exactly the read event");
    let events = guard
      .command(flowstate_collab::crdt_runtime::SemanticCommand::ForkRevision { revision_id })
      .expect("fork revision");
    assert_eq!(events.len(), 1, "revision fork emits exactly the fork event");
    drop(guard);;

    // The live document is untouched: same text, editing + undo keep working.
    assert!(body_string(&gate).contains("typed before open"));
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: " and after".into(),
        style_override: None,
      })
      .expect("insert after open");
    handle.apply_undo().expect("undo");
    let text = body_string(&gate);
    assert!(text.contains("typed before open") && !text.contains("and after"), "undo reverts only the live edit: {text:?}");
    let maintained = handle.projection().expect("projection");
    let fresh = LocalWriteAuthority::canonical_projection(&handle).expect("canonical");
    assert_eq!(maintained.paragraphs.len(), fresh.paragraphs.len());
  }
}
