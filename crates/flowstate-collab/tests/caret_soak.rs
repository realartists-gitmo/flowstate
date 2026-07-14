//! Presence-caret soak against the Loro-first intent API (spec §13.8 flavor).
//!
//! The old optimistic-reconcile caret soak raced pending editor batches behind
//! an in-flight raw-command runtime transaction; that surface no longer
//! exists — with `LocalDocHandle` the "optimistic" apply IS the committed
//! apply, so there is nothing to reconcile. What survives is the caret
//! DURABILITY question: a presence selection captured as encoded Loro cursors
//! must keep resolving to an in-bounds document position on every replica while
//! concurrent intents and imports churn the document underneath it.
//!
//! Shape: N peers edit concurrently through their `LocalDocHandle`s; each
//! round every peer captures a presence selection at a random caret position
//! (gate-held, `CrdtRuntime::presence_selection`), the mesh syncs, and then
//! EVERY peer resolves EVERY captured selection through
//! `CrdtRuntime::resolve_presence_carets`. Asserted per round: no panics, every
//! resolved caret lands inside the resolving peer's document bounds, and the
//! replicas converge (body text + projection).

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::{CrdtRuntime, RuntimePresenceCaretRequest};
  use flowstate_collab::local_write::{
    DeleteRangeIntent, GateHolder, InsertTextIntent, JoinParagraphsIntent, LocalDocHandle, LocalWriteConfig, SetMarksIntent,
    SetParagraphStyleIntent, SplitParagraphIntent, TextAnchor, WriteGate, WriteRejected,
  };
  use flowstate_collab::presence::PresenceSelection;
  use flowstate_document::{
    DocumentOffset, DocumentProjection, EditorSelection, ParagraphStyle, RunSemanticStyle, RunStyles, paragraph_text, paragraph_text_len,
  };

  /// Deterministic xorshift PRNG — reproducible soak, no wall-clock dependence.
  struct Rng(u64);

  impl Rng {
    fn new(seed: u64) -> Self {
      Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
      let mut x = self.0;
      x ^= x << 13;
      x ^= x >> 7;
      x ^= x << 17;
      self.0 = x;
      x
    }

    fn below(&mut self, bound: usize) -> usize {
      if bound == 0 { 0 } else { (self.next() % bound as u64) as usize }
    }
  }

  struct Peer {
    handle: LocalDocHandle,
    gate: Arc<WriteGate<CrdtRuntime>>,
    synced_vv: Vec<loro::VersionVector>,
  }

  impl Peer {
    fn new(title: &str, peer_count: usize) -> Self {
      let core = CrdtRuntime::new_empty(title).expect("runtime");
      let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
      Self {
        handle,
        gate,
        synced_vv: vec![loro::VersionVector::default(); peer_count],
      }
    }

    fn projection(&self) -> DocumentProjection {
      self.handle.projection().expect("projection")
    }

    fn export_updates_since(&self, vv: &loro::VersionVector) -> Vec<u8> {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard
        .doc()
        .export(loro::ExportMode::updates(vv))
        .expect("export")
    }

    fn state_vv(&self) -> loro::VersionVector {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard.doc().state_vv()
    }

    fn import(&self, bytes: &[u8]) {
      if bytes.is_empty() {
        return;
      }
      let mut guard = self.gate.lock(GateHolder::ImportChunk).expect("gate");
      guard.import_remote_update(bytes).expect("import");
    }

    fn body_text(&self) -> String {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      flowstate_document::loro_schema::body_text(guard.doc()).to_string()
    }

    /// Capture a presence selection for a collapsed caret at `offset`, holding
    /// the gate as the Presence holder (cursor encode is a doc read).
    fn capture_presence(&self, offset: DocumentOffset) -> Option<PresenceSelection> {
      let guard = self.gate.lock(GateHolder::Presence).expect("gate");
      guard.presence_selection(&EditorSelection::collapsed(offset))
    }

    /// Resolve remote presence selections against this peer's canonical state.
    fn resolve_presence(&self, requests: Vec<RuntimePresenceCaretRequest>) -> Vec<flowstate_document::ExternalCaret> {
      let guard = self.gate.lock(GateHolder::Presence).expect("gate");
      guard.resolve_presence_carets(requests).carets
    }
  }

  /// Full-mesh sync: every peer pulls every other peer's new history.
  fn sync_all(peers: &mut [Peer]) {
    for _ in 0..2 {
      for from in 0..peers.len() {
        let from_vv_now = peers[from].state_vv();
        for to in 0..peers.len() {
          if from == to {
            continue;
          }
          let since = peers[to].synced_vv[from].clone();
          let bytes = peers[from].export_updates_since(&since);
          peers[to].import(&bytes);
          peers[to].synced_vv[from] = from_vv_now.clone();
        }
      }
    }
  }

  fn random_styles(rng: &mut Rng) -> RunStyles {
    let mut styles = RunStyles::default();
    if rng.below(2) == 0 {
      styles.semantic = RunSemanticStyle::Custom((rng.below(4) + 1) as u8);
    }
    if rng.below(3) == 0 {
      styles.direct_underline = true;
    }
    styles
  }

  /// One random text intent against `peer` (ASCII-only content so every raw byte
  /// hint is char-boundary-clean). Rejections from stale identities are legal
  /// outcomes (I-15); anything that wedges the doc is not.
  fn random_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.projection();
    if projection.paragraphs.is_empty() {
      return Ok(());
    }
    let paragraph_ix = rng.below(projection.paragraphs.len());
    let paragraph = projection.ids.paragraph_ids[paragraph_ix];
    let text_len = paragraph_text_len(&projection.paragraphs[paragraph_ix]);
    let byte = rng.below(text_len + 1);
    match rng.below(10) {
      0..=4 => peer
        .handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, byte),
          text: format!("s{step}"),
          style_override: None,
        })
        .map(|_| ()),
      5 => {
        if text_len == 0 {
          return Ok(());
        }
        let start = rng.below(text_len);
        let end = (start + 1 + rng.below(3)).min(text_len);
        peer
          .handle
          .delete_range(DeleteRangeIntent {
            start: TextAnchor::new(paragraph, start),
            end: TextAnchor::new(paragraph, end),
          })
          .map(|_| ())
      },
      6 if projection.paragraphs.len() < 24 => peer
        .handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(paragraph, byte),
          inherited_style: ParagraphStyle::Normal,
        })
        .map(|_| ()),
      7 => {
        if paragraph_ix + 1 >= projection.paragraphs.len() {
          return Ok(());
        }
        let second = projection.ids.paragraph_ids[paragraph_ix + 1];
        peer
          .handle
          .join_paragraphs(JoinParagraphsIntent { first: paragraph, second })
          .map(|_| ())
      },
      8 => {
        if text_len == 0 {
          return Ok(());
        }
        let start = rng.below(text_len);
        let end = (start + 1 + rng.below(4)).min(text_len);
        peer
          .handle
          .set_marks(SetMarksIntent {
            start: TextAnchor::new(paragraph, start),
            end: TextAnchor::new(paragraph, end),
            styles: random_styles(rng),
          })
          .map(|_| ())
      },
      _ => peer
        .handle
        .set_paragraph_style(SetParagraphStyleIntent {
          paragraph,
          style: if rng.below(2) == 0 {
            ParagraphStyle::Normal
          } else {
            ParagraphStyle::Custom(1)
          },
        })
        .map(|_| ()),
    }
  }

  fn tolerate(result: Result<(), WriteRejected>, seed: u64, round: usize, op: usize) {
    match result {
      Ok(()) => {},
      Err(fatal @ (WriteRejected::GatePoisoned | WriteRejected::CompensationFailed { .. })) => {
        panic!("seed {seed} round {round} op {op}: doc wedged: {fatal}")
      },
      // Stale identity / empty / structural / compensated rejections are legal
      // fuzz outcomes; only convergence is asserted.
      Err(_) => {},
    }
  }

  fn assert_replicas_converged(peers: &[Peer], seed: u64, round: usize) {
    let reference_text = peers[0].body_text();
    let reference = peers[0].projection();
    for (ix, peer) in peers.iter().enumerate().skip(1) {
      assert_eq!(
        peer.body_text(),
        reference_text,
        "seed {seed} round {round}: peer {ix} body text diverged"
      );
      let projection = peer.projection();
      assert_eq!(
        projection.ids.paragraph_ids, reference.ids.paragraph_ids,
        "seed {seed} round {round}: peer {ix} paragraph ids diverged"
      );
      for paragraph_ix in 0..reference.paragraphs.len() {
        assert_eq!(
          paragraph_text(&projection, paragraph_ix),
          paragraph_text(&reference, paragraph_ix),
          "seed {seed} round {round}: peer {ix} paragraph {paragraph_ix} text diverged"
        );
      }
    }
  }

  fn run_presence_soak(seed: u64, peers_n: usize, rounds: usize, ops_per_round: usize) {
    let mut rng = Rng::new(seed);
    let mut peers: Vec<Peer> = (0..peers_n)
      .map(|_| Peer::new("caret soak", peers_n))
      .collect();
    sync_all(&mut peers);

    for round in 0..rounds {
      // Concurrent intents (pre-sync, so cursor capture races the round's edits).
      for op in 0..ops_per_round {
        let peer_ix = rng.below(peers_n);
        let step = round * ops_per_round + op;
        tolerate(random_intent(&mut rng, &peers[peer_ix], step), seed, round, op);
      }

      // Every peer captures a presence caret at a random position in its own
      // (diverged) replica. `None` is legal only for degenerate offsets; a caret
      // in a live paragraph must encode.
      let mut selections: Vec<PresenceSelection> = Vec::new();
      for peer in &peers {
        let projection = peer.projection();
        if projection.paragraphs.is_empty() {
          continue;
        }
        let paragraph_ix = rng.below(projection.paragraphs.len());
        let byte = rng.below(paragraph_text_len(&projection.paragraphs[paragraph_ix]) + 1);
        let offset = DocumentOffset {
          paragraph: paragraph_ix,
          byte,
        };
        if let Some(selection) = peer.capture_presence(offset) {
          selections.push(selection);
        }
      }

      sync_all(&mut peers);

      // Post-sync, every peer resolves every captured selection. Unresolvable
      // cursors (target deleted concurrently) drop out — legal. Every caret that
      // DOES resolve must land inside the resolving peer's document bounds.
      for (peer_ix, peer) in peers.iter().enumerate() {
        let projection = peer.projection();
        let requests: Vec<RuntimePresenceCaretRequest> = selections
          .iter()
          .map(|selection| RuntimePresenceCaretRequest {
            selection: selection.clone(),
            color_rgb: 0x00FF_00FF,
          })
          .collect();
        let carets = peer.resolve_presence(requests);
        for caret in &carets {
          assert!(
            caret.offset.paragraph < projection.paragraphs.len(),
            "seed {seed} round {round}: peer {peer_ix} resolved caret paragraph {} out of bounds ({} paragraphs)",
            caret.offset.paragraph,
            projection.paragraphs.len(),
          );
          let len = paragraph_text_len(&projection.paragraphs[caret.offset.paragraph]);
          assert!(
            caret.offset.byte <= len,
            "seed {seed} round {round}: peer {peer_ix} resolved caret byte {} beyond paragraph end {len}",
            caret.offset.byte,
          );
        }
      }

      assert_replicas_converged(&peers, seed, round);
    }
  }

  #[test]
  fn presence_caret_soak_two_peers() {
    for seed in [0xC0FF_EE12, 7] {
      run_presence_soak(seed, 2, 8, 10);
    }
  }

  #[test]
  fn presence_caret_soak_three_peers() {
    for seed in [42, 20260707] {
      run_presence_soak(seed, 3, 6, 8);
    }
  }

  /// Directed caret-durability regression (the spirit of the old enter/char
  /// spam): a caret parked at the end of a paragraph on peer 0 must keep
  /// resolving to that paragraph's end while peer 1 concurrently prepends text
  /// and splits paragraphs upstream of it — resolution goes through the encoded
  /// cursor, never through raw offsets.
  #[test]
  fn caret_at_paragraph_end_survives_upstream_churn() {
    let mut peers: Vec<Peer> = (0..2).map(|_| Peer::new("caret churn", 2)).collect();
    sync_all(&mut peers);

    let paragraph = peers[0].projection().ids.paragraph_ids[0];
    peers[0]
      .handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "anchor".into(),
        style_override: None,
      })
      .expect("seed insert");
    sync_all(&mut peers);

    // Park the caret at the end of "anchor" on peer 0.
    let selection = peers[0]
      .capture_presence(DocumentOffset { paragraph: 0, byte: 6 })
      .expect("caret in live text must encode");

    for i in 0..10 {
      // Peer 1 churns upstream: prepend + split at the front of the paragraph.
      peers[1]
        .handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, 0),
          text: format!("x{i}"),
          style_override: None,
        })
        .expect("upstream insert");
      if i % 3 == 0 {
        let _ = peers[1].handle.split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(paragraph, 2),
          inherited_style: ParagraphStyle::Normal,
        });
      }
      sync_all(&mut peers);

      for (peer_ix, peer) in peers.iter().enumerate() {
        let projection = peer.projection();
        let carets = peer.resolve_presence(vec![RuntimePresenceCaretRequest {
          selection: selection.clone(),
          color_rgb: 0x00FF_FFFF,
        }]);
        assert_eq!(carets.len(), 1, "iteration {i}: peer {peer_ix} must still resolve the parked caret");
        let caret = &carets[0];
        let text = paragraph_text(&projection, caret.offset.paragraph);
        assert!(
          text.as_bytes()[..caret.offset.byte].ends_with(b"anchor"),
          "iteration {i}: peer {peer_ix} caret must stay glued to the end of 'anchor', got byte {} in {text:?}",
          caret.offset.byte,
        );
      }
    }
    assert_replicas_converged(&peers, 0, 0);
  }
}
