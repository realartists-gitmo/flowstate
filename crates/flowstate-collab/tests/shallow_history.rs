//! §A12.1.3 slice 4 gates (design G2): a SHALLOW session must serve
//! far-behind peers from reconstructed full history (never the silently
//! truncating live export), and must durably absorb remote ops that depend
//! on pre-root history (the "rebase reopen" contract) instead of failing or
//! dropping them.

#[cfg(test)]
mod tests {
  mod tests {
    use flowstate_collab::crdt_runtime::{CrdtRuntime, RuntimeEvent};
    use flowstate_collab::local_write::{InsertTextIntent, LocalDocHandle, LocalWriteConfig, TextAnchor};
    use flowstate_document::{DocumentPackage, loro_schema::body_text, loro_schema::new_loro_document};
    use loro::{ExportMode, LoroDoc};

    /// Full source doc with pre-root history + a package carrying a shallow
    /// chunk rooted mid-history, plus a far-behind peer forked BEFORE the root.
    fn shallow_session_with_behind_peer() -> (CrdtRuntime, LoroDoc, LoroDoc) {
      let source = new_loro_document("Shallow serve").expect("doc");
      let text = body_text(&source);
      text
        .insert(text.len_unicode(), "ancient shared history")
        .expect("insert");
      source.commit();

      // The far-behind peer last synced BEFORE the shallow root.
      let behind = LoroDoc::new();
      behind
        .import(&source.export(ExportMode::Snapshot).expect("snapshot"))
        .expect("behind bootstrap");

      text
        .insert(text.len_unicode(), " | past the root")
        .expect("insert");
      source.commit();
      let root = source.state_frontiers();
      text
        .insert(text.len_unicode(), " | recent window")
        .expect("insert");
      source.commit();

      let mut package = DocumentPackage::from_loro_snapshot(&source, "Shallow serve").expect("package");
      package.compact_to_snapshot(&source).expect("compact");
      package
        .record_shallow_snapshot(&source, &root)
        .expect("record shallow");
      let live = package
        .load_loro_doc_shallow()
        .expect("shallow load")
        .expect("shallow chunk present");
      assert!(live.is_shallow());
      let runtime = CrdtRuntime::from_doc(live, Some(package), None).expect("runtime");
      (runtime, source, behind)
    }

    #[test]
    fn below_root_pull_is_served_from_full_history() {
      let (runtime, source, behind) = shallow_session_with_behind_peer();

      // The live doc alone would silently truncate this export; the runtime
      // must serve it from the reconstructed full history.
      let updates = runtime
        .export_updates_for(&behind.oplog_vv())
        .expect("below-root serve");
      let status = behind.import(&updates).expect("behind import");
      assert!(status.pending.is_none(), "the served range must be complete: {status:?}");
      assert_eq!(
        body_text(&behind).to_string(),
        body_text(&source).to_string(),
        "the far-behind peer must converge to the tip"
      );
    }

    #[test]
    fn below_root_import_is_absorbed_durably() {
      let (mut runtime, _source, behind) = shallow_session_with_behind_peer();

      // The behind peer typed while offline: its ops depend on pre-root history.
      let behind_text = body_text(&behind);
      behind_text
        .insert(behind_text.len_unicode(), " | offline peer edit")
        .expect("behind edit");
      behind.commit();
      let foreign = behind
        .export(ExportMode::updates(&loro::VersionVector::default()))
        .expect("behind export");

      let live_text_before = body_text(runtime.doc()).to_string();
      let events = runtime
        .import_remote_updates(&[&foreign])
        .expect("below-root import must absorb, not fail");
      assert!(
        events
          .iter()
          .flatten()
          .any(|event| matches!(event, RuntimeEvent::HistoryRebaseRequired { .. })),
        "the session must be told a reopen is required: {events:?}"
      );
      // The in-memory shallow doc is untouched...
      assert_eq!(body_text(runtime.doc()).to_string(), live_text_before);
      // ...but the PACKAGE now carries the merged full history.
      let merged = runtime
        .package()
        .expect("runtime keeps its package")
        .load_loro_doc()
        .expect("merged package loads full");
      assert!(!merged.is_shallow());
      let merged_text = body_text(&merged).to_string();
      assert!(
        merged_text.contains("offline peer edit"),
        "the foreign edit must be durably merged: {merged_text:?}"
      );
      assert!(merged_text.contains("recent window"), "local history must survive the merge");
    }

