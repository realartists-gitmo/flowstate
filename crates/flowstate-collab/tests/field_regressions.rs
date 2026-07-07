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
}
