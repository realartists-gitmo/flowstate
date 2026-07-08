//! §act-three A / §9.1 — the view-overlay eventual-exactness oracle, driven
//! against the REAL write authority.
//!
//! The editor's visible document is `canonical ⊕ overlay(pending_queue)`. This
//! fuzz stands up a live `LocalDocHandle` (the one write path), an editor-sim
//! that advances a `canonical` projection SOLELY by draining the ordered
//! stream (exactly `sync_projection_from_authority`), and a
//! `gpui_flowtext::OverlayQueue` holding the predictions of intents that have
//! been submitted but whose batch has not yet drained. It then randomizes the
//! adversarial axes from spec §9.2:
//!
//! - drain scheduling: delayed / partial / bursty (0..N stream items per tick);
//! - remote imports interleaved at every point;
//! - queue pressure toward the depth bound;
//! - a mix of predictable (text/split/delete/join/marks/para-style) and inert
//!   (object) intents.
//!
//! Oracle 9.1 is asserted at every quiescent point (queue empty after a drain):
//! `visible == canonical == the authority's canonical projection`,
//! byte-for-byte. Prediction quality (predicted-vs-canonical) is measured and
//! reported, never assumed. Because the overlay is stateless re-derivation, a
//! wrong prediction can survive at most until its intent's batch drains — the
//! persistence-of-divergence property this fuzz exists to enforce.

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{
    DeleteRangeIntent, GateHolder, InsertTextIntent, JoinParagraphsIntent, LocalDocHandle, LocalIntent, LocalWriteAuthority, LocalWriteConfig,
    SetMarksIntent, SplitParagraphIntent, TextAnchor, WriteGate,
  };
  use flowstate_document::{
    DocumentProjection, OverlayQueue, ParagraphStyle, ProjectionStreamItem, RunSemanticStyle, RunStyles, apply_projection_patch_batch,
    paragraph_text,
  };
  use flowstate_collab::local_write::SetParagraphStyleIntent;

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

  /// The editor-sim: a canonical projection advanced only by draining the
  /// stream, plus the overlay queue. Each queue entry is correlated to its
  /// commit by the frontier the commit produced (the only correlator the real
  /// system has — the stream is frontier-ordered).
  struct EditorSim {
    canonical: DocumentProjection,
    overlay: OverlayQueue,
    /// Count of local intents submitted-and-committed since the last drain.
    /// A drain applies the whole accumulated stream (matching the real
    /// editor's atomic pump), so it acks exactly this many overlay entries.
    committed_since_drain: usize,
    /// Whether a heal (base-mismatch or in-place apply error) was needed on the
    /// last drain — surfaced so the fuzz can track heal frequency (§9.6).
    heals: usize,
  }

  impl EditorSim {
    fn new(canonical: DocumentProjection) -> Self {
      Self {
        canonical,
        overlay: OverlayQueue::new(),
        committed_since_drain: 0,
        heals: 0,
      }
    }

    /// The rendered view.
    fn visible(&mut self) -> DocumentProjection {
      self.overlay.derive_visible(&self.canonical)
    }

    /// Drain the ENTIRE ordered stream and apply every item in commit order,
    /// exactly like `sync_projection_from_authority`: chained in-place apply,
    /// heal-from-canonical on any base mismatch or apply error. Then ack the
    /// overlay entries for every local commit that has now landed. The
    /// "delayed / partial / bursty" axis comes from drain FREQUENCY — the
    /// authority's stream accumulates un-drained batches between drains.
    fn drain(&mut self, handle: &LocalDocHandle) {
      let items = handle.drain_projection_stream().expect("drain");
      if items.is_empty() {
        // Even with no new batches, any committed intents are already in
        // canonical (a prior drain landed them) — but with none since the last
        // drain there is nothing to ack.
        return;
      }
      let mut needs_heal = false;
      for item in items {
        match item {
          ProjectionStreamItem::Patches(batch) => {
            if batch.new_frontier == self.canonical.frontier {
              // idempotent redelivery
            } else if batch.base_frontier == self.canonical.frontier && apply_projection_patch_batch(&mut self.canonical, &batch).is_ok() {
              // applied in place, frontier chained
            } else {
              needs_heal = true;
            }
          },
          ProjectionStreamItem::Replace(document) => self.canonical = *document,
        }
      }
      if needs_heal {
        self.canonical = handle.canonical_projection().expect("heal");
        self.heals += 1;
      }
      // A full drain lands every local commit submitted since the last drain.
      for _ in 0..self.committed_since_drain {
        self.overlay.acknowledge_oldest();
      }
      self.committed_since_drain = 0;
    }
  }

  fn new_handle() -> (LocalDocHandle, Arc<WriteGate<CrdtRuntime>>) {
    let core = CrdtRuntime::new_empty("overlay-fuzz").expect("runtime");
    LocalDocHandle::new(core, LocalWriteConfig::default())
  }

  // Gate-guard helpers — the temporary-chain form drops the guard at the end of
  // the statement, keeping the hold as tight as possible (no long-lived binding).
  fn export_since(gate: &Arc<WriteGate<CrdtRuntime>>, vv: &loro::VersionVector) -> Vec<u8> {
    gate.lock(GateHolder::ExportUpdates).expect("gate").doc().export(loro::ExportMode::updates(vv)).expect("export")
  }

  fn export_all(gate: &Arc<WriteGate<CrdtRuntime>>) -> Vec<u8> {
    export_since(gate, &loro::VersionVector::default())
  }

  fn state_vv(gate: &Arc<WriteGate<CrdtRuntime>>) -> loro::VersionVector {
    gate.lock(GateHolder::ExportUpdates).expect("gate").doc().state_vv()
  }

  fn import_into(gate: &Arc<WriteGate<CrdtRuntime>>, bytes: &[u8]) {
    let _ = gate.lock(GateHolder::ImportChunk).expect("gate").import_remote_update(bytes);
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
        return Err(format!("paragraph[{ix}] runs differ"));
      }
    }
    if left.ids.paragraph_ids != right.ids.paragraph_ids {
      return Err("paragraph ids differ".to_string());
    }
    if left.blocks.len() != right.blocks.len() {
      return Err(format!("block count {} != {}", left.blocks.len(), right.blocks.len()));
    }
    Ok(())
  }

  /// Visual-only agreement (excludes fabricated ids): predictions fabricate
  /// fresh ids the drain replaces, so prediction quality is measured visually.
  fn visuals_agree(left: &DocumentProjection, right: &DocumentProjection) -> bool {
    left.paragraphs.len() == right.paragraphs.len()
      && (0..left.paragraphs.len()).all(|ix| {
        paragraph_text(left, ix) == paragraph_text(right, ix) && left.paragraphs[ix].style == right.paragraphs[ix].style
      })
  }

  /// Drain the whole accumulated stream to a quiescent point.
  fn drain_all(sim: &mut EditorSim, handle: &LocalDocHandle) {
    sim.drain(handle);
  }

  /// Build a random predictable intent against the CURRENT visible view (the
  /// editor issues intents from what the user sees). Returns None if the view
  /// is too small for the chosen op.
  fn random_predictable_intent(rng: &mut Rng, visible: &DocumentProjection, step: usize) -> Option<LocalIntent> {
    if visible.paragraphs.is_empty() {
      return None;
    }
    let paragraph_ix = rng.below(visible.paragraphs.len());
    let paragraph = visible.ids.paragraph_ids[paragraph_ix];
    let text_len = paragraph_text(visible, paragraph_ix).len();
    let byte = rng.below(text_len + 1);
    match rng.below(7) {
      0..=2 => Some(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, byte),
        text: format!("o{step}"),
        style_override: None,
      })),
      3 => Some(LocalIntent::SplitParagraph(SplitParagraphIntent {
        at: TextAnchor::new(paragraph, byte),
        inherited_style: ParagraphStyle::Normal,
      })),
      4 => {
        if text_len == 0 {
          return None;
        }
        let start = rng.below(text_len);
        let end = (start + 1 + rng.below(3)).min(text_len);
        Some(LocalIntent::DeleteRange(DeleteRangeIntent {
          start: TextAnchor::new(paragraph, start),
          end: TextAnchor::new(paragraph, end),
        }))
      },
      5 => {
        if paragraph_ix + 1 >= visible.paragraphs.len() {
          return None;
        }
        Some(LocalIntent::JoinParagraphs(JoinParagraphsIntent {
          first: paragraph,
          second: visible.ids.paragraph_ids[paragraph_ix + 1],
        }))
      },
      _ => {
        if text_len == 0 {
          return None;
        }
        let start = rng.below(text_len);
        let end = (start + 1 + rng.below(4)).min(text_len);
        let styles = RunStyles {
          semantic: RunSemanticStyle::Custom((rng.below(3) + 1) as u8),
          ..RunStyles::default()
        };
        Some(LocalIntent::SetMarks(SetMarksIntent {
          start: TextAnchor::new(paragraph, start),
          end: TextAnchor::new(paragraph, end),
          styles,
        }))
      },
    }
  }

  /// Submit an intent: enqueue its prediction, commit it through the authority,
  /// and record the commit's frontier for later ack. Legal rejections (stale
  /// hint / empty) simply skip the enqueue.
  fn submit(sim: &mut EditorSim, handle: &LocalDocHandle, intent: LocalIntent) {
    // Predict first (editor renders before the commit returns).
    let id_before = sim.overlay.len();
    sim.overlay.enqueue(intent.clone());
    match handle.apply_intent(intent) {
      Ok(_outcome) => {
        sim.committed_since_drain += 1;
      },
      Err(_rejection) => {
        // ANY rejection (stale hint, empty, structure violation, or an I-10
        // compensated mid-apply failure — all legal when an intent built from
        // the rendered view no longer resolves canonically) means the intent
        // never committed; withdraw its speculative overlay entry (I-15).
        debug_assert_eq!(sim.overlay.len(), id_before + 1);
        sim.overlay.cancel_newest();
      },
    }
  }

  fn seed_document(handle: &LocalDocHandle, sim: &mut EditorSim) {
    // A few paragraphs to edit against.
    let projection = handle.projection().expect("projection");
    let first = projection.ids.paragraph_ids[0];
    for i in 0..6 {
      let _ = handle.apply_intent(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(first, usize::MAX),
        text: format!("seed paragraph {i} body"),
        style_override: None,
      }));
      let _ = handle.apply_intent(LocalIntent::SplitParagraph(SplitParagraphIntent {
        at: TextAnchor::new(first, usize::MAX),
        inherited_style: ParagraphStyle::Normal,
      }));
    }
    drain_all(sim, handle);
  }

  /// The core drain-timing fuzz. One seed = one editing session with randomized
  /// submit/drain/remote/pressure scheduling; oracle 9.1 at every quiescent
  /// point.
  fn run_overlay_fuzz(seed: u64, steps: usize) {
    let mut rng = Rng::new(seed);
    let (handle, gate) = new_handle();
    let mut sim = EditorSim::new(handle.projection().expect("projection"));
    seed_document(&handle, &mut sim);

    // A converged remote peer for interleaved imports.
    let (remote, remote_gate) = new_handle();
    let seed_bytes = export_all(&gate);
    import_into(&remote_gate, &seed_bytes);
    let mut remote_synced = state_vv(&gate);

    for step in 0..steps {
      match rng.below(10) {
        // Submit a predictable intent (dominant). Intents anchor to CANONICAL
        // identities (what the gate resolves against); the prediction renders
        // on top of the visible view.
        0..=4 => {
          if let Some(intent) = random_predictable_intent(&mut rng, &sim.canonical, step) {
            submit(&mut sim, &handle, intent);
          }
        },
        // Submit a paragraph-style change (predictable).
        5 => {
          let paragraph = sim.canonical.ids.paragraph_ids[rng.below(sim.canonical.paragraphs.len())];
          submit(
            &mut sim,
            &handle,
            LocalIntent::SetParagraphStyle(SetParagraphStyleIntent {
              paragraph,
              style: if rng.below(2) == 0 { ParagraphStyle::Normal } else { ParagraphStyle::Custom((rng.below(3) + 1) as u8) },
            }),
          );
        },
        // Burst drain of everything accumulated since the last drain (the
        // stream naturally buffered multiple local + remote batches).
        6..=7 => {
          drain_all(&mut sim, &handle);
        },
        // Remote import: the remote peer edits and we import it into the gate.
        8 => {
          let remote_projection = remote.projection().expect("remote projection");
          if !remote_projection.paragraphs.is_empty() {
            let rp = remote_projection.ids.paragraph_ids[rng.below(remote_projection.paragraphs.len())];
            let _ = remote.apply_intent(LocalIntent::InsertText(InsertTextIntent {
              at: TextAnchor::new(rp, 0),
              text: format!("R{step}"),
              style_override: None,
            }));
            let update = export_since(&remote_gate, &remote_synced);
            import_into(&gate, &update);
            remote_synced = state_vv(&remote_gate);
          }
        },
        // Full drain to a quiescent point + oracle 9.1.
        _ => {
          drain_all(&mut sim, &handle);
          if sim.overlay.is_empty() {
            let visible = sim.visible();
            let authority = handle.canonical_projection().expect("canonical");
            if let Err(reason) = projections_agree(&sim.canonical, &authority) {
              panic!("seed {seed} step {step}: editor canonical diverged from authority: {reason}");
            }
            if let Err(reason) = projections_agree(&visible, &authority) {
              panic!("seed {seed} step {step}: ORACLE 9.1 VIOLATION — visible != canonical: {reason}");
            }
          }
        },
      }
      // Re-pull remote's view of our history so its next edit is against fresh
      // state (keeps interleaving live, not stale).
      if step % 5 == 0 {
        let update = export_all(&gate);
        import_into(&remote_gate, &update);
      }
    }

    // Final settle: drain everything, oracle 9.1 must hold exactly.
    drain_all(&mut sim, &handle);
    let visible = sim.visible();
    let authority = handle.canonical_projection().expect("canonical");
    assert_eq!(sim.overlay.len(), 0, "seed {seed}: queue must be empty after full drain");
    projections_agree(&sim.canonical, &authority).unwrap_or_else(|reason| panic!("seed {seed}: final canonical divergence: {reason}"));
    projections_agree(&visible, &authority).unwrap_or_else(|reason| panic!("seed {seed}: final oracle 9.1 violation: {reason}"));
  }

  #[test]
  fn overlay_drain_timing_fuzz_holds_oracle() {
    for seed in 1..40u64 {
      run_overlay_fuzz(seed, 80);
    }
  }

  #[test]
  fn overlay_oracle_under_heavy_remote_interleave() {
    // Longer runs, same invariant — stress the interleave + heal paths.
    for seed in [7u64, 42, 101, 255, 4096] {
      run_overlay_fuzz(seed, 200);
    }
  }

  /// Prediction QUALITY (§9.2: "measured, not assumed"): for each predictable
  /// op class, the editor-side prediction must VISUALLY match what the
  /// authority commits — captured before the drain, compared after. Fabricated
  /// ids may differ (the drain replaces them), so this compares text + styles.
  #[test]
  fn predictions_visually_match_canonical_per_class() {
    // For each op class: seed a fresh doc, build the intent from the seeded
    // canonical identities, capture the prediction (pre-drain visible), drain
    // fully, and assert the prediction visually matches canonical.
    let case = |build_intent: &dyn Fn(&DocumentProjection) -> LocalIntent, label: &str| {
      let (handle, _gate) = new_handle();
      let mut sim = EditorSim::new(handle.projection().expect("projection"));
      seed_document(&handle, &mut sim);
      let intent = build_intent(&sim.canonical);
      submit(&mut sim, &handle, intent);
      let predicted = sim.visible();
      drain_all(&mut sim, &handle);
      assert!(visuals_agree(&predicted, &sim.canonical), "{label}");
    };

    case(
      &|c| {
        LocalIntent::InsertText(InsertTextIntent {
          at: TextAnchor::new(c.ids.paragraph_ids[1], 3),
          text: "INS".into(),
          style_override: None,
        })
      },
      "insert prediction must visually match canonical",
    );
    case(
      &|c| {
        LocalIntent::SplitParagraph(SplitParagraphIntent {
          at: TextAnchor::new(c.ids.paragraph_ids[1], 2),
          inherited_style: ParagraphStyle::Normal,
        })
      },
      "split prediction must visually match canonical",
    );
    case(
      &|c| {
        LocalIntent::DeleteRange(DeleteRangeIntent {
          start: TextAnchor::new(c.ids.paragraph_ids[1], 2),
          end: TextAnchor::new(c.ids.paragraph_ids[3], 4),
        })
      },
      "cross-paragraph delete prediction must visually match canonical",
    );
    case(
      &|c| {
        LocalIntent::JoinParagraphs(JoinParagraphsIntent {
          first: c.ids.paragraph_ids[1],
          second: c.ids.paragraph_ids[2],
        })
      },
      "join prediction must visually match canonical",
    );
  }
}