    #[test]
    fn shallow_session_undo_and_edits_work() {
      // G5 smoke: a session opened from a shallow snapshot types and undoes on
      // the normal write path; every checkout target is post-root by
      // construction (the root is fixed at open).
      let (runtime, _source, _behind) = shallow_session_with_behind_peer();
      assert!(runtime.doc().is_shallow());
      let (handle, _gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
      let paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
      let before = handle.projection().expect("projection");
      let before_text = flowstate_document::paragraph_text(&before, 0);
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: " typed on the shallow session".to_string(),
          style_override: None,
        })
        .expect("insert on shallow session");
      let outcome = handle.apply_undo().expect("undo on shallow session");
      assert!(outcome.applied, "undo must apply on a shallow session");
      let after = handle.projection().expect("projection");
      assert_eq!(flowstate_document::paragraph_text(&after, 0), before_text);
    }

    #[test]
    fn shallow_session_converges_over_repeated_exchange() {
      // G7 (targeted): repeated bidirectional exchange between a shallow
      // session and an in-window peer stays convergent.
      let (mut runtime, source, _behind) = shallow_session_with_behind_peer();
      // Peer bootstraps from the CURRENT tip (inside the window).
      let peer = LoroDoc::new();
      peer
        .import(&source.export(ExportMode::Snapshot).expect("snapshot"))
        .expect("peer bootstrap");

      for round in 0..5 {
        // Peer edit → shallow session.
        let peer_from = peer.oplog_vv();
        let peer_text = body_text(&peer);
        peer_text
          .insert(peer_text.len_unicode(), &format!(" | peer round {round}"))
          .expect("peer edit");
        peer.commit();
        let update = peer
          .export(ExportMode::updates(&peer_from))
          .expect("peer export");
        runtime
          .import_remote_updates(&[&update])
          .expect("session import");

        // Shallow session edit → peer.
        let session_from = peer.oplog_vv();
        let session_text = body_text(runtime.doc());
        session_text
          .insert(session_text.len_unicode(), &format!(" | session round {round}"))
          .expect("session edit");
        runtime.doc().commit();
        let update = runtime
          .export_updates_for(&session_from)
          .expect("session export");
        peer.import(&update).expect("peer import");

        assert_eq!(
          body_text(runtime.doc()).to_string(),
          body_text(&peer).to_string(),
          "round {round} must converge"
        );
      }
    }

    #[test]
    fn routine_checkpoint_skips_consolidation_but_stays_reopenable() {
      // §A12.4.1: a revision checkpoint below the segment threshold keeps the
      // old full snapshot + segment chain (no full-history export) yet the
      // written package reopens to the same tip, with a fresh shallow
      // accelerator.
      let (mut runtime, _source, _behind) = shallow_session_with_behind_peer();
      let dir = tempfile::tempdir().expect("tempdir");
      let path = dir.path().join("incremental.db8");
      // (Edits go through the persist path in real sessions, keeping the
      // package tip chained; the checkpoint's own meta/revision commit is the
      // delta under test here.)
      let snapshots_before = runtime.package().expect("package").loro_snapshots.len();
      let (job, _events) = runtime
        .begin_checkpoint("Routine save", Some(path.clone()))
        .expect("begin")
        .expect("package present");
      let (package, wrote) = job.run();
      assert!(wrote.expect("job io"), "checkpoint must reach disk");
      assert_eq!(
        package.loro_snapshots.len(),
        snapshots_before,
        "no full-history export below the threshold (a shallow session also skips the revision-load accelerator chunk)"
      );
      assert!(!package.loro_update_segments.is_empty(), "the segment chain must survive");
      // §A13.4.3: the accelerator deliberately LAGS on routine checkpoints
      // (re-minting costs a ~200ms shallow export; staleness only costs the
      // next open a cheap segment replay). Present is the contract; the
      // reopen assertions below prove the lagging chunk still reconstructs
      // the tip.
      assert!(package.latest_shallow_snapshot().is_some(), "accelerator chunk must survive");
      runtime.finish_checkpoint(package, true);

      let reopened = DocumentPackage::read(&path).expect("reopen");
      let full = reopened.load_loro_doc().expect("full load");
      assert_eq!(
        body_text(&full).to_string(),
        body_text(runtime.doc()).to_string(),
        "the incremental checkpoint must reopen to the live tip"
      );
      let shallow = reopened
        .load_loro_doc_shallow()
        .expect("shallow load")
        .expect("chunk");
      assert_eq!(body_text(&shallow).to_string(), body_text(&full).to_string());
    }

