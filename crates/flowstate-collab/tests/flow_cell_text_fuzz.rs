//! Same-cell concurrent rich-text fuzz (build-order step 5) — the two headline
//! defects asserted dead:
//! 1. concurrent char-level edits INSIDE one cell interleave (no
//!    last-writer-wins blob clobber);
//! 2. a reorder can NEVER lose a concurrent keystroke (order-list writes never
//!    touch text containers).
//!
//! Peers drive the REAL write path: `FlowCellAuthority` → `FlowIntent::CellText`
//! → gated commit, exactly what an attached `RichTextEditor` does.

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::flow::{FlowCellAuthority, FlowDocHandle, FlowRuntime};
  use flowstate_collab::local_write::WriteGate;
  use flowstate_flow::format::{CellId, FlowFormat, SheetId};
  use flowstate_flow::intents::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent};
  use gpui_flowtext::local_intents::{
    DeleteRangeIntent, InsertTextIntent, JoinParagraphsIntent, LocalIntent, LocalWriteAuthority as _, SetMarksIntent, SplitParagraphIntent,
    TextAnchor, WriteRejected,
  };
  use gpui_flowtext::{ParagraphStyle, RunStyles};
  use uuid::Uuid;

  struct Rng(u64);

  impl Rng {
    fn new(seed: u64) -> Self {
      Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
      self.0 ^= self.0 << 13;
      self.0 ^= self.0 >> 7;
      self.0 ^= self.0 << 17;
      self.0
    }

    fn below(&mut self, bound: usize) -> usize {
      (self.next() % bound.max(1) as u64) as usize
    }
  }

  struct Peer {
    handle: Arc<FlowDocHandle>,
    authority: Arc<FlowCellAuthority>,
    #[allow(dead_code, reason = "keeps the gate alive for the handle's lifetime")]
    gate: Arc<WriteGate<FlowRuntime>>,
  }

  impl Peer {
    fn oplog_vv(&self) -> loro::VersionVector {
      self
        .handle
        .with_test_runtime(|runtime| runtime.oplog_version_vector())
    }

    fn cell_text(&self, cell: CellId) -> String {
      self.handle.with_test_runtime(|runtime| {
        let rows = flowstate_flow::loro_projection::materialize_cell_rows(runtime.doc(), cell).expect("cell rows");
        rows
          .blocks
          .iter()
          .filter_map(|block| match block {
            gpui_flowtext::InputBlock::Paragraph(paragraph) => Some(
              paragraph
                .runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            ),
            _ => None,
          })
          .collect::<Vec<_>>()
          .join("\n")
      })
    }
  }

  fn sync_all(peers: &[Peer]) {
    for _ in 0..2 {
      for a in 0..peers.len() {
        for b in 0..peers.len() {
          if a == b {
            continue;
          }
          let vv = peers[b].oplog_vv();
          if let Ok(bytes) = peers[a]
            .handle
            .with_test_runtime(|runtime| runtime.export_updates_for(&vv))
            && !bytes.is_empty()
          {
            peers[b]
              .handle
              .with_test_runtime(|runtime| runtime.import_remote_updates(&[bytes.as_slice()]))
              .expect("import");
          }
        }
      }
    }
  }

  fn spawn(peers_n: usize) -> (Vec<Peer>, SheetId, CellId) {
    let format = FlowFormat::policy_debate();
    let (seed_handle, _seed_gate) = FlowDocHandle::new(FlowRuntime::new(&format).expect("seed runtime"));
    let sheet = Uuid::new_v4();
    let cell = Uuid::new_v4();
    seed_handle
      .apply(FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "Text fuzz".into(),
        sheet_type_id: format.sheet_types[0].id,
      })
      .expect("sheet");
    for column_index in 0..2 {
      // A second cell so reorders have something to move against.
      seed_handle
        .apply(FlowIntent::AddCell {
          sheet_id: sheet,
          cell_id: if column_index == 0 { cell } else { Uuid::new_v4() },
          placement: CellPlacement::ColumnEnd { column_index: 0 },
          seed: CellSeed::Empty,
        })
        .expect("cell");
    }
    let snapshot = seed_handle.with_test_runtime(|runtime| runtime.snapshot().expect("snapshot"));
    let peers = (0..peers_n)
      .map(|_| {
        let (handle, gate) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).expect("peer"));
        let authority = handle.cell_authority(cell);
        let _ = handle.open_cell(cell).expect("open cell");
        Peer { handle, authority, gate }
      })
      .collect();
    (peers, sheet, cell)
  }

  /// One random cell-text intent through the peer's authority. Expected
  /// rejections (anchors moved by concurrent edits) are fine.
  fn random_text_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.authority.canonical_projection()?;
    let paragraph_count = projection.paragraphs.len();
    let pick_anchor = |rng: &mut Rng| {
      let ix = rng.below(paragraph_count);
      let id = projection.ids.paragraph_ids[ix];
      let len = flowstate_document::paragraph_text_len(&projection.paragraphs[ix]);
      TextAnchor::new(id, rng.below(len + 1))
    };
    let intent = match rng.below(6) {
      0 | 1 => LocalIntent::InsertText(InsertTextIntent {
        at: pick_anchor(rng),
        text: format!("w{step} "),
        style_override: None,
      }),
      2 => LocalIntent::DeleteRange(DeleteRangeIntent {
        start: pick_anchor(rng),
        end: pick_anchor(rng),
      }),
      3 => LocalIntent::SplitParagraph(SplitParagraphIntent {
        at: pick_anchor(rng),
        inherited_style: ParagraphStyle::Normal,
      }),
      4 => {
        if paragraph_count < 2 {
          return Ok(());
        }
        let first_ix = rng.below(paragraph_count - 1);
        LocalIntent::JoinParagraphs(JoinParagraphsIntent {
          first: projection.ids.paragraph_ids[first_ix],
          second: projection.ids.paragraph_ids[first_ix + 1],
        })
      },
      _ => LocalIntent::SetMarks(SetMarksIntent {
        start: pick_anchor(rng),
        end: pick_anchor(rng),
        styles: RunStyles {
          strikethrough: rng.below(2) == 0,
          direct_underline: rng.below(2) == 0,
          ..RunStyles::default()
        },
      }),
    };
    peer.authority.apply(intent).map(|_| ())
  }

  fn run_fuzz(seed: u64, peers_n: usize, rounds: usize, ops_per_round: usize) {
    let mut rng = Rng::new(seed);
    let (peers, sheet, cell) = spawn(peers_n);

    for round in 0..rounds {
      for op in 0..ops_per_round {
        let peer_ix = rng.below(peers_n);
        let step = round * ops_per_round + op;
        // Interleave the occasional structural reorder with the typing storm —
        // the reorder-vs-edit headline arm.
        if rng.below(8) == 0 {
          let board = peers[peer_ix].handle.board_projection().expect("board");
          let cells: Vec<CellId> = board
            .sheet(sheet)
            .expect("sheet")
            .cells
            .iter()
            .map(|cell| cell.id)
            .collect();
          if cells.len() > 1 {
            let target = cells[rng.below(cells.len())];
            if target != cell {
              let _ = peers[peer_ix].handle.apply(FlowIntent::MoveCellSubtree {
                sheet_id: sheet,
                cell_id: cell,
                drop: if rng.below(2) == 0 {
                  FlowDropIntent::BeforeSibling(target)
                } else {
                  FlowDropIntent::AfterSibling(target)
                },
              });
            }
          }
        }
        match random_text_intent(&mut rng, &peers[peer_ix], step) {
          Ok(()) | Err(WriteRejected::EmptyIntent | WriteRejected::StructureViolation(_) | WriteRejected::UnresolvedParagraph(_)) => {},
          Err(other) => panic!("seed {seed} round {round} op {op}: unexpected rejection {other}"),
        }
      }
      sync_all(&peers);

      let reference_vv = peers[0].oplog_vv();
      let reference_text = peers[0].cell_text(cell);
      let reference_board = peers[0].handle.board_projection().expect("board");
      for (ix, peer) in peers.iter().enumerate().skip(1) {
        assert_eq!(
          peer.oplog_vv(),
          reference_vv,
          "seed {seed} round {round}: peer {ix} version vector differs after quiescent sync"
        );
        assert_eq!(
          peer.cell_text(cell),
          reference_text,
          "seed {seed} round {round}: peer {ix} cell text diverged"
        );
        assert_eq!(
          peer.handle.board_projection().expect("board"),
          reference_board,
          "seed {seed} round {round}: peer {ix} board diverged"
        );
      }
    }
  }

  /// THE headline pair, deterministic form: peer A types while peer B reorders
  /// the same cell; both edits survive the merge exactly.
  #[test]
  fn reorder_never_loses_a_concurrent_keystroke() {
    let (peers, sheet, cell) = spawn(2);
    let projection = peers[0]
      .authority
      .canonical_projection()
      .expect("projection");
    let paragraph = projection.ids.paragraph_ids[0];

    peers[0]
      .authority
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "surviving keystrokes".into(),
        style_override: None,
      }))
      .expect("typing");
    let board = peers[1].handle.board_projection().expect("board");
    let other = board
      .sheet(sheet)
      .expect("sheet")
      .cells
      .iter()
      .map(|candidate| candidate.id)
      .find(|id| *id != cell)
      .expect("other cell");
    peers[1]
      .handle
      .apply(FlowIntent::MoveCellSubtree {
        sheet_id: sheet,
        cell_id: cell,
        drop: FlowDropIntent::AfterSibling(other),
      })
      .expect("reorder");

    sync_all(&peers);
    for peer in &peers {
      assert_eq!(peer.cell_text(cell), "surviving keystrokes");
      let board = peer.handle.board_projection().expect("board");
      let order: Vec<CellId> = board
        .sheet(sheet)
        .expect("sheet")
        .cells
        .iter()
        .map(|cell| cell.id)
        .collect();
      assert_eq!(order, vec![other, cell], "the reorder survived alongside the keystrokes");
    }
  }

  #[test]
  fn concurrent_same_cell_edits_interleave() {
    let (peers, _sheet, cell) = spawn(2);
    let projection = peers[0]
      .authority
      .canonical_projection()
      .expect("projection");
    let paragraph = projection.ids.paragraph_ids[0];
    peers[0]
      .authority
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "left".into(),
        style_override: None,
      }))
      .expect("peer 0 types");
    peers[1]
      .authority
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "right".into(),
        style_override: None,
      }))
      .expect("peer 1 types");
    sync_all(&peers);
    let merged = peers[0].cell_text(cell);
    assert_eq!(merged, peers[1].cell_text(cell));
    assert!(
      merged.contains("left") && merged.contains("right"),
      "both peers' characters survive char-level merge: {merged:?}"
    );
  }

  #[test]
  fn two_peer_cell_text_fuzz_converges() {
    for seed in [1, 7, 42, 505, 1337, 8738] {
      run_fuzz(seed, 2, 6, 12);
    }
  }

  #[test]
  fn three_peer_cell_text_fuzz_converges() {
    for seed in [3, 99, 4242, 20260707] {
      run_fuzz(seed, 3, 5, 10);
    }
  }

  /// Long-horizon soak (10k+ text intents across the sweep), ignored by default.
  #[test]
  #[ignore = "long-horizon soak — run via `heaven.sh soak`"]
  fn flow_cell_text_soak() {
    let seeds: u64 = std::env::var("FUZZ_SOAK_SEEDS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(500);
    let rounds: usize = std::env::var("FUZZ_SOAK_ROUNDS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(8);
    let ops: usize = std::env::var("FUZZ_SOAK_OPS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(16);
    for seed in 1..=seeds {
      let peers = 2 + (seed % 2) as usize;
      run_fuzz(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), peers, rounds, ops);
      if seed % 25 == 0 {
        eprintln!("[soak] flow_cell_text {seed}/{seeds}");
      }
    }
  }
}
