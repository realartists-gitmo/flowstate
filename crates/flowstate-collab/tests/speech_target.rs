//! CT-S3: the team speech-doc self-marker — replication, atomic LWW under
//! concurrent designation, undo-inertness, and the document-end append
//! discipline that killed the line-end send race.

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, TextAnchor, WriteGate};
  use flowstate_document::loro_schema::SpeechTargetMarker;

  struct Peer {
    handle: LocalDocHandle,
    gate: Arc<WriteGate<CrdtRuntime>>,
  }

  impl Peer {
    fn new(title: &str) -> Self {
      let core = CrdtRuntime::new_empty(title).expect("runtime");
      let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
      Self { handle, gate }
    }

    fn export_all(&self) -> Vec<u8> {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .expect("export")
    }

    fn import(&self, bytes: &[u8]) {
      let mut guard = self.gate.lock(GateHolder::ImportChunk).expect("gate");
      guard.import_remote_update(bytes).expect("import");
    }

    fn set_marker(&self, marker: &SpeechTargetMarker) {
      let mut guard = self.gate.lock(GateHolder::DocumentService).expect("gate");
      guard.set_speech_target(marker).expect("set speech target");
    }

    fn marker(&self) -> Option<SpeechTargetMarker> {
      let guard = self.gate.lock(GateHolder::DocumentService).expect("gate");
      guard.speech_target()
    }

    fn body(&self) -> String {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      flowstate_document::loro_schema::body_text(guard.doc()).to_string()
    }

    fn append_text(&self, text: &str) {
      let projection = self.handle.projection().expect("projection");
      let paragraph = *projection.ids.paragraph_ids.last().expect("paragraph");
      self
        .handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: text.into(),
          style_override: None,
        })
        .expect("append");
    }
  }

  fn sync(a: &Peer, b: &Peer) {
    let from_a = a.export_all();
    let from_b = b.export_all();
    b.import(&from_a);
    a.import(&from_b);
  }

  fn marker(active: bool, by: &str, at: i64) -> SpeechTargetMarker {
    SpeechTargetMarker {
      active,
      designated_by: by.to_string(),
      designated_at_ms: at,
    }
  }

  #[test]
  fn speech_marker_replicates_and_round_trips() {
    let a = Peer::new("speech-marker");
    let b = Peer::new("speech-marker");
    sync(&a, &b);

    a.set_marker(&marker(true, "Alex", 111));
    sync(&a, &b);
    assert_eq!(b.marker(), Some(marker(true, "Alex", 111)), "the designation replicates");

    // The peer toggles it off; the clear replicates back.
    b.set_marker(&marker(false, "Blair", 222));
    sync(&a, &b);
    assert_eq!(a.marker(), Some(marker(false, "Blair", 222)), "the clear replicates");
  }

  #[test]
  fn concurrent_designation_converges_atomically() {
    let a = Peer::new("speech-race");
    let b = Peer::new("speech-race");
    sync(&a, &b);

    // Concurrent designations — neither has seen the other's.
    a.set_marker(&marker(true, "Alex", 200));
    b.set_marker(&marker(true, "Blair", 100));
    sync(&a, &b);
    sync(&a, &b);

    let converged_a = a.marker().expect("marker survives");
    let converged_b = b.marker().expect("marker survives");
    assert_eq!(converged_a, converged_b, "both replicas agree on one designation");
    // Atomicity: the whole register wins together — never peer A's flag with
    // peer B's timestamp.
    assert!(
      converged_a == marker(true, "Alex", 200) || converged_a == marker(true, "Blair", 100),
      "the winner is one designator's intact marker, got {converged_a:?}"
    );
  }

  #[test]
  fn speech_marker_is_undo_inert() {
    let peer = Peer::new("speech-undo");
    peer.append_text("card body");
    peer.set_marker(&marker(true, "Alex", 42));

    // Ctrl+Z after designating must undo the TEXT, not the marker.
    let outcome = peer.handle.apply_undo().expect("undo runs");
    assert!(outcome.applied, "undo applies to the text edit");
    assert!(!peer.body().contains("card body"), "the text edit is undone");
    assert_eq!(
      peer.marker(),
      Some(marker(true, "Alex", 42)),
      "the designation is untouched by undo (meta origin is inert)"
    );
  }

  #[test]
  fn concurrent_document_end_appends_both_survive() {
    let a = Peer::new("speech-sends");
    let b = Peer::new("speech-sends");
    a.append_text("opening. ");
    sync(&a, &b);

    // Two peers' backtick sends land concurrently — both append at document
    // end (CT-S2 contract) and neither may vanish.
    a.append_text("[A card]");
    b.append_text("[B card]");
    sync(&a, &b);
    sync(&a, &b);

    assert_eq!(a.body(), b.body(), "replicas converge");
    assert!(a.body().contains("[A card]"), "peer A's send survives");
    assert!(a.body().contains("[B card]"), "peer B's send survives");
  }
}
