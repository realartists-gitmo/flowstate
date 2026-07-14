//! §A14.1.1 gates: a MASS local commit publishes as bounded slices; a
//! receiver imports them as separate small gate holds (the priority lane
//! admits keystrokes between them) and converges to the sender byte-for-byte.

#[cfg(test)]
mod tests {
  mod tests {
    use flowstate_collab::crdt_runtime::{CrdtRuntime, RuntimeEvent, SemanticCommand};
    use flowstate_document::{RunStyles, loro_schema::body_text};
    use loro::LoroDoc;

    fn mass_commit_events() -> (CrdtRuntime, Vec<u8>, Vec<Vec<u8>>) {
      let mut runtime = CrdtRuntime::new_empty("Mass chunking").expect("runtime");
      // Receivers in the field share history up to the sender's last publish;
      // capture that baseline (genesis/schema ops) before the mass commit.
      let baseline = runtime
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("baseline export");
      // One mass commit: a single ~40k-char insert (40k op atoms — well past
      // the 2048-atom slice budget).
      let big = "mass chunked payload ".repeat(2_000);
      let events = runtime
        .command(SemanticCommand::InsertText {
          unicode_index: 1,
          text: big,
          styles: RunStyles::default(),
        })
        .expect("mass insert");
      let updates: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
          RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
          _ => None,
        })
        .collect();
      (runtime, baseline, updates)
    }

    #[test]
    fn mass_local_commit_publishes_bounded_slices() {
      let (runtime, baseline, updates) = mass_commit_events();
      assert!(
        updates.len() >= 10,
        "a 40k-atom commit must slice (got {} update event(s))",
        updates.len()
      );
      // Every slice imports standalone IN ORDER on a peer sharing the
      // baseline, and the peer converges to the sender's exact text.
      let peer = LoroDoc::new();
      peer.import(&baseline).expect("baseline import");
      for (ix, update) in updates.iter().enumerate() {
        let status = peer
          .import(update)
          .unwrap_or_else(|error| panic!("slice {ix} import failed: {error}"));
        assert!(status.pending.is_none(), "slice {ix} must be causally self-contained: {status:?}");
      }
      assert_eq!(body_text(&peer).to_string(), body_text(runtime.doc()).to_string());
    }

    #[test]
    fn chunked_slices_import_through_the_runtime_receiver() {
      let (sender, baseline, updates) = mass_commit_events();
      let mut receiver = CrdtRuntime::from_doc(
        {
          let doc = LoroDoc::new();
          flowstate_document::loro_schema::configure_text_styles(&doc);
          doc.import(&baseline).expect("baseline import");
          doc
        },
        None,
        None,
      )
      .expect("receiver runtime");
      let mut import_holds_over_budget = 0;
      for update in &updates {
        let t = std::time::Instant::now();
        receiver
          .import_remote_updates(&[update.as_slice()])
          .expect("receiver import");
        // Counter-gate (generous for shared-machine noise): one slice's whole
        // import+derive must stay far below the old ~240ms mass hold.
        if t.elapsed() > std::time::Duration::from_millis(50) {
          import_holds_over_budget += 1;
        }
      }
      assert_eq!(import_holds_over_budget, 0, "every slice import must be a small hold");
      assert_eq!(body_text(receiver.doc()).to_string(), body_text(sender.doc()).to_string());
    }

    #[test]
    fn each_slice_emits_a_projection_update() {
      // §A14.2: the receiver paints PROGRESSIVELY — every chunked slice import
      // yields its own projection event, so the user sees the mass op land in
      // increments instead of one 240ms freeze.
      let (sender, baseline, updates) = mass_commit_events();
      let mut receiver = CrdtRuntime::from_doc(
        {
          let doc = LoroDoc::new();
          flowstate_document::loro_schema::configure_text_styles(&doc);
          doc.import(&baseline).expect("baseline import");
          doc
        },
        None,
        None,
      )
      .expect("receiver runtime");
      let mut slices_with_projection = 0;
      for update in &updates {
        let batches = receiver
          .import_remote_updates(&[update.as_slice()])
          .expect("import");
        let has_projection = batches
          .iter()
          .flatten()
          .any(|event| matches!(event, RuntimeEvent::ProjectionUpdated { .. } | RuntimeEvent::ProjectionPatched { .. }));
        if has_projection {
          slices_with_projection += 1;
        }
      }
      assert!(
        slices_with_projection >= updates.len() - 1,
        "nearly every slice must paint ({slices_with_projection}/{} slices had a projection event)",
        updates.len()
      );
      assert_eq!(body_text(receiver.doc()).to_string(), body_text(sender.doc()).to_string());
    }
  }
}