    #[test]
    fn shallow_session_slow_path_undo_is_bounded() {
      // §A13.3.0: the slow-path (UndoManager) undo on a SHALLOW-opened doc.
      // The 1.0-1.6s tail was measured on full-history in-memory docs; the
      // shallow root must bound every walk to the session window. Remote
      // chatter lands INSIDE the typed hull so the recorded fast path
      // truncates and undo falls through to the checkout-based path.
      let (runtime, source, _behind) = shallow_session_with_behind_peer();
      assert!(runtime.doc().is_shallow());
      let peer = LoroDoc::new();
      peer
        .import(&source.export(ExportMode::Snapshot).expect("snapshot"))
        .expect("peer bootstrap");
      let (handle, gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
      let paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
      for i in 0..40 {
        handle
          .insert_text(InsertTextIntent {
            at: TextAnchor::new(paragraph, usize::MAX),
            text: format!(" typed{i}"),
            style_override: None,
          })
          .expect("typed edit");
        if i % 4 == 0 {
          // Remote chatter at the head of the same paragraph — inside the
          // recorded hull, forcing truncation of the fast-path stacks.
          let from = peer.oplog_vv();
          {
            let guard = gate
              .lock(flowstate_collab::local_write::GateHolder::ExportUpdates)
              .expect("gate");
            let update = guard
              .doc()
              .export(ExportMode::updates(&peer.oplog_vv()))
              .expect("session export");
            drop(guard);
            peer.import(&update).expect("peer catch-up")
          };
          let peer_text = body_text(&peer);
          // INSIDE the typed tail (hull-overlapping): forces at-coordinate
          // truncation of the recorded stacks, so undo falls to the slow path.
          let at = peer_text.len_unicode().saturating_sub(2);
          peer_text.insert(at, "r").expect("peer edit");
          peer.commit();
          let update = peer
            .export(ExportMode::updates(&from))
            .expect("peer export");
          let mut guard = gate
            .lock(flowstate_collab::local_write::GateHolder::ImportChunk)
            .expect("gate");
          guard
            .import_remote_updates(&[&update])
            .expect("session import");
        }
      }
      // Undo the whole session; time the worst step (slow path engages once
      // the truncated stacks run dry).
      let mut worst = std::time::Duration::ZERO;
      let mut applied = 0;
      for _ in 0..40 {
        let t = std::time::Instant::now();
        let outcome = handle.apply_undo().expect("undo");
        worst = worst.max(t.elapsed());
        if !outcome.applied {
          break;
        }
        applied += 1;
      }
      eprintln!("[shallow-undo-timing] applied={applied} worst_step={worst:?}");
      assert!(applied > 0, "at least one undo must apply");
      assert!(
        worst < std::time::Duration::from_millis(2000),
        "slow-path undo on a shallow session took {worst:?} — the root is not bounding the walk"
      );
    }
  }
}
