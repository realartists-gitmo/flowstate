//! Network-pathology convergence fuzz (Loro-first spec §13/§22).
//!
//! The intent/object fuzzes exchange updates losslessly and in order; real
//! transports do not. This harness delivers update chunks REORDERED,
//! DUPLICATED, and DELAYED across rounds while peers keep editing (inserts,
//! splits, undo/redo). Imports with missing causal dependencies must park as
//! PENDING inside Loro and recover when the gap arrives — never wedge, never
//! project half a batch. After the chaos, a clean anti-entropy pass must
//! converge every peer on identical canonical state AND a projection equal to
//! a fresh rematerialization of its own doc (`swarm_loopback` covers the real
//! iroh transport out-of-gate; this covers the semantics deterministically).

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{
    GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, SplitParagraphIntent, TextAnchor, WriteGate, WriteRejected,
  };
  use flowstate_document::{DocumentProjection, ParagraphStyle, document_from_loro, paragraph_text};

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
    /// Version vector snapshot at the last chunk export.
    exported_vv: loro::VersionVector,
  }

  impl Peer {
    fn new(title: &str) -> Self {
      let core = CrdtRuntime::new_empty(title).expect("runtime");
      let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
      let exported_vv = {
        let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
        guard.doc().state_vv()
      };
      // The pre-export baseline must NOT include the startup commits, or the
      // first chunk would omit each peer's seed/replica metadata and every
      // import of it would pend forever.
      let _ = exported_vv;
      Self {
        handle,
        gate,
        exported_vv: loro::VersionVector::default(),
      }
    }

    fn projection(&self) -> DocumentProjection {
      self.handle.projection().expect("projection")
    }

    fn fresh_canonical(&self) -> DocumentProjection {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      document_from_loro(guard.doc()).expect("fresh rematerialization")
    }

    fn body_text(&self) -> String {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      flowstate_document::loro_schema::body_text(guard.doc()).to_string()
    }

    fn state_vv(&self) -> loro::VersionVector {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard.doc().state_vv()
    }

    /// Export everything committed since the previous chunk export — one
    /// "packet" as a gossip transport would carry it.
    fn export_chunk(&mut self) -> Option<Vec<u8>> {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      let now = guard.doc().state_vv();
      if now == self.exported_vv {
        return None;
      }
      let bytes = guard
        .doc()
        .export(loro::ExportMode::updates(&self.exported_vv))
        .expect("export");
      drop(guard);
      self.exported_vv = now;
      (!bytes.is_empty()).then_some(bytes)
    }

    fn import(&self, bytes: &[u8]) {
      let mut guard = self.gate.lock(GateHolder::ImportChunk).expect("gate");
      guard
        .import_remote_update(bytes)
        .expect("import never errors, pending is a status");
    }
  }

  fn random_edit(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.projection();
    if projection.paragraphs.is_empty() {
      return Ok(());
    }
    let paragraph_ix = rng.below(projection.paragraphs.len());
    let paragraph = projection.ids.paragraph_ids[paragraph_ix];
    match rng.below(6) {
      0..=2 => peer
        .handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("n{step}"),
          style_override: None,
        })
        .map(|_| ()),
      3 => peer
        .handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          inherited_style: ParagraphStyle::Normal,
        })
        .map(|_| ()),
      4 => peer.handle.apply_undo().map(|_| ()),
      _ => peer.handle.apply_redo().map(|_| ()),
    }
  }

  /// One in-flight packet: producer peer + payload + how many rounds it still
  /// sits in the network before delivery is even attempted.
  struct Packet {
    from: usize,
    bytes: Vec<u8>,
    delay_rounds: usize,
  }

  fn run_chaos(seed: u64, peers_n: usize, rounds: usize, ops_per_round: usize) {
    let mut rng = Rng::new(seed);
    let mut peers: Vec<Peer> = (0..peers_n).map(|_| Peer::new("chaos")).collect();
    let mut network: Vec<Packet> = Vec::new();

    for round in 0..rounds {
      // ---- Edits --------------------------------------------------------
      for op in 0..ops_per_round {
        let peer_ix = rng.below(peers_n);
        match random_edit(&mut rng, &peers[peer_ix], round * ops_per_round + op) {
          Ok(()) | Err(WriteRejected::EmptyIntent | WriteRejected::StructureViolation(_) | WriteRejected::UnresolvedParagraph(_)) => {},
          Err(other) => panic!("seed {seed} round {round} op {op}: unexpected rejection {other}"),
        }
      }

      // ---- Publish: each peer's new history becomes one packet ----------
      for (from, peer) in peers.iter_mut().enumerate() {
        if let Some(bytes) = peer.export_chunk() {
          network.push(Packet {
            from,
            bytes,
            delay_rounds: rng.below(3),
          });
        }
      }

      // ---- Chaotic delivery: shuffle, sometimes duplicate, delay --------
      // Deterministic Fisher-Yates on the in-flight set.
      for ix in (1..network.len()).rev() {
        network.swap(ix, rng.below(ix + 1));
      }
      let mut undelivered = Vec::new();
      for mut packet in std::mem::take(&mut network) {
        if packet.delay_rounds > 0 {
          packet.delay_rounds -= 1;
          undelivered.push(packet);
          continue;
        }
        for (to, peer) in peers.iter().enumerate() {
          if to != packet.from {
            peer.import(&packet.bytes);
            if rng.below(5) == 0 {
              // Duplicate delivery: idempotent import is part of the contract.
              peer.import(&packet.bytes);
            }
          }
        }
      }
      network = undelivered;
    }

    // ---- Drain the network, then clean anti-entropy to quiescence --------
    for packet in network {
      for (to, peer) in peers.iter().enumerate() {
        if to != packet.from {
          peer.import(&packet.bytes);
        }
      }
    }
    for _pass in 0..8 {
      let before: Vec<_> = peers.iter().map(Peer::state_vv).collect();
      for from in 0..peers_n {
        let bytes = {
          let guard = peers[from]
            .gate
            .lock(GateHolder::ExportUpdates)
            .expect("gate");
          guard
            .doc()
            .export(loro::ExportMode::all_updates())
            .expect("export")
        };
        for (to, peer) in peers.iter().enumerate() {
          if to != from {
            peer.import(&bytes);
          }
        }
      }
      if peers.iter().map(Peer::state_vv).collect::<Vec<_>>() == before {
        break;
      }
    }

    // ---- Convergence + self-consistency bar ------------------------------
    let reference_text = peers[0].body_text();
    for (ix, peer) in peers.iter().enumerate() {
      let projection = peer.projection();
      let canonical = peer.fresh_canonical();
      assert_eq!(
        projection.paragraphs.len(),
        canonical.paragraphs.len(),
        "seed {seed}: peer {ix} projection/canonical paragraph count diverged"
      );
      for paragraph_ix in 0..projection.paragraphs.len() {
        assert_eq!(
          paragraph_text(&projection, paragraph_ix),
          paragraph_text(&canonical, paragraph_ix),
          "seed {seed}: peer {ix} paragraph {paragraph_ix} deviates from own canonical"
        );
      }
      assert_eq!(peer.state_vv(), peers[0].state_vv(), "seed {seed}: peer {ix} version vector diverged");
      assert_eq!(peer.body_text(), reference_text, "seed {seed}: peer {ix} body text diverged");
    }
  }

  #[test]
  fn reordered_duplicated_delayed_delivery_converges() {
    for seed in [1, 7, 42, 99, 20260707] {
      run_chaos(seed, 3, 6, 8);
    }
  }

  #[test]
  fn two_peer_chaos_with_heavy_delay_converges() {
    for seed in [3, 1337] {
      run_chaos(seed, 2, 10, 6);
    }
  }
}
