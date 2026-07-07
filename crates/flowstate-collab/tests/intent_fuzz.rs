//! N-peer randomized intent convergence fuzz (Loro-first spec §13.7).
//!
//! Every peer edits exclusively through its `LocalDocHandle` (the one write
//! path), with randomized intents — inserts, deletes, splits, joins, marks,
//! paragraph styles, expand-mark boundary typing, split-at-styled-run-end,
//! rich-fragment paste — and randomized update exchange. After every sync round, all peers must
//! agree on BOTH canonical Loro state and the materialized projection.
//! Includes the §13.13 zero-patch-frontier-churn regression (the 19:28 field
//! wound): a frontier-only remote advance must never disturb local typing.

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{
    DeleteRangeIntent, FragmentBlock, GateHolder, InsertRichFragmentIntent, InsertTextIntent, JoinParagraphsIntent, LocalDocHandle,
    LocalWriteConfig, SetMarksIntent, SetParagraphStyleIntent, SplitParagraphIntent, TextAnchor, WriteGate, WriteRejected,
  };
  use flowstate_document::{Block, DocumentProjection, InputParagraph, InputRun, ParagraphStyle, RunSemanticStyle, RunStyles, paragraph_text};

  /// Deterministic xorshift PRNG — reproducible fuzz, no wall-clock dependence.
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
    /// Version vector snapshot per remote peer we last pulled from — plain
    /// full-history exchange keeps the harness simple and deterministic.
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
      guard.doc().export(loro::ExportMode::updates(vv)).expect("export")
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
  }

  fn random_styles(rng: &mut Rng) -> RunStyles {
    let mut styles = RunStyles::default();
    if rng.below(2) == 0 {
      styles.semantic = RunSemanticStyle::Custom((rng.below(4) + 1) as u8);
    }
    if rng.below(3) == 0 {
      styles.direct_underline = true;
    }
    if rng.below(4) == 0 {
      styles.strikethrough = true;
    }
    styles
  }

  /// One random intent against `peer`. Rejections for stale identities are legal
  /// outcomes (I-15) — the fuzz only demands convergence, not acceptance.
  fn random_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.projection();
    if projection.paragraphs.is_empty() {
      return Ok(());
    }
    let paragraph_ix = rng.below(projection.paragraphs.len());
    let paragraph = projection.ids.paragraph_ids[paragraph_ix];
    let text_len = flowstate_document::paragraph_text_len(&projection.paragraphs[paragraph_ix]);
    let byte = rng.below(text_len + 1);
    match rng.below(11) {
      // Weighted toward typing: inserts dominate real sessions.
      0..=3 => {
        let text = format!("s{step}p{paragraph_ix}");
        peer
          .handle
          .insert_text(InsertTextIntent {
            at: TextAnchor::new(paragraph, byte),
            text,
            style_override: (rng.below(4) == 0).then(|| random_styles(rng)),
          })
          .map(|_| ())
      },
      4 => {
        // Boundary typing at the very end of the paragraph — the expand-`After`
        // inheritance case (spec §9).
        peer
          .handle
          .insert_text(InsertTextIntent {
            at: TextAnchor::new(paragraph, usize::MAX),
            text: format!("e{step}"),
            style_override: None,
          })
          .map(|_| ())
      },
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
      6 => {
        // Split — including split-at-styled-run-end (mark the tail first on a
        // coin flip so the sentinel-hygiene rule gets fuzzed, spec §13.7).
        if rng.below(2) == 0 && text_len > 0 {
          let mark_start = text_len.saturating_sub(1 + rng.below(2)).min(text_len - 1);
          let _ = peer.handle.set_marks(SetMarksIntent {
            start: TextAnchor::new(paragraph, mark_start),
            end: TextAnchor::new(paragraph, text_len),
            styles: random_styles(rng),
          });
        }
        peer
          .handle
          .split_paragraph(SplitParagraphIntent {
            at: TextAnchor::new(paragraph, byte),
            inherited_style: if rng.below(2) == 0 { ParagraphStyle::Normal } else { ParagraphStyle::Custom(1) },
          })
          .map(|_| ())
      },
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
      9 => {
        // Rich-fragment paste (the compound intent): two short paragraphs in one
        // gate hold / one commit.
        let fragment_paragraph = |text: String| {
          FragmentBlock::Paragraph(InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![InputRun {
              text,
              styles: RunStyles::default(),
            }],
          })
        };
        peer
          .handle
          .insert_rich_fragment(InsertRichFragmentIntent {
            at: TextAnchor::new(paragraph, byte),
            blocks: vec![fragment_paragraph(format!("f{step}a")), fragment_paragraph(format!("f{step}b"))],
          })
          .map(|_| ())
      },
      _ => peer
        .handle
        .set_paragraph_style(SetParagraphStyleIntent {
          paragraph,
          style: if rng.below(2) == 0 { ParagraphStyle::Normal } else { ParagraphStyle::Custom((rng.below(3) + 1) as u8) },
        })
        .map(|_| ()),
    }
  }

  /// Full-mesh sync: every peer pulls every other peer's new history.
  fn sync_all(peers: &mut [Peer]) {
    for _ in 0..2 {
      // Two passes so transitively-learned history also converges.
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

  fn projections_agree(left: &DocumentProjection, right: &DocumentProjection) -> Result<(), String> {
    if left.paragraphs.len() != right.paragraphs.len() {
      return Err(format!("paragraph count {} != {}", left.paragraphs.len(), right.paragraphs.len()));
    }
    for ix in 0..left.paragraphs.len() {
      if paragraph_text(left, ix) != paragraph_text(right, ix) {
        return Err(format!("paragraph[{ix}] text {:?} != {:?}", paragraph_text(left, ix), paragraph_text(right, ix)));
      }
      if left.paragraphs[ix].style != right.paragraphs[ix].style {
        return Err(format!("paragraph[{ix}] style differs"));
      }
      if left.paragraphs[ix].runs != right.paragraphs[ix].runs {
        return Err(format!("paragraph[{ix}] runs differ: {:?} != {:?}", left.paragraphs[ix].runs, right.paragraphs[ix].runs));
      }
    }
    if left.ids.paragraph_ids != right.ids.paragraph_ids {
      return Err("paragraph ids differ".to_string());
    }
    if left.ids.block_ids != right.ids.block_ids {
      return Err("block ids differ".to_string());
    }
    // Structural block agreement (kinds; the object/table suites in
    // object_table_convergence.rs additionally compare table topology).
    let kind = |block: &Block| match block {
      Block::Paragraph(_) => "paragraph",
      Block::Image(_) => "image",
      Block::Equation(_) => "equation",
      Block::Table(_) => "table",
    };
    if left.blocks.len() != right.blocks.len() {
      return Err(format!("block count {} != {}", left.blocks.len(), right.blocks.len()));
    }
    for ix in 0..left.blocks.len() {
      if kind(&left.blocks[ix]) != kind(&right.blocks[ix]) {
        return Err(format!("block[{ix}] kind {} != {}", kind(&left.blocks[ix]), kind(&right.blocks[ix])));
      }
    }
    Ok(())
  }

  fn run_fuzz(seed: u64, peers_n: usize, rounds: usize, ops_per_round: usize) {
    let mut rng = Rng::new(seed);
    let mut peers: Vec<Peer> = (0..peers_n).map(|_| Peer::new("fuzz", peers_n)).collect();
    // Initial full exchange so the mergeable seeds converge before editing.
    sync_all(&mut peers);

    for round in 0..rounds {
      for op in 0..ops_per_round {
        let peer_ix = rng.below(peers_n);
        let step = round * ops_per_round + op;
        match random_intent(&mut rng, &peers[peer_ix], step) {
          Ok(()) => {},
          Err(WriteRejected::EmptyIntent | WriteRejected::StructureViolation(_) | WriteRejected::UnresolvedParagraph(_)) => {},
          Err(other) => panic!("seed {seed} round {round} op {op}: unexpected rejection {other}"),
        }
      }
      sync_all(&mut peers);

      // Convergence bar (spec §13.7): canonical Loro text AND projection agree.
      let reference_text = peers[0].body_text();
      let reference_projection = peers[0].projection();
      for (ix, peer) in peers.iter().enumerate().skip(1) {
        assert_eq!(peer.body_text(), reference_text, "seed {seed} round {round}: peer {ix} body text diverged");
        if let Err(reason) = projections_agree(&peer.projection(), &reference_projection) {
          panic!("seed {seed} round {round}: peer {ix} projection diverged: {reason}");
        }
      }
    }
  }

  #[test]
  fn two_peer_intent_fuzz_converges() {
    for seed in [1, 7, 42, 1337] {
      run_fuzz(seed, 2, 6, 12);
    }
  }

  #[test]
  fn three_peer_intent_fuzz_converges() {
    for seed in [3, 99, 20260707] {
      run_fuzz(seed, 3, 5, 10);
    }
  }

  /// Spec §13.13 — zero-patch frontier churn is architecturally inert. A remote
  /// update that advances the Loro frontier without changing the projection must
  /// not disturb local typing in any way: no rejection, no recovery, no replay.
  #[test]
  fn zero_patch_frontier_churn_is_inert() {
    let mut peers: Vec<Peer> = (0..2).map(|_| Peer::new("churn", 2)).collect();
    sync_all(&mut peers);
    let paragraph = peers[0].projection().ids.paragraph_ids[0];

    for i in 0..10 {
      // Peer 1 produces a frontier-advancing, projection-neutral update
      // (metadata touch: register_replica writes bookkeeping maps only).
      let guard = peers[1].gate.lock(GateHolder::ExportUpdates).expect("gate");
      flowstate_document::register_replica(guard.doc(), Some(0x77)).expect("metadata commit");
      drop(guard);
      let bytes = peers[1].export_updates_since(&peers[0].synced_vv[1].clone());
      peers[0].synced_vv[1] = peers[1].state_vv();
      peers[0].import(&bytes);

      // Immediately type locally: must commit cleanly against current state.
      let outcome = peers[0]
        .handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("{i}"),
          style_override: None,
        })
        .expect("local insert must be untouched by frontier-only churn");
      let commit = outcome.commit();
      assert!(!commit.counters.full_rebuild, "frontier churn must not force local rebuilds");
    }
    // Both replicas seeded their own sentinel before the exchange, so the
    // absolute text shape carries two boundary newlines; the property under
    // test is that every digit landed, in order, at the anchored paragraph.
    assert!(
      peers[0].body_text().contains("0123456789"),
      "digits must land contiguously in order: {:?}",
      peers[0].body_text()
    );
  }
}
